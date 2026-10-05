#[cfg(target_os = "linux")]
use x11rb::connection::Connection;
#[cfg(target_os = "linux")]
use x11rb::protocol::xproto::{ConnectionExt, GrabMode, ModMask};

pub fn start_global_hotkey_listener<F>(on_toggle: F)
where
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

                // 查找字母 'd' 和 'q' 的键码 (Keysym: 'd' = 0x64, 'D' = 0x44, 'q' = 0x71, 'Q' = 0x51)
                let Ok(cookie) = conn.get_keyboard_mapping(min_kc, count) else {
                    return;
                };
                let Ok(mapping) = cookie.reply() else {
                    return;
                };

                let keysyms_per_keycode = mapping.keysyms_per_keycode as usize;
                let mut target_keycodes = Vec::new();

                for (idx, chunk) in mapping.keysyms.chunks(keysyms_per_keycode).enumerate() {
                    let kc = min_kc + idx as u8;
                    for &sym in chunk {
                        if sym == 0x0064 || sym == 0x0044 || sym == 0x0071 || sym == 0x0051 {
                            if !target_keycodes.contains(&kc) {
                                target_keycodes.push(kc);
                            }
                        }
                    }
                }

                // 捕获 Alt (Mod1) + Keycode，同时兼顾 NumLock (Mod2) 与 CapsLock (Lock) 组合
                let masks = [
                    ModMask::M1,
                    ModMask::M1 | ModMask::M2,
                    ModMask::M1 | ModMask::LOCK,
                    ModMask::M1 | ModMask::M2 | ModMask::LOCK,
                ];

                for &kc in &target_keycodes {
                    for &mask in &masks {
                        let _ = conn.grab_key(
                            false,
                            root,
                            mask,
                            kc,
                            GrabMode::ASYNC,
                            GrabMode::ASYNC,
                        );
                    }
                }
                let _ = conn.flush();

                println!("[Yanxi Hotkey] 🎯 全局热键监听就绪: Alt + D / Alt + Q (唤醒/收起翻译浮窗)");

                loop {
                    match conn.wait_for_event() {
                        Ok(x11rb::protocol::Event::KeyPress(_)) => {
                            on_toggle();
                        }
                        Ok(_) => {}
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
