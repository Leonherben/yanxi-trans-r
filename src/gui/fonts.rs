use eframe::egui::{Context, FontData, FontDefinitions, FontFamily};
use skrifa::{GlyphId, MetadataProvider as _};
use std::path::{Path, PathBuf};
use std::sync::Arc;

pub fn configure_cjk_fonts(ctx: &Context) {
    let mut fonts = FontDefinitions::default();
    if let Some((name, data)) = load_best_cjk_font() {
        println!("[Yanxi] 成功加载中文字体: {name}");
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
        eprintln!("[Yanxi] 未找到包含中文字符的字体，请安装系统中文字体后重试");
    }
}

// Inspect the candidate font's charmap directly instead of single-font
// epaint::Fonts::has_glyphs, which breaks when the font also provides '?' or '◻'.
fn supports_chinese(data: Arc<FontData>) -> bool {
    let Ok(font) = skrifa::FontRef::from_index(data.font.as_ref(), data.index) else {
        return false;
    };
    let charmap = font.charmap();
    "中文翻译设置"
        .chars()
        .all(|c| charmap.map(c).is_some_and(|id| id != GlyphId::NOTDEF))
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

    fn build_test_sfnt_with_chars(chars: &[char]) -> Arc<FontData> {
        let mut sorted: Vec<u32> = chars.iter().map(|&c| c as u32).collect();
        sorted.sort_unstable();
        sorted.dedup();

        let mut cmap = Vec::new();
        cmap.extend_from_slice(&0u16.to_be_bytes()); // version
        cmap.extend_from_slice(&1u16.to_be_bytes()); // numTables
        cmap.extend_from_slice(&3u16.to_be_bytes()); // platformID: Windows
        cmap.extend_from_slice(&10u16.to_be_bytes()); // encodingID: Unicode full
        cmap.extend_from_slice(&12u32.to_be_bytes()); // subtable offset

        let subtable_len = 16u32 + (sorted.len() as u32) * 12;
        cmap.extend_from_slice(&12u16.to_be_bytes()); // format 12
        cmap.extend_from_slice(&0u16.to_be_bytes()); // reserved
        cmap.extend_from_slice(&subtable_len.to_be_bytes());
        cmap.extend_from_slice(&0u32.to_be_bytes()); // language
        cmap.extend_from_slice(&(sorted.len() as u32).to_be_bytes()); // numGroups
        for (idx, cp) in sorted.iter().enumerate() {
            cmap.extend_from_slice(&cp.to_be_bytes());
            cmap.extend_from_slice(&cp.to_be_bytes());
            cmap.extend_from_slice(&((idx as u32) + 1).to_be_bytes());
        }

        let mut head = vec![0u8; 54];
        head[0..4].copy_from_slice(&0x0001_0000u32.to_be_bytes());
        head[12..16].copy_from_slice(&0x5F0F_3CF5u32.to_be_bytes());
        head[18..20].copy_from_slice(&1000u16.to_be_bytes());

        let mut maxp = Vec::with_capacity(6);
        maxp.extend_from_slice(&0x0000_5000u32.to_be_bytes());
        maxp.extend_from_slice(&((sorted.len() as u16) + 1).to_be_bytes());

        let tables: [(&[u8; 4], &[u8]); 3] = [(b"cmap", &cmap), (b"head", &head), (b"maxp", &maxp)];
        let header_size = 12 + tables.len() * 16;
        let mut out = vec![0u8; header_size];
        out[0..4].copy_from_slice(&0x0001_0000u32.to_be_bytes());
        out[4..6].copy_from_slice(&(tables.len() as u16).to_be_bytes());

        for (i, (tag, data)) in tables.iter().enumerate() {
            let offset = out.len() as u32;
            let len = data.len() as u32;
            let rec = 12 + i * 16;
            out[rec..rec + 4].copy_from_slice(*tag);
            out[rec + 8..rec + 12].copy_from_slice(&offset.to_be_bytes());
            out[rec + 12..rec + 16].copy_from_slice(&len.to_be_bytes());
            out.extend_from_slice(data);
            while out.len() % 4 != 0 {
                out.push(0);
            }
        }

        Arc::new(FontData::from_owned(out))
    }

    #[test]
    fn rejects_latin_only_font_instead_of_accepting_fontconfig_match() {
        // egui's bundled fonts lack CJK glyphs, so matching a valid Latin font
        // must not stop us from trying the system Chinese candidates.
        let defaults = FontDefinitions::default();
        let latin = defaults.font_data.values().next().unwrap();
        assert!(!supports_chinese(Arc::clone(latin)));
    }

    #[test]
    fn accepts_cjk_font_even_when_it_also_defines_replacement_glyphs() {
        let font = build_test_sfnt_with_chars(&['?', '◻', '中', '文', '翻', '译', '设', '置']);
        assert!(supports_chinese(font));
    }

    #[test]
    fn rejects_font_missing_some_required_cjk_glyphs() {
        let partial = build_test_sfnt_with_chars(&['?', '◻', '中', '文']);
        assert!(!supports_chinese(partial));
    }
}
