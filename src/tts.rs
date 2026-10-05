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
    let accent_code = match accent {
        Accent::Uk => "1",
        Accent::Us => "2",
    };
    let url = format!("https://dict.youdao.com/dictvoice?audio={encoded}&type={accent_code}");

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(6))
        .build()
        .unwrap_or_default();

    let resp = match client
        .get(&url)
        .header(
            reqwest::header::USER_AGENT,
            "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0.0.0 Safari/537.36",
        )
        .send()
        .await
    {
        Ok(r) => r,
        Err(e) => {
            eprintln!("[Yanxi TTS] 发音网络请求异常: {e}");
            return;
        }
    };

    if !resp.status().is_success() {
        eprintln!("[Yanxi TTS] 发音接口返回非成功状态: {}", resp.status());
        return;
    }

    let bytes = match resp.bytes().await {
        Ok(b) => b,
        Err(e) => {
            eprintln!("[Yanxi TTS] 读取音频字节流失败: {e}");
            return;
        }
    };

    if bytes.len() <= 500 {
        eprintln!("[Yanxi TTS] 音频数据长度不足 (可能为空或错误提示)");
        return;
    }

    // 在阻塞线程池中直接管道喂给播放器，全程零磁盘写入
    tokio::task::spawn_blocking(move || {
        let child = Command::new("pw-play")
            .arg("-")
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .or_else(|_| {
                Command::new("paplay")
                    .stdin(Stdio::piped())
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .spawn()
            })
            .or_else(|_| {
                Command::new("mpv")
                    .arg("-")
                    .stdin(Stdio::piped())
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .spawn()
            });

        match child {
            Ok(mut proc) => {
                if let Some(mut stdin) = proc.stdin.take() {
                    let _ = stdin.write_all(&bytes);
                    drop(stdin);
                }
                let _ = proc.wait();
            }
            Err(e) => {
                eprintln!("[Yanxi TTS] 未找到系统音频播放器 (pw-play / paplay / mpv): {e}");
            }
        }
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

