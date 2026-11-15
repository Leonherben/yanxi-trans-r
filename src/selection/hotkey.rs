use std::sync::Arc;
use std::sync::mpsc::Receiver;
use std::time::Duration;

#[cfg(target_os = "linux")]
use x11rb::connection::Connection;
#[cfg(target_os = "linux")]
use x11rb::protocol::xproto::{ConnectionExt, GrabMode, ModMask};

/// Strict parser shared by validation and registration. One non-modifier key is required.
fn parse_parts(s: &str) -> Result<(u16, Vec<u32>), String> {
    let mut mask = 0u16;
    let mut keysyms = None;
    for part in s.split('+') {
        let part = part.trim();
        let clean = if part.starts_with('<') && part.ends_with('>') {
            &part[1..part.len() - 1]
        } else {
            part
        }
        .to_ascii_lowercase();
        let modifier = match clean.as_str() {
            "alt" | "mod1" | "opt" | "option" => Some(1 << 3),
            "ctrl" | "control" => Some(1 << 2),
            "shift" => Some(1),
            "super" | "win" | "cmd" | "meta" | "mod4" => Some(1 << 6),
            _ => None,
        };
        if let Some(bit) = modifier {
            if mask & bit != 0 {
                return Err("快捷键中修饰键重复，请检查输入".into());
            }
            mask |= bit;
            continue;
        }
        let syms = match clean.as_str() {
            "space" => vec![0x20],
            "tab" => vec![0xff09],
            "enter" | "return" => vec![0xff0d],
            "escape" | "esc" => vec![0xff1b],
            "backspace" => vec![0xff08],
            c if c.len() == 1
                && c.as_bytes()[0].is_ascii_graphic()
                && !matches!(c.as_bytes()[0], b'<' | b'>') =>
            {
                let ch = c.as_bytes()[0];
                if ch.is_ascii_alphabetic() {
                    vec![ch as u32, ch.to_ascii_uppercase() as u32]
                } else {
                    vec![ch as u32]
                }
            }
            f if f.starts_with('f') => {
                let n = f[1..]
                    .parse::<u32>()
                    .ok()
                    .filter(|n| (1..=12).contains(n))
                    .filter(|n| f == format!("f{n}"));
                match n {
                    Some(n) => vec![0xffbe + n - 1],
                    None => return Err(format!("快捷键按键无效：{part}")),
                }
            }
            _ => return Err(format!("快捷键按键无效：{part}")),
        };
        if keysyms.replace(syms).is_some() {
            return Err("快捷键只能包含一个普通按键，例如 alt+q".into());
        }
    }
    if mask == 0 {
        return Err("请至少使用一个修饰键，例如 alt+q".into());
    }
    let keysyms = keysyms.ok_or_else(|| "快捷键缺少按键，例如 alt+q".to_string())?;
    #[cfg(target_os = "linux")]
    if mask & ((1 << 2) | (1 << 3)) == ((1 << 2) | (1 << 3))
        && keysyms.iter().any(|key| (0xffbe..=0xffc9).contains(key))
    {
        return Err("此组合用于切换系统终端，请改用字母键，例如 alt+q".into());
    }
    Ok((mask, keysyms))
}

pub fn validate_hotkey(s: &str) -> Result<(), String> {
    parse_parts(s).map(|_| ())
}

/// Preserve the public parser signature; invalid input produces no binding.
#[cfg(target_os = "linux")]
pub fn parse_hotkey_str(s: &str) -> (ModMask, Vec<u32>) {
    let (mask, syms) = parse_parts(s).unwrap_or_default();
    (ModMask::from(mask), syms)
}

#[cfg(target_os = "linux")]
type Bindings = Vec<(u8, ModMask)>;

#[cfg(target_os = "linux")]
fn desired_bindings(
    mapping: &x11rb::protocol::xproto::GetKeyboardMappingReply,
    min_kc: u8,
    hotkey: &str,
) -> Result<Bindings, String> {
    let (mask, syms) = parse_parts(hotkey)?;
    let width = mapping.keysyms_per_keycode as usize;
    if width == 0 {
        return Err("读取键盘映射失败".into());
    }
    let mut result = Vec::new();
    for (idx, chunk) in mapping.keysyms.chunks(width).enumerate() {
        if chunk.iter().any(|sym| syms.contains(sym)) {
            let kc = min_kc.checked_add(idx as u8).ok_or("键盘映射无效")?;
            for locks in [
                0,
                u16::from(ModMask::M2),
                u16::from(ModMask::LOCK),
                u16::from(ModMask::M2 | ModMask::LOCK),
            ] {
                result.push((kc, ModMask::from(mask | locks)));
            }
        }
    }
    if result.is_empty() {
        return Err("当前键盘布局中没有找到此快捷键的按键".into());
    }
    Ok(result)
}

/// Install only new grabs, keeping the old combination live until all new grabs succeed.
#[cfg(target_os = "linux")]
fn replace_bindings<C: Connection>(
    conn: &C,
    root: u32,
    old: &Bindings,
    wanted: &Bindings,
) -> Result<(), String> {
    let mut added = Vec::new();
    for &(kc, mask) in wanted {
        if old.contains(&(kc, mask)) {
            continue;
        }
        let result = conn
            .grab_key(false, root, mask, kc, GrabMode::ASYNC, GrabMode::ASYNC)
            .map_err(|e| e.to_string())
            .and_then(|cookie| cookie.check().map_err(|e| e.to_string()));
        if let Err(e) = result {
            for &(new_kc, new_mask) in &added {
                let _ = conn
                    .ungrab_key(new_kc, root, new_mask)
                    .map(|cookie| cookie.check());
            }
            let _ = conn.flush();
            return Err(format!(
                "快捷键注册失败，可能已被其他程序占用：{e}；原快捷键已保留"
            ));
        }
        added.push((kc, mask));
    }
    for &(kc, mask) in old {
        if !wanted.contains(&(kc, mask)) {
            conn.ungrab_key(kc, root, mask)
                .map_err(|e| e.to_string())?
                .check()
                .map_err(|e| e.to_string())?;
        }
    }
    conn.flush().map_err(|e| e.to_string())
}

