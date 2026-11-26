use std::sync::Arc;
use std::sync::mpsc::Receiver;
#[cfg(target_os = "linux")]
use std::time::Duration;

#[cfg(target_os = "linux")]
use x11rb::connection::Connection;
#[cfg(target_os = "linux")]
use x11rb::protocol::xproto::{ConnectionExt, GrabMode, ModMask};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum HotkeyKey {
    Space,
    Tab,
    Enter,
    Escape,
    Backspace,
    Char(u8),
    Fn(u8),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ParsedHotkey {
    alt: bool,
    ctrl: bool,
    shift: bool,
    win: bool,
    key: HotkeyKey,
}

/// Strict parser shared by validation and registration. One non-modifier key is required.
fn parse_parts(s: &str) -> Result<ParsedHotkey, String> {
    let mut alt = false;
    let mut ctrl = false;
    let mut shift = false;
    let mut win = false;
    let mut key = None;
    for part in s.split('+') {
        let part = part.trim();
        let clean = if part.starts_with('<') && part.ends_with('>') {
            &part[1..part.len() - 1]
        } else {
            part
        }
        .to_ascii_lowercase();
        let modifier_slot = match clean.as_str() {
            "alt" | "mod1" | "opt" | "option" => Some(&mut alt),
            "ctrl" | "control" => Some(&mut ctrl),
            "shift" => Some(&mut shift),
            "super" | "win" | "cmd" | "meta" | "mod4" => Some(&mut win),
            _ => None,
        };
        if let Some(slot) = modifier_slot {
            if *slot {
                return Err("快捷键中修饰键重复，请检查输入".into());
            }
            *slot = true;
            continue;
        }
        let parsed_key = match clean.as_str() {
            "space" => HotkeyKey::Space,
            "tab" => HotkeyKey::Tab,
            "enter" | "return" => HotkeyKey::Enter,
            "escape" | "esc" => HotkeyKey::Escape,
            "backspace" => HotkeyKey::Backspace,
            c if c.len() == 1
                && c.as_bytes()[0].is_ascii_graphic()
                && !matches!(c.as_bytes()[0], b'<' | b'>') =>
            {
                HotkeyKey::Char(c.as_bytes()[0])
            }
            f if f.starts_with('f') => {
                let n = f[1..]
                    .parse::<u8>()
                    .ok()
                    .filter(|n| (1..=12).contains(n))
                    .filter(|n| f == format!("f{n}"));
                match n {
                    Some(n) => HotkeyKey::Fn(n),
                    None => return Err(format!("快捷键按键无效：{part}")),
                }
            }
            _ => return Err(format!("快捷键按键无效：{part}")),
        };
        if key.replace(parsed_key).is_some() {
            return Err("快捷键只能包含一个普通按键，例如 alt+q".into());
        }
    }
    if !alt && !ctrl && !shift && !win {
        return Err("请至少使用一个修饰键，例如 alt+q".into());
    }
    let key = key.ok_or_else(|| "快捷键缺少按键，例如 alt+q".to_string())?;
    #[cfg(target_os = "linux")]
    if ctrl && alt && matches!(key, HotkeyKey::Fn(1..=12)) {
        return Err("此组合用于切换系统终端，请改用字母键，例如 alt+q".into());
    }
    Ok(ParsedHotkey {
        alt,
        ctrl,
        shift,
        win,
        key,
    })
}

pub fn validate_hotkey(s: &str) -> Result<(), String> {
    parse_parts(s).map(|_| ())
}

#[cfg(target_os = "linux")]
fn to_x11_parts(parsed: ParsedHotkey) -> (ModMask, Vec<u32>) {
    let mut mask = 0u16;
    if parsed.alt {
        mask |= 1 << 3;
    }
    if parsed.ctrl {
        mask |= 1 << 2;
    }
    if parsed.shift {
        mask |= 1;
    }
    if parsed.win {
        mask |= 1 << 6;
    }
    let syms = match parsed.key {
        HotkeyKey::Space => vec![0x20],
        HotkeyKey::Tab => vec![0xff09],
        HotkeyKey::Enter => vec![0xff0d],
        HotkeyKey::Escape => vec![0xff1b],
        HotkeyKey::Backspace => vec![0xff08],
        HotkeyKey::Char(ch) => {
            if ch.is_ascii_alphabetic() {
                vec![ch as u32, ch.to_ascii_uppercase() as u32]
            } else {
                vec![ch as u32]
            }
        }
        HotkeyKey::Fn(n) => vec![0xffbe + u32::from(n) - 1],
    };
    (ModMask::from(mask), syms)
}

/// Preserve the public parser signature; invalid input produces no binding.
#[cfg(target_os = "linux")]
pub fn parse_hotkey_str(s: &str) -> (ModMask, Vec<u32>) {
    match parse_parts(s) {
        Ok(parsed) => to_x11_parts(parsed),
        Err(_) => (ModMask::from(0u16), Vec::new()),
    }
}

#[cfg(target_os = "linux")]
type Bindings = Vec<(u8, ModMask)>;

#[cfg(target_os = "linux")]
fn desired_bindings(
    mapping: &x11rb::protocol::xproto::GetKeyboardMappingReply,
    min_kc: u8,
    hotkey: &str,
) -> Result<Bindings, String> {
    let (mask, syms) = to_x11_parts(parse_parts(hotkey)?);
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
                result.push((kc, mask | locks));
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
    #[cfg(windows)]
    {
        use std::sync::Mutex;
        use windows::Win32::Foundation::{LPARAM, WPARAM};
        use windows::Win32::System::Threading::GetCurrentThreadId;
        use windows::Win32::UI::Input::KeyboardAndMouse::{
            HOT_KEY_MODIFIERS, MOD_ALT, MOD_CONTROL, MOD_NOREPEAT, MOD_SHIFT, MOD_WIN,
            RegisterHotKey, UnregisterHotKey, VK_BACK, VK_ESCAPE, VK_F1, VK_RETURN, VK_SPACE,
            VK_TAB, VkKeyScanW,
        };
        use windows::Win32::UI::WindowsAndMessaging::{
            GetMessageW, MSG, PM_NOREMOVE, PeekMessageW, PostThreadMessageW, WM_APP, WM_HOTKEY,
            WM_QUIT,
        };

        const WM_APP_RELOAD: u32 = WM_APP + 1;

        fn to_windows_hotkey(hotkey: &str) -> Result<(HOT_KEY_MODIFIERS, u32), String> {
            let parsed = parse_parts(hotkey)?;
            let mut mods = MOD_NOREPEAT;
            if parsed.alt {
                mods |= MOD_ALT;
            }
            if parsed.ctrl {
                mods |= MOD_CONTROL;
            }
            if parsed.shift {
                mods |= MOD_SHIFT;
            }
            if parsed.win {
                mods |= MOD_WIN;
            }
            let vk = match parsed.key {
                HotkeyKey::Space => u32::from(VK_SPACE.0),
                HotkeyKey::Tab => u32::from(VK_TAB.0),
                HotkeyKey::Enter => u32::from(VK_RETURN.0),
                HotkeyKey::Escape => u32::from(VK_ESCAPE.0),
                HotkeyKey::Backspace => u32::from(VK_BACK.0),
                HotkeyKey::Fn(n) => u32::from(VK_F1.0) + u32::from(n) - 1,
                HotkeyKey::Char(ch) if ch.is_ascii_alphabetic() => {
                    u32::from(ch.to_ascii_uppercase())
                }
                HotkeyKey::Char(ch) if ch.is_ascii_digit() => u32::from(ch),
                HotkeyKey::Char(ch) => {
                    let scan = unsafe { VkKeyScanW(u16::from(ch)) };
                    if scan == -1 {
                        return Err(format!("当前键盘布局不支持快捷键按键：{}", ch as char));
                    }
                    u32::from((scan as u16) & 0xff)
                }
            };
            Ok((mods, vk))
        }

        let report = Arc::clone(&on_status);
        let pending_reload = Arc::new(Mutex::new(None::<String>));
        let pending_for_thread = Arc::clone(&pending_reload);
        let (tid_tx, tid_rx) = std::sync::mpsc::sync_channel::<u32>(1);

        let spawned = std::thread::Builder::new()
            .name("yanxi-win-hotkey".into())
            .spawn(move || {
                let mut msg = MSG::default();
                // Force creation of the thread message queue before publishing the thread ID.
                unsafe {
                    let _ = PeekMessageW(&mut msg, None, 0, 0, PM_NOREMOVE);
                }
                let tid = unsafe { GetCurrentThreadId() };
                let _ = tid_tx.send(tid);

                let mut current_id: Option<i32> = None;
                let mut next_id: i32 = 1;
                let mut current_key = String::new();

                let register_key = |raw_key: &str,
                                    current_id: &mut Option<i32>,
                                    next_id: &mut i32,
                                    current_key: &mut String| {
                    let key = raw_key.trim().to_ascii_lowercase();
                    match to_windows_hotkey(&key) {
                        Ok((mods, vk)) => {
                            let candidate_id = *next_id;
                            match unsafe { RegisterHotKey(None, candidate_id, mods, vk) } {
                                Ok(()) => {
                                    if let Some(old_id) = current_id.replace(candidate_id) {
                                        unsafe {
                                            let _ = UnregisterHotKey(None, old_id);
                                        }
                                    }
                                    *next_id = if candidate_id == 1 { 2 } else { 1 };
                                    *current_key = key.clone();
                                    report(true, format!("快捷键已就绪：{key}"));
                                }
                                Err(e) => {
                                    let base = format!(
                                        "快捷键注册失败，可能已被其他程序占用：{e}"
                                    );
                                    let msg = if current_key.is_empty() {
                                        base
                                    } else {
                                        format!("{base}；原快捷键已保留（当前仍为 {current_key}）")
                                    };
                                    report(false, msg);
                                }
                            }
                        }
                        Err(e) => {
                            let msg = if current_key.is_empty() {
                                e
                            } else {
                                format!("{e}（当前仍为 {current_key}）")
                            };
                            report(false, msg);
                        }
                    }
                };

                register_key(&initial_hotkey, &mut current_id, &mut next_id, &mut current_key);

                loop {
                    let ret = unsafe { GetMessageW(&mut msg, None, 0, 0) };
                    if ret.0 <= 0 {
                        break;
                    }
                    if msg.message == WM_HOTKEY {
                        if current_id.is_some_and(|id| msg.wParam.0 == id as usize) {
                            on_toggle();
                        }
                    } else if msg.message == WM_APP_RELOAD {
                        if let Ok(mut guard) = pending_for_thread.lock() {
                            if let Some(new_key) = guard.take() {
                                register_key(
                                    &new_key,
                                    &mut current_id,
                                    &mut next_id,
                                    &mut current_key,
                                );
                            }
                        }
                    }
                }

                if let Some(id) = current_id {
                    unsafe {
                        let _ = UnregisterHotKey(None, id);
                    }
                }
            });

        match spawned {
            Ok(_) => {
                if let Ok(tid) = tid_rx.recv() {
                    let _ = std::thread::Builder::new()
                        .name("yanxi-win-hotkey-bridge".into())
                        .spawn(move || {
                            while let Ok(new_key) = hotkey_rx.recv() {
                                if let Ok(mut guard) = pending_reload.lock() {
                                    *guard = Some(new_key);
                                }
                                unsafe {
                                    let _ = PostThreadMessageW(
                                        tid,
                                        WM_APP_RELOAD,
                                        WPARAM(0),
                                        LPARAM(0),
                                    );
                                }
                            }
                            unsafe {
                                let _ = PostThreadMessageW(tid, WM_QUIT, WPARAM(0), LPARAM(0));
                            }
                        });
                }
            }
            Err(e) => {
                on_status(false, format!("启动热键监听线程失败：{e}"));
            }
        }
    }
    #[cfg(not(any(target_os = "linux", windows)))]
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
