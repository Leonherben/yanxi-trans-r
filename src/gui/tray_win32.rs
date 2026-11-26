use std::sync::{Arc, Mutex};
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, WPARAM};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Shell::{
    NIF_ICON, NIF_MESSAGE, NIF_TIP, NIM_ADD, NIM_DELETE, NIM_MODIFY, NOTIFYICONDATAW,
    Shell_NotifyIconW,
};
use windows::Win32::UI::WindowsAndMessaging::{
    AppendMenuW, CreateIconFromResourceEx, CreatePopupMenu, CreateWindowExW, DefWindowProcW,
    DestroyIcon, DestroyMenu, DestroyWindow, DispatchMessageW, GetCursorPos, GetMessageW,
    GetSystemMetrics, HICON, IMAGE_FLAGS, MF_SEPARATOR, MF_STRING, MSG, PostMessageW,
    PostQuitMessage, RegisterClassExW, RegisterWindowMessageW, SM_CXSMICON, SM_CYSMICON,
    SetForegroundWindow, TPM_BOTTOMALIGN, TPM_LEFTALIGN, TPM_RIGHTBUTTON, TrackPopupMenu,
    TranslateMessage, WINDOW_EX_STYLE, WINDOW_STYLE, WM_APP, WM_CLOSE, WM_COMMAND,
    WM_CONTEXTMENU, WM_DESTROY, WM_LBUTTONUP, WM_NULL, WM_RBUTTONUP, WNDCLASSEXW,
};
use windows::core::PCWSTR;

const WM_TRAY_CALLBACK: u32 = WM_APP + 10;
const ID_TRAY_ICON: u32 = 1;
const ID_MENU_TOGGLE: usize = 1001;
const ID_MENU_SETTINGS: usize = 1002;
const ID_MENU_EXIT: usize = 1003;

struct TrayCallbacks {
    on_toggle: Arc<dyn Fn() + Send + Sync>,
    on_settings: Arc<dyn Fn() + Send + Sync>,
    on_exit: Arc<dyn Fn() + Send + Sync>,
}

static TRAY_CALLBACKS: Mutex<Option<TrayCallbacks>> = Mutex::new(None);
static WM_TASKBAR_CREATED: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
static TRAY_HICON: std::sync::atomic::AtomicIsize = std::sync::atomic::AtomicIsize::new(0);
static TRAY_HWND: std::sync::atomic::AtomicIsize = std::sync::atomic::AtomicIsize::new(0);

pub struct TrayHandle {
    hwnd: isize,
}

impl Drop for TrayHandle {
    fn drop(&mut self) {
        if self.hwnd != 0 {
            unsafe {
                let _ = PostMessageW(
                    Some(HWND(self.hwnd as *mut core::ffi::c_void)),
                    WM_CLOSE,
                    WPARAM(0),
                    LPARAM(0),
                );
            }
        }
        if let Ok(mut guard) = TRAY_CALLBACKS.lock() {
            *guard = None;
        }
    }
}

fn wide_null(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(std::iter::once(0)).collect()
}

fn load_tray_hicon() -> Option<HICON> {
    let ico = include_bytes!("../../assets/icon.ico");
    if ico.len() < 6 {
        return None;
    }
    let count = u16::from_le_bytes([ico[4], ico[5]]) as usize;
    if count == 0 || ico.len() < 6 + count * 16 {
        return None;
    }
    let cx = unsafe { GetSystemMetrics(SM_CXSMICON) }.max(16);
    let cy = unsafe { GetSystemMetrics(SM_CYSMICON) }.max(16);
    let mut best: Option<(i32, usize, usize)> = None;
    for i in 0..count {
        let off = 6 + i * 16;
        let w = if ico[off] == 0 { 256 } else { ico[off] as i32 };
        let size =
            u32::from_le_bytes([ico[off + 8], ico[off + 9], ico[off + 10], ico[off + 11]]) as usize;
        let data_off = u32::from_le_bytes([
            ico[off + 12],
            ico[off + 13],
            ico[off + 14],
            ico[off + 15],
        ]) as usize;
        if size == 0 || data_off + size > ico.len() {
            continue;
        }
        let diff = (w - cx).abs();
        if best.is_none_or(|(best_diff, _, _)| diff < best_diff) {
            best = Some((diff, data_off, data_off + size));
        }
    }
    let (_, start, end) = best?;
    unsafe { CreateIconFromResourceEx(&ico[start..end], true, 0x0003_0000, cx, cy, IMAGE_FLAGS(0)) }
        .ok()
}

