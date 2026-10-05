use eframe::egui::{Context, FontData, FontDefinitions, FontFamily};
use std::path::{Path, PathBuf};
use std::sync::Arc;

pub fn configure_cjk_fonts(ctx: &Context) {
    let mut fonts = FontDefinitions::default();

    if let Some((name, font_bytes)) = load_best_cjk_font() {
        println!("[Yanxi GUI] 🔤 成功加载中文字体: {}", name);
        fonts.font_data.insert(
            name.clone(),
            Arc::new(FontData::from_owned(font_bytes)),
        );

        // 将中文字体插入到比例字体 (Proportional) 的最前端
        if let Some(prop) = fonts.families.get_mut(&FontFamily::Proportional) {
            prop.insert(0, name.clone());
        }

        // 将中文字体插入到等宽字体 (Monospace) 的后备列表
        if let Some(mono) = fonts.families.get_mut(&FontFamily::Monospace) {
            mono.push(name);
        }

        ctx.set_fonts(fonts);
    } else {
        eprintln!("[Yanxi GUI] ⚠️ 未找到系统中文字体，可能会显示乱码方框");
    }
}

fn load_best_cjk_font() -> Option<(String, Vec<u8>)> {
    // 1. 尝试通过 fc-match (Linux fontconfig) 查找系统推荐中文字体
    #[cfg(target_os = "linux")]
    {
        if let Ok(output) = std::process::Command::new("fc-match")
            .args(["-f", "%{file}", ":lang=zh"])
            .output()
        {
            if output.status.success() {
                let path_str = String::from_utf8_lossy(&output.stdout).trim().to_string();
                if !path_str.is_empty() {
                    let path = Path::new(&path_str);
                    if path.exists() {
                        if let Ok(bytes) = std::fs::read(path) {
                            let font_name = path
                                .file_name()
                                .and_then(|n| n.to_str())
                                .unwrap_or("cjk-system-font")
                                .to_string();
                            return Some((font_name, bytes));
                        }
                    }
                }
            }
        }
    }

    // 2. 候选字体文件路径列表
    let mut candidate_paths: Vec<PathBuf> = Vec::new();

    // Linux 常见路径
    if let Some(home) = dirs::home_dir() {
        candidate_paths.push(home.join(".local/share/fonts/HarmonyOS_Sans/HarmonyOS_Sans_SC_Regular.ttf"));
        candidate_paths.push(home.join(".local/share/fonts/PingFangSC/PingFangSC-Regular.ttf"));
        candidate_paths.push(home.join(".fonts/HarmonyOS_Sans_SC_Regular.ttf"));
    }
    candidate_paths.push(PathBuf::from("/usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc"));
    candidate_paths.push(PathBuf::from("/usr/share/fonts/noto-cjk/NotoSansCJK-Regular.ttc"));
    candidate_paths.push(PathBuf::from("/usr/share/fonts/truetype/wqy/wqy-microhei.ttc"));
    candidate_paths.push(PathBuf::from("/usr/share/fonts/truetype/wqy/wqy-zenhei.ttc"));
    candidate_paths.push(PathBuf::from("/usr/share/fonts/truetype/arphic/uming.ttc"));
    candidate_paths.push(PathBuf::from("/usr/share/fonts/truetype/droid/DroidSansFallbackFull.ttf"));

    // Windows 常见路径
    candidate_paths.push(PathBuf::from("C:\\Windows\\Fonts\\msyh.ttc"));
    candidate_paths.push(PathBuf::from("C:\\Windows\\Fonts\\msyh.ttf"));
    candidate_paths.push(PathBuf::from("C:\\Windows\\Fonts\\simsun.ttc"));
    candidate_paths.push(PathBuf::from("C:\\Windows\\Fonts\\simhei.ttf"));

    // macOS 常见路径
    candidate_paths.push(PathBuf::from("/System/Library/Fonts/PingFang.ttc"));
    candidate_paths.push(PathBuf::from("/Library/Fonts/Arial Unicode.ttf"));

    for path in candidate_paths {
        if path.exists() {
            if let Ok(bytes) = std::fs::read(&path) {
                let font_name = path
                    .file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or("cjk-fallback-font")
                    .to_string();
                return Some((font_name, bytes));
            }
        }
    }

    None
}
