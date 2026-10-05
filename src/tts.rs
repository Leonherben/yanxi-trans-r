use std::io::Write;
use std::process::{Command, Stdio};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Accent {
    Uk,
    Us,
}

pub fn has_english_text(text: &str) -> bool {
    text.chars().any(|c| c.is_ascii_alphabetic())
}

fn url_encode(input: &str) -> String {
    let mut encoded = String::new();
    for byte in input.bytes() {
        match byte {
            b'a'..=b'z' | b'A'..=b'Z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                encoded.push(byte as char);
            }
            b' ' => encoded.push_str("%20"),
            _ => {
                encoded.push_str(&format!("%{:02X}", byte));
            }
        }
    }
    encoded
}

/// 播放指定文本的真人发音 (零磁盘缓存，内存直接管道流式喂入播放器)
pub async fn play_pronunciation(text: &str, accent: Accent) {
    let clean = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if clean.is_empty() {
        return;
    }

    let encoded = url_encode(&clean);
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(6))
        .build()
        .unwrap_or_default();

    // 优先尝试有道真人词典发音；若为多词短语/句子或接口异常，自动切换 Google TTS 高质量发音
    let is_single_word = !clean.contains(' ');
    let mut audio_bytes = None;

    if is_single_word {
        let accent_code = match accent {
            Accent::Uk => "1",
            Accent::Us => "2",
        };
        let youdao_url = format!("https://dict.youdao.com/dictvoice?audio={encoded}&type={accent_code}");
        if let Ok(resp) = client
            .get(&youdao_url)
            .header(
                reqwest::header::USER_AGENT,
                "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0.0.0 Safari/537.36",
            )
            .send()
            .await
        {
            if resp.status().is_success() {
                if let Ok(b) = resp.bytes().await {
                    if b.len() > 500 {
                        audio_bytes = Some(b);
                    }
                }
            }
        }
    }

    if audio_bytes.is_none() {
        let tl = match accent {
            Accent::Uk => "en-GB",
            Accent::Us => "en-US",
        };
        let google_url = format!("https://translate.google.com/translate_tts?ie=UTF-8&tl={tl}&client=tw-ob&q={encoded}");
        if let Ok(resp) = client
            .get(&google_url)
            .header(
                reqwest::header::USER_AGENT,
                "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0.0.0 Safari/537.36",
            )
            .send()
            .await
        {
            if resp.status().is_success() {
                if let Ok(b) = resp.bytes().await {
                    if b.len() > 300 {
                        audio_bytes = Some(b);
                    }
                }
            }
        }
    }

    let bytes = match audio_bytes {
        Some(b) => b,
        None => {
            eprintln!("[Yanxi TTS] 无法从发音服务获取音频数据 (已尝试有道词典与 Google TTS)");
            return;
        }
    };

    // 在阻塞线程池中解码播放：优先流式管道，降级 POSIX 共享内存 RAM 播放，全程零物理磁盘缓存
    tokio::task::spawn_blocking(move || {
        // 1. 优先尝试 GStreamer 纯管道流式解码播放 (零文件，最优雅)
        let gst_child = Command::new("gst-launch-1.0")
            .args(["-q", "fdsrc", "!", "decodebin", "!", "audioconvert", "!", "audioresample", "!", "autoaudiosink"])
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn();

        if let Ok(mut proc) = gst_child {
            if let Some(mut stdin) = proc.stdin.take() {
                let _ = stdin.write_all(&bytes);
                drop(stdin);
            }
            if let Ok(status) = proc.wait() {
                if status.success() {
                    return;
                }
            }
        }

        // 2. 降级尝试 POSIX 共享内存 /dev/shm (纯内核 RAM 虚拟盘，零物理磁盘 I/O 与磨损)
        let pid = std::process::id();
        let shm_path = format!("/dev/shm/yanxi_tts_{}.mp3", pid);
        if std::fs::write(&shm_path, &bytes).is_ok() {
            let res = Command::new("pw-play")
                .arg(&shm_path)
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
                .or_else(|_| {
                    Command::new("paplay")
                        .arg(&shm_path)
                        .stdout(Stdio::null())
                        .stderr(Stdio::null())
                        .status()
                })
                .or_else(|_| {
                    Command::new("mpv")
                        .args([&shm_path, "--no-video", "--really-quiet"])
                        .stdout(Stdio::null())
                        .stderr(Stdio::null())
                        .status()
                });

            let _ = std::fs::remove_file(&shm_path);
            if res.is_ok() {
                return;
            }
        }

        eprintln!("[Yanxi TTS] 未能通过系统音频服务播放 (已尝试 gst-launch-1.0 / pw-play / paplay / mpv)");
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_has_english_text() {
        assert!(has_english_text("hello"));
        assert!(has_english_text("Hello, 世界!"));
        assert!(!has_english_text("你好，世界！12345"));
        assert!(!has_english_text("   "));
    }

    #[test]
    fn test_url_encode() {
        assert_eq!(url_encode("hello world"), "hello%20world");
        assert_eq!(url_encode("test&query=1"), "test%26query%3D1");
    }
}