fn build_notify_icon_data(hwnd: HWND, hicon: HICON, tip: &str) -> NOTIFYICONDATAW {
    let mut nid = NOTIFYICONDATAW {
        cbSize: std::mem::size_of::<NOTIFYICONDATAW>() as u32,
        hWnd: hwnd,
        uID: ID_TRAY_ICON,
        uFlags: NIF_ICON | NIF_MESSAGE | NIF_TIP,
        uCallbackMessage: WM_TRAY_CALLBACK,
        hIcon: hicon,
        ..Default::default()
    };
    let encoded: Vec<u16> = tip.encode_utf16().collect();
    let max_copy = encoded.len().min(nid.szTip.len().saturating_sub(1));
    nid.szTip[..max_copy].copy_from_slice(&encoded[..max_copy]);
    nid
}

fn add_tray_icon(hwnd: HWND) {
    let raw_icon = TRAY_HICON.load(std::sync::atomic::Ordering::Relaxed);
    let hicon = HICON(raw_icon as *mut core::ffi::c_void);
    let nid = build_notify_icon_data(hwnd, hicon, "言蹊翻译 (Alt+Q)");
    unsafe {
        let _ = Shell_NotifyIconW(NIM_ADD, &nid);
    }
}

pub fn update_tooltip(tip: &str) {
    let raw_hwnd = TRAY_HWND.load(std::sync::atomic::Ordering::Relaxed);
    if raw_hwnd == 0 {
        return;
    }
    let hwnd = HWND(raw_hwnd as *mut core::ffi::c_void);
    let mut nid = build_notify_icon_data(hwnd, HICON::default(), tip);
    nid.uFlags = NIF_TIP;
    unsafe {
        let _ = Shell_NotifyIconW(NIM_MODIFY, &nid);
    }
}

fn remove_tray_icon(hwnd: HWND) {
    let nid = NOTIFYICONDATAW {
        cbSize: std::mem::size_of::<NOTIFYICONDATAW>() as u32,
        hWnd: hwnd,
        uID: ID_TRAY_ICON,
        ..Default::default()
    };
    unsafe {
        let _ = Shell_NotifyIconW(NIM_DELETE, &nid);
    }
}

fn show_context_menu(hwnd: HWND) {
    unsafe {
        let Ok(menu) = CreatePopupMenu() else {
            return;
        };
        let toggle_label = wide_null("显示 / 隐藏悬浮窗");
        let settings_label = wide_null("设置");
        let exit_label = wide_null("退出");
        let _ = AppendMenuW(
            menu,
            MF_STRING,
            ID_MENU_TOGGLE,
            PCWSTR(toggle_label.as_ptr()),
        );
        let _ = AppendMenuW(
            menu,
            MF_STRING,
            ID_MENU_SETTINGS,
            PCWSTR(settings_label.as_ptr()),
        );
        let _ = AppendMenuW(menu, MF_SEPARATOR, 0, PCWSTR::null());
        let _ = AppendMenuW(menu, MF_STRING, ID_MENU_EXIT, PCWSTR(exit_label.as_ptr()));

        let mut pt = POINT::default();
        let _ = GetCursorPos(&mut pt);
        let _ = SetForegroundWindow(hwnd);
        let _ = TrackPopupMenu(
            menu,
            TPM_BOTTOMALIGN | TPM_LEFTALIGN | TPM_RIGHTBUTTON,
            pt.x,
            pt.y,
            None,
            hwnd,
            None,
        );
        let _ = PostMessageW(Some(hwnd), WM_NULL, WPARAM(0), LPARAM(0));
        let _ = DestroyMenu(menu);
    }
}

