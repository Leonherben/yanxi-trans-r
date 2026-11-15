use eframe::egui::{Context, FontData, FontDefinitions, FontFamily, FontId};
use std::path::{Path, PathBuf};
use std::sync::Arc;

pub fn configure_cjk_fonts(ctx: &Context) {
    let mut fonts = FontDefinitions::default();
    if let Some((name, data)) = load_best_cjk_font() {
        println!("[Yanxi GUI] 成功加载中文字体: {name}");
        fonts.font_data.insert(name.clone(), data);
        fonts
            .families
            .entry(FontFamily::Proportional)
            .or_default()
            .insert(0, name.clone());
        fonts
            .families
            .entry(FontFamily::Monospace)
            .or_default()
            .push(name);
        ctx.set_fonts(fonts);
    } else {
        eprintln!("[Yanxi GUI] 未找到包含中文字符的字体，请安装系统中文字体后重试");
    }
}

// Check the candidate itself, without egui's bundled fallback fonts. A fontconfig
// match can be a Latin font when the desktop language is English.
fn supports_chinese(data: Arc<FontData>) -> bool {
    let mut definitions = FontDefinitions::empty();
    definitions.font_data.insert("candidate".into(), data);
    definitions
        .families
        .insert(FontFamily::Proportional, vec!["candidate".into()]);
    let mut fonts = eframe::egui::epaint::text::Fonts::new(Default::default(), definitions);
    fonts.has_glyphs(&FontId::proportional(14.0), "中文翻译设置")
}

fn load_best_cjk_font() -> Option<(String, Arc<FontData>)> {
    let mut candidates: Vec<PathBuf> = Vec::new();
    #[cfg(target_os = "linux")]
    if let Ok(output) = std::process::Command::new("fc-match")
        .args(["-s", "-f", "%{file}\n", ":lang=zh-cn"])
        .output()
    {
        if output.status.success() {
            candidates.extend(
                String::from_utf8_lossy(&output.stdout)
                    .lines()
                    .map(PathBuf::from),
            );
        }
    }
    if let Some(home) = dirs::home_dir() {
        candidates
            .push(home.join(".local/share/fonts/HarmonyOS_Sans/HarmonyOS_Sans_SC_Regular.ttf"));
        candidates.push(home.join(".local/share/fonts/PingFangSC/PingFangSC-Regular.ttf"));
        candidates.push(home.join(".fonts/HarmonyOS_Sans_SC_Regular.ttf"));
    }
    candidates.extend(
        [
            "/usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc",
            "/usr/share/fonts/noto-cjk/NotoSansCJK-Regular.ttc",
            "/usr/share/fonts/truetype/wqy/wqy-microhei.ttc",
            "/usr/share/fonts/truetype/wqy/wqy-zenhei.ttc",
            "/usr/share/fonts/truetype/arphic/uming.ttc",
            "/usr/share/fonts/truetype/droid/DroidSansFallbackFull.ttf",
            "C:\\Windows\\Fonts\\msyh.ttc",
            "C:\\Windows\\Fonts\\msyh.ttf",
            "C:\\Windows\\Fonts\\simsun.ttc",
            "C:\\Windows\\Fonts\\simhei.ttf",
            "/System/Library/Fonts/PingFang.ttc",
            "/Library/Fonts/Arial Unicode.ttf",
        ]
        .into_iter()
        .map(PathBuf::from),
    );

    let mut checked = std::collections::HashSet::new();
    for path in candidates {
        if !checked.insert(path.clone()) {
            continue;
        }
        if let Ok(bytes) = std::fs::read(&path) {
            let data = Arc::new(FontData::from_owned(bytes));
            if supports_chinese(Arc::clone(&data)) {
                let name = Path::new(&path).file_name()?.to_string_lossy().into_owned();
                return Some((name, data));
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_latin_only_font_instead_of_accepting_fontconfig_match() {
        // egui's bundled fonts lack CJK glyphs, so matching a valid Latin font
        // must not stop us from trying the system Chinese candidates.
        let defaults = FontDefinitions::default();
        let latin = defaults.font_data.values().next().unwrap();
        assert!(!supports_chinese(Arc::clone(latin)));
    }
}
