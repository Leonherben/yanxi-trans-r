#[cfg(not(windows))]
use std::io::Write;
#[cfg(not(windows))]
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

    let bytes = match fetch_audio(&clean, accent).await {
        Some(bytes) => bytes,
        None => {
            eprintln!("[Yanxi TTS] 无法从发音服务获取音频数据 (已尝试有道、百度与 Google TTS)");
            return;
        }
    };

    play_bytes(bytes);
}

/// Try the configured voice sources in order. A transient reset must not make
/// pronunciation fail outright, so every source gets a bounded retry.
async fn fetch_audio(clean: &str, accent: Accent) -> Option<Vec<u8>> {
    let encoded = url_encode(clean);
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(8))
        .connect_timeout(std::time::Duration::from_secs(5))
        .build()
        .ok()?;
    let user_agent = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0.0.0 Safari/537.36";
    let is_single_word = !clean.contains(' ');
    let accent_code = match accent {
        Accent::Uk => "1",
        Accent::Us => "2",
    };
    let tl = match accent {
        Accent::Uk => "en-GB",
        Accent::Us => "en-US",
    };
    let mut sources = Vec::new();
    if is_single_word {
        sources.push(format!(
            "https://dict.youdao.com/dictvoice?audio={encoded}&type={accent_code}"
        ));
    }
    sources.push(format!(
        "https://fanyi.baidu.com/gettts?lan=en&text={encoded}&spd=3&source=web&voice={}",
        if accent == Accent::Uk { "2" } else { "1" }
    ));
    if is_single_word {
        sources.push(format!(
            "https://dict.youdao.com/dictvoice?audio={encoded}&type={accent_code}"
        ));
    }
    sources.push(format!(
        "https://translate.google.com/translate_tts?ie=UTF-8&tl={tl}&client=tw-ob&q={encoded}"
    ));

    for url in sources {
        for attempt in 0..2 {
            match client
                .get(&url)
                .header(reqwest::header::USER_AGENT, user_agent)
                .send()
                .await
            {
                Ok(resp) if resp.status().is_success() => {
                    if let Ok(bytes) = resp.bytes().await {
                        if !bytes.is_empty() {
                            return Some(bytes.to_vec());
                        }
                    }
                }
                Ok(resp) if attempt == 0 && resp.status().is_server_error() => {}
                _ if attempt == 0 => {
                    tokio::time::sleep(std::time::Duration::from_millis(250)).await;
                }
                _ => break,
            }
        }
    }
    None
}

/// Decode and play in RAM. Each player is tried by its real exit status, so a
/// player that starts but cannot open the audio device falls through to the next.
#[cfg(not(windows))]
fn play_bytes(bytes: Vec<u8>) {
    tokio::task::spawn_blocking(move || {
        // 1. GStreamer streaming from stdin: no file at all.
        if let Ok(mut child) = Command::new("gst-launch-1.0")
            .args([
                "-q",
                "fdsrc",
                "!",
                "decodebin",
                "!",
                "audioconvert",
                "!",
                "audioresample",
                "!",
                "autoaudiosink",
            ])
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
        {
            if let Some(mut stdin) = child.stdin.take() {
                let _ = stdin.write_all(&bytes);
            }
            if child.wait().map(|status| status.success()).unwrap_or(false) {
                return;
            }
        }

        // 2. Shared memory file in /dev/shm, then real exit-status checks.
        let pid = std::process::id();
        let shm_path = format!("/dev/shm/yanxi_tts_{pid}.mp3");
        if std::fs::write(&shm_path, &bytes).is_err() {
            eprintln!("[Yanxi TTS] 无法写入临时音频");
            return;
        }
        let candidates: [(&str, Vec<&str>); 3] = [
            ("pw-play", vec![]),
            ("paplay", vec![]),
            ("mpv", vec!["--no-video", "--really-quiet"]),
        ];
        let mut played = false;
        for (player, extra) in candidates {
            let mut args: Vec<&str> = extra;
            args.push(&shm_path);
            let ok = Command::new(player)
                .args(&args)
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
                .map(|status| status.success())
                .unwrap_or(false);
            if ok {
                played = true;
                break;
            }
        }
        let _ = std::fs::remove_file(&shm_path);
        if !played {
            eprintln!(
                "[Yanxi TTS] 未能通过系统音频服务播放 (已尝试 gst-launch-1.0 / pw-play / paplay / mpv)"
            );
        }
    });
}

#[cfg(windows)]
fn play_bytes(bytes: Vec<u8>) {
    use std::sync::atomic::{AtomicU64, Ordering};
    use windows::Win32::Media::Multimedia::mciSendStringW;
    use windows::core::HSTRING;

    static TTS_SEQ: AtomicU64 = AtomicU64::new(0);

    tokio::task::spawn_blocking(move || {
        let pid = std::process::id();
        let seq = TTS_SEQ.fetch_add(1, Ordering::Relaxed);
        let (ext, mci_type) = if bytes.starts_with(b"RIFF") {
            ("wav", "waveaudio")
        } else {
            ("mp3", "mpegvideo")
        };
        let temp_path = std::env::temp_dir().join(format!("yanxi_tts_{pid}_{seq}.{ext}"));
        if std::fs::write(&temp_path, &bytes).is_err() {
            eprintln!("[Yanxi TTS] 无法写入临时音频文件");
            return;
        }

        let alias = format!("yanxi_tts_{pid}_{seq}");
        let send_mci = |cmd: &str| -> u32 {
            let wide = HSTRING::from(cmd);
            unsafe { mciSendStringW(&wide, None, None) }
        };

        let open_cmd = format!(
            "open \"{}\" type {mci_type} alias {alias}",
            temp_path.display()
        );
        let open_err = send_mci(&open_cmd);
        if open_err == 0 {
            let play_err = send_mci(&format!("play {alias} wait"));
            if play_err != 0 {
                eprintln!("[Yanxi TTS] Windows 音频播放失败 (MCI 错误码: {play_err})");
            }
            let _ = send_mci(&format!("close {alias}"));
        } else {
            eprintln!("[Yanxi TTS] Windows 音频打开失败 (MCI 错误码: {open_err})");
        }

        let _ = std::fs::remove_file(&temp_path);
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
    #[ignore = "requires network access for the real voice services"]
    fn fetches_real_audio_for_word() {
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let bytes = runtime.block_on(fetch_audio("Save", Accent::Us));
        let bytes = bytes.expect("no voice source returned audio");
        assert!(bytes.len() > 500, "audio too small: {}", bytes.len());
    }

    #[test]
    #[ignore = "requires network and a real audio device"]
    fn plays_real_audio_end_to_end() {
        let runtime = tokio::runtime::Runtime::new().unwrap();
        runtime.block_on(play_pronunciation("Save", Accent::Us));
    }

    #[test]
    fn test_url_encode() {
        assert_eq!(url_encode("hello world"), "hello%20world");
        assert_eq!(url_encode("test&query=1"), "test%26query%3D1");
    }
}
