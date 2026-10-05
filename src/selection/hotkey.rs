use std::sync::mpsc::Receiver;
use std::time::Duration;

#[cfg(target_os = "linux")]
use x11rb::connection::Connection;
#[cfg(target_os = "linux")]
use x11rb::protocol::xproto::{ConnectionExt, GrabMode, ModMask};

/// 解析快捷键字符串（如 "alt+q", "ctrl+alt+t", "super+f1"）为 X11 ModMask 与 Keysyms
#[cfg(target_os = "linux")]
pub fn parse_hotkey_str(s: &str) -> (ModMask, Vec<u32>) {
    let mut mask = ModMask::from(0u16);
    let mut keysyms = Vec::new();

    let parts: Vec<&str> = s.split('+').map(|p| p.trim()).collect();
    for part in parts {
        let clean = part.trim_matches(|c| c == '<' || c == '>').trim().to_lowercase();
        match clean.as_str() {
            "alt" | "mod1" | "opt" | "option" => mask |= ModMask::M1,
            "ctrl" | "control" => mask |= ModMask::CONTROL,
            "shift" => mask |= ModMask::SHIFT,
            "super" | "win" | "cmd" | "meta" | "mod4" => mask |= ModMask::M4,
            single if single.len() == 1 => {
                let ch = single.chars().next().unwrap();
                if ch.is_ascii_alphabetic() {
                    let c_low = ch.to_ascii_lowercase() as u32;
                    let c_up = ch.to_ascii_uppercase() as u32;
                    keysyms.push(c_low);
                    keysyms.push(c_up);
                } else if ch.is_ascii_digit() {
                    keysyms.push(ch as u32);
                } else {
                    keysyms.push(ch as u32);
                }
            }
            "space" => keysyms.push(0x0020),
            "tab" => keysyms.push(0xff09),
            "enter" | "return" => keysyms.push(0xff0d),
            "escape" | "esc" => keysyms.push(0xff1b),
            "backspace" => keysyms.push(0xff08),
            f_key if f_key.starts_with('f') && f_key.len() <= 3 => {
                if let Ok(num) = f_key[1..].parse::<u32>() {
                    if (1..=12).contains(&num) {
                        keysyms.push(0xffbe + (num - 1));
                    }
                }
            }
            _ => {}
        }
    }

    if u16::from(mask) == 0 {
        mask = ModMask::M1;
    }

    if keysyms.is_empty() {
        keysyms = vec![0x0071, 0x0051];
    }

    (mask, keysyms)
}

#[cfg(target_os = "linux")]
fn apply_grab_keys<C: ConnectionExt>(
    conn: &C,
    root: u32,
    mapping: &x11rb::protocol::xproto::GetKeyboardMappingReply,
    min_kc: u8,
    keysyms_per_kc: usize,
    hotkey_str: &str,
    grabbed_kc: &mut Vec<u8>,
    grabbed_mk: &mut Vec<ModMask>,
) {
    let (base_mask, target_keysyms) = parse_hotkey_str(hotkey_str);
    let mut target_kcs = Vec::new();

    for (idx, chunk) in mapping.keysyms.chunks(keysyms_per_kc).enumerate() {
        let kc = min_kc + idx as u8;
        for &sym in chunk {
            if target_keysyms.contains(&sym) {
                if !target_kcs.contains(&kc) {
                    target_kcs.push(kc);
                }
            }
        }
    }

    let masks = [
        base_mask,
        base_mask | ModMask::M2,
        base_mask | ModMask::LOCK,
        base_mask | ModMask::M2 | ModMask::LOCK,
    ];

    for &kc in &target_kcs {
        for &m in &masks {
            let _ = conn.grab_key(
                false,
                root,
                m,
                kc,
                GrabMode::ASYNC,
                GrabMode::ASYNC,
            );
            if !grabbed_kc.contains(&kc) {
                grabbed_kc.push(kc);
            }
            if !grabbed_mk.contains(&m) {
                grabbed_mk.push(m);
            }
        }
    }
}