unsafe extern "system" fn tray_wnd_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    let taskbar_msg = WM_TASKBAR_CREATED.load(std::sync::atomic::Ordering::Relaxed);
    if taskbar_msg != 0 && msg == taskbar_msg {
        add_tray_icon(hwnd);
        return LRESULT(0);
    }

    match msg {
        WM_TRAY_CALLBACK => {
            let event = (lparam.0 as u32) & 0xFFFF;
            if event == WM_LBUTTONUP {
                let cb = TRAY_CALLBACKS
                    .lock()
                    .ok()
                    .and_then(|g| g.as_ref().map(|c| Arc::clone(&c.on_toggle)));
                if let Some(cb) = cb {
                    cb();
                }
            } else if event == WM_RBUTTONUP || event == WM_CONTEXTMENU {
                show_context_menu(hwnd);
            }
            LRESULT(0)
        }
        WM_COMMAND => {
            let cmd = wparam.0 & 0xFFFF;
            match cmd {
                ID_MENU_TOGGLE => {
                    let cb = TRAY_CALLBACKS
                        .lock()
                        .ok()
                        .and_then(|g| g.as_ref().map(|c| Arc::clone(&c.on_toggle)));
                    if let Some(cb) = cb {
                        cb();
                    }
                }
                ID_MENU_SETTINGS => {
                    let cb = TRAY_CALLBACKS
                        .lock()
                        .ok()
                        .and_then(|g| g.as_ref().map(|c| Arc::clone(&c.on_settings)));
                    if let Some(cb) = cb {
                        cb();
                    }
                }
                ID_MENU_EXIT => {
                    remove_tray_icon(hwnd);
                    let cb = TRAY_CALLBACKS
                        .lock()
                        .ok()
                        .and_then(|g| g.as_ref().map(|c| Arc::clone(&c.on_exit)));
                    if let Some(cb) = cb {
                        cb();
                    }
                    unsafe {
                        let _ = DestroyWindow(hwnd);
                    }
                }
                _ => {}
            }
            LRESULT(0)
        }
        WM_CLOSE => {
            remove_tray_icon(hwnd);
            unsafe {
                let _ = DestroyWindow(hwnd);
            }
            LRESULT(0)
        }
        WM_DESTROY => {
            remove_tray_icon(hwnd);
            unsafe {
                PostQuitMessage(0);
            }
            LRESULT(0)
        }
        _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    }
}

pub fn start_tray_icon<F1, F2, F3>(
    on_toggle: F1,
    on_settings: F2,
    on_exit: F3,
) -> Option<TrayHandle>
where
    F1: Fn() + Send + Sync + 'static,
    F2: Fn() + Send + Sync + 'static,
    F3: Fn() + Send + Sync + 'static,
{
    if let Ok(mut guard) = TRAY_CALLBACKS.lock() {
        *guard = Some(TrayCallbacks {
            on_toggle: Arc::new(on_toggle),
            on_settings: Arc::new(on_settings),
            on_exit: Arc::new(on_exit),
        });
    }

    let (hwnd_tx, hwnd_rx) = std::sync::mpsc::sync_channel::<isize>(1);
    let spawned = std::thread::Builder::new()
        .name("yanxi-win-tray".into())
        .spawn(move || unsafe {
            let taskbar_w = wide_null("TaskbarCreated");
            let registered = RegisterWindowMessageW(PCWSTR(taskbar_w.as_ptr()));
            WM_TASKBAR_CREATED.store(registered, std::sync::atomic::Ordering::Relaxed);

            let hmodule = match GetModuleHandleW(None) {
                Ok(h) => h,
                Err(_) => {
                    let _ = hwnd_tx.send(0);
                    return;
                }
            };
            let hicon = load_tray_hicon().unwrap_or_default();
            TRAY_HICON.store(hicon.0 as isize, std::sync::atomic::Ordering::Relaxed);

            let class_w = wide_null("YanxiTrayWindowClass");
            let wnd_class = WNDCLASSEXW {
                cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
                lpfnWndProc: Some(tray_wnd_proc),
                hInstance: hmodule.into(),
                lpszClassName: PCWSTR(class_w.as_ptr()),
                ..Default::default()
            };
            RegisterClassExW(&wnd_class);

            let title_w = wide_null("YanxiTray");
            let hwnd = match CreateWindowExW(
                WINDOW_EX_STYLE(0),
                PCWSTR(class_w.as_ptr()),
                PCWSTR(title_w.as_ptr()),
                WINDOW_STYLE(0),
                0,
                0,
                0,
                0,
                None,
                None,
                Some(hmodule.into()),
                None,
            ) {
                Ok(h) => h,
                Err(_) => {
                    if !hicon.0.is_null() {
                        let _ = DestroyIcon(hicon);
                    }
                    let _ = hwnd_tx.send(0);
                    return;
                }
            };

            add_tray_icon(hwnd);
            TRAY_HWND.store(hwnd.0 as isize, std::sync::atomic::Ordering::Relaxed);
            let _ = hwnd_tx.send(hwnd.0 as isize);

            let mut msg = MSG::default();
            while GetMessageW(&mut msg, None, 0, 0).0 > 0 {
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }

            TRAY_HWND.store(0, std::sync::atomic::Ordering::Relaxed);
            if !hicon.0.is_null() {
                let _ = DestroyIcon(hicon);
                TRAY_HICON.store(0, std::sync::atomic::Ordering::Relaxed);
            }
        });

    if spawned.is_err() {
        return None;
    }
    match hwnd_rx.recv() {
        Ok(hwnd) if hwnd != 0 => Some(TrayHandle { hwnd }),
        _ => None,
    }
}