pub fn start_global_hotkey_listener<F, S>(
    initial_hotkey: String,
    hotkey_rx: Receiver<String>,
    on_toggle: F,
    on_status: S,
) where
    F: Fn() + Send + Sync + 'static,
    S: Fn(bool, String) + Send + Sync + 'static,
{
    let on_status = Arc::new(on_status);
    #[cfg(target_os = "linux")]
    {
        let report = Arc::clone(&on_status);
        let spawned = std::thread::Builder::new()
            .name("yanxi-x11-hotkey".into())
            .spawn(move || {
                let run = || -> Result<(), String> {
                    let (conn, screen_num) =
                        x11rb::connect(None).map_err(|e| format!("连接 X11 热键服务失败：{e}"))?;
                    let root = conn.setup().roots[screen_num].root;
                    let min_kc = conn.setup().min_keycode;
                    let count = conn.setup().max_keycode.saturating_sub(min_kc) + 1;
                    let mapping = conn
                        .get_keyboard_mapping(min_kc, count)
                        .map_err(|e| format!("读取键盘映射失败：{e}"))?
                        .reply()
                        .map_err(|e| format!("读取键盘映射失败：{e}"))?;
                    let mut active = Bindings::new();
                    let mut current = String::new();
                    let mut pending = Some(initial_hotkey);
                    loop {
                        if let Some(key) = pending.take() {
                            let key = key.trim().to_ascii_lowercase();
                            let result =
                                desired_bindings(&mapping, min_kc, &key).and_then(|wanted| {
                                    replace_bindings(&conn, root, &active, &wanted)?;
                                    active = wanted;
                                    Ok(())
                                });
                            match result {
                                Ok(()) => {
                                    current = key;
                                    report(true, format!("快捷键已就绪：{current}"));
                                }
                                Err(e) => report(
                                    false,
                                    if current.is_empty() {
                                        e
                                    } else {
                                        format!("{e}（当前仍为 {current}）")
                                    },
                                ),
                            }
                        }
                        match hotkey_rx.try_recv() {
                            Ok(key) => {
                                pending = Some(key);
                                continue;
                            }
                            Err(std::sync::mpsc::TryRecvError::Disconnected) => break,
                            Err(std::sync::mpsc::TryRecvError::Empty) => {}
                        }
                        match conn
                            .poll_for_event()
                            .map_err(|e| format!("X11 热键监听异常：{e}"))?
                        {
                            Some(x11rb::protocol::Event::KeyPress(event)) => {
                                // Button-state bits are unrelated; all keyboard modifiers must match.
                                let state = u16::from(event.state) & 0xff;
                                if active.iter().any(|&(kc, mask)| {
                                    kc == event.detail && u16::from(mask) == state
                                }) {
                                    on_toggle();
                                }
                            }
                            Some(_) => {}
                            None => std::thread::sleep(Duration::from_millis(20)),
                        }
                    }
                    for (kc, mask) in active {
                        let _ = conn.ungrab_key(kc, root, mask);
                    }
                    let _ = conn.flush();
                    Ok(())
                };
                if let Err(e) = run() {
                    report(false, e);
                }
            });
        if let Err(e) = spawned {
            on_status(false, format!("启动热键监听线程失败：{e}"));
        }
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = (initial_hotkey, hotkey_rx, on_toggle);
        on_status(false, "当前平台暂未实现全局快捷键监听".into());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn invalid_hotkeys_do_not_fall_back() {
        for bad in [
            "",
            "alt",
            "q",
            "alt+",
            "ctrl++q",
            "alt+bogus",
            "alt+f13",
            "alt+q+w",
            "alt+alt+q",
            "alt+中文",
            "<alt+q",
            "alt+f01",
        ] {
            assert!(validate_hotkey(bad).is_err(), "accepted: {bad}");
        }
        for good in ["alt+q", "ctrl+alt+d", "super+f1", "<alt>+q", "Shift+F12"] {
            assert!(validate_hotkey(good).is_ok(), "rejected: {good}");
        }
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn rejects_virtual_terminal_switch_combinations() {
        for key in 1..=12 {
            assert!(validate_hotkey(&format!("ctrl+alt+f{key}")).is_err());
        }
        assert!(validate_hotkey("ctrl+alt+y").is_ok());
        assert!(validate_hotkey("alt+f2").is_ok());
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn test_parse_hotkey_str() {
        let (mask, syms) = parse_hotkey_str("alt+q");
        assert_eq!(mask, ModMask::M1);
        assert_eq!(syms, vec![0x71, 0x51]);
        assert_eq!(
            parse_hotkey_str("ctrl+alt+d"),
            (ModMask::CONTROL | ModMask::M1, vec![0x64, 0x44])
        );
        assert_eq!(parse_hotkey_str("super+f1"), (ModMask::M4, vec![0xffbe]));
        assert_eq!(parse_hotkey_str("<alt>+q"), (mask, syms));
        assert!(parse_hotkey_str("invalid").1.is_empty());
    }
}