pub fn start_global_hotkey_listener<F>(
    initial_hotkey: String,
    hotkey_rx: Receiver<String>,
    on_toggle: F,
) where
    F: Fn() + Send + Sync + 'static,
{
    #[cfg(target_os = "linux")]
    {
        std::thread::Builder::new()
            .name("yanxi-x11-hotkey".into())
            .spawn(move || {
                let Ok((conn, screen_num)) = x11rb::connect(None) else {
                    eprintln!("[Yanxi Hotkey] 无法连接到 X11 显示服务进行全局热键监听");
                    return;
                };

                let root = conn.setup().roots[screen_num].root;
                let min_kc = conn.setup().min_keycode;
                let max_kc = conn.setup().max_keycode;
                let count = max_kc.saturating_sub(min_kc) + 1;

                let Ok(cookie) = conn.get_keyboard_mapping(min_kc, count) else {
                    return;
                };
                let Ok(mapping) = cookie.reply() else {
                    return;
                };

                let keysyms_per_kc = mapping.keysyms_per_keycode as usize;

                let mut current_hotkey = if initial_hotkey.trim().is_empty() {
                    "alt+q".to_string()
                } else {
                    initial_hotkey
                };

                let mut grabbed_keycodes: Vec<u8> = Vec::new();
                let mut grabbed_masks: Vec<ModMask> = Vec::new();

                apply_grab_keys(&conn, root, &mapping, min_kc, keysyms_per_kc, &current_hotkey, &mut grabbed_keycodes, &mut grabbed_masks);
                let _ = conn.flush();
                println!("[Yanxi Hotkey] 🎯 全局热键监听就绪: \"{}\" (唤醒/收起翻译浮窗)", current_hotkey);

                loop {
                    let mut updated = false;
                    while let Ok(new_key) = hotkey_rx.try_recv() {
                        let trimmed = new_key.trim().to_lowercase();
                        if !trimmed.is_empty() && trimmed != current_hotkey {
                            for &kc in &grabbed_keycodes {
                                for &m in &grabbed_masks {
                                    let _ = conn.ungrab_key(kc, root, m);
                                }
                            }
                            let _ = conn.flush();
                            grabbed_keycodes.clear();
                            grabbed_masks.clear();

                            current_hotkey = trimmed;
                            updated = true;
                        }
                    }

                    if updated {
                        apply_grab_keys(&conn, root, &mapping, min_kc, keysyms_per_kc, &current_hotkey, &mut grabbed_keycodes, &mut grabbed_masks);
                        let _ = conn.flush();
                        println!("[Yanxi Hotkey] 🔄 全局热键已热更新绑定为: \"{}\"", current_hotkey);
                    }

                    match conn.poll_for_event() {
                        Ok(Some(x11rb::protocol::Event::KeyPress(_))) => {
                            on_toggle();
                        }
                        Ok(Some(_)) => {}
                        Ok(None) => {
                            std::thread::sleep(Duration::from_millis(25));
                        }
                        Err(e) => {
                            eprintln!("[Yanxi Hotkey] X11 热键循环异常: {e}");
                            break;
                        }
                    }
                }
            })
            .expect("Failed to spawn X11 hotkey listener thread");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_hotkey_str() {
        let (mask, syms) = parse_hotkey_str("alt+q");
        assert_eq!(u16::from(mask), u16::from(ModMask::M1));
        assert!(syms.contains(&0x0071)); // 'q'
        assert!(syms.contains(&0x0051)); // 'Q'

        let (mask2, syms2) = parse_hotkey_str("ctrl+alt+d");
        assert_eq!(u16::from(mask2), u16::from(ModMask::CONTROL | ModMask::M1));
        assert!(syms2.contains(&0x0064)); // 'd'
        assert!(syms2.contains(&0x0044)); // 'D'

        let (mask3, syms3) = parse_hotkey_str("super+f1");
        assert_eq!(u16::from(mask3), u16::from(ModMask::M4));
        assert_eq!(syms3, vec![0xffbe]); // F1

        let (mask_pynput, syms_pynput) = parse_hotkey_str("<alt>+q");
        assert_eq!(mask_pynput, mask);
        assert_eq!(syms_pynput, syms);
    }
}

