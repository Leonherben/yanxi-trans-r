pub mod fonts;
pub mod popup;
pub mod state;
#[cfg(windows)]
mod tray_win32;

use std::sync::{Arc, Mutex};
use tokio::sync::mpsc::UnboundedReceiver;

use crate::cache::SQLiteCache;
use crate::config::{AppConfig, SelectionMode};
use crate::models::TranslationRequest;
use crate::selection::{SelectionEvent, create_selection_listener, start_global_hotkey_listener};
use crate::single_instance::{InstanceCheck, SingleInstance};
use crate::translator::create_translator;
use popup::PopupApp;
use state::{GuiCommand, SharedPopupState, TranslationStatus};

#[cfg(target_os = "linux")]
use x11rb::connection::Connection;
#[cfg(target_os = "linux")]
use x11rb::wrapper::ConnectionExt as _;

#[cfg(target_os = "linux")]
thread_local! {
    static X11_CONN: std::cell::RefCell<Option<(x11rb::rust_connection::RustConnection, usize, u32)>> = const { std::cell::RefCell::new(None) };
}

#[cfg(target_os = "linux")]
fn with_x11_conn<R>(
    f: impl FnOnce(&x11rb::rust_connection::RustConnection, usize, u32) -> R,
) -> Option<R> {
    X11_CONN.with(|cell| {
        let mut opt = cell.borrow_mut();
        if opt.is_none() {
            if let Ok((conn, screen_num)) = x11rb::connect(None) {
                use x11rb::protocol::xproto::ConnectionExt;
                if let Some(atom) = conn
                    .intern_atom(false, b"_NET_MOVERESIZE_WINDOW")
                    .ok()
                    .and_then(|c| c.reply().ok())
                    .map(|r| r.atom)
                {
                    *opt = Some((conn, screen_num, atom));
                }
            }
        }
        opt.as_ref()
            .map(|(conn, screen_num, atom)| f(conn, *screen_num, *atom))
    })
}

#[cfg(target_os = "linux")]
pub fn move_resize_x11(win: u32, x: i32, y: i32, w: u32, h: u32) {
    with_x11_conn(|conn, screen_num, atom| {
        use x11rb::protocol::xproto::{
            ClientMessageData, ClientMessageEvent, ConnectionExt, EventMask,
        };
        let root = conn.setup().roots[screen_num].root;
        let flags = 0x0F01u32; // NorthWest + x + y + w + h
        let data = [flags, x.max(0) as u32, y.max(0) as u32, w, h];
        let event = ClientMessageEvent {
            response_type: x11rb::protocol::xproto::CLIENT_MESSAGE_EVENT,
            format: 32,
            sequence: 0,
            window: win,
            type_: atom,
            data: ClientMessageData::from(data),
        };
        // Avoid synchronous cookie.check() on every drag frame.
        let _ = conn.send_event(
            false,
            root,
            EventMask::SUBSTRUCTURE_REDIRECT | EventMask::SUBSTRUCTURE_NOTIFY,
            event,
        );
        let _ = conn.flush();
    });
}

#[cfg(target_os = "linux")]
pub fn get_window_position_x11(win: u32) -> Option<(i32, i32)> {
    with_x11_conn(|conn, screen_num, _| {
        use x11rb::protocol::xproto::ConnectionExt;
        let root = conn.setup().roots[screen_num].root;
        conn.translate_coordinates(win, root, 0, 0)
            .ok()?
            .reply()
            .ok()
            .map(|reply| (reply.dst_x as i32, reply.dst_y as i32))
    })
    .flatten()
}

pub fn cursor_pos_screen() -> Option<(i32, i32)> {
    #[cfg(target_os = "linux")]
    {
        with_x11_conn(|conn, screen_num, _| {
            use x11rb::protocol::xproto::ConnectionExt;
            let root = conn.setup().roots[screen_num].root;
            conn.query_pointer(root)
                .ok()?
                .reply()
                .ok()
                .map(|reply| (reply.root_x as i32, reply.root_y as i32))
        })
        .flatten()
    }
    #[cfg(windows)]
    {
        cursor_pos_win32()
    }
    #[cfg(not(any(target_os = "linux", windows)))]
    {
        None
    }
}

#[cfg(target_os = "linux")]
fn send_wm_state_x11(win: u32, action: u32, prop_names: &[&[u8]]) {
    use x11rb::protocol::xproto::{
        ClientMessageData, ClientMessageEvent, ConnectionExt, EventMask,
    };
    let Ok((conn, screen_num)) = x11rb::connect(None) else {
        return;
    };
    let root = conn.setup().roots[screen_num].root;
    let Ok(Ok(state)) = conn.intern_atom(false, b"_NET_WM_STATE").map(|c| c.reply()) else {
        return;
    };
    for name in prop_names {
        let Ok(Ok(atom)) = conn.intern_atom(false, name).map(|c| c.reply()) else {
            continue;
        };
        let event = ClientMessageEvent {
            response_type: x11rb::protocol::xproto::CLIENT_MESSAGE_EVENT,
            format: 32,
            sequence: 0,
            window: win,
            type_: state.atom,
            data: ClientMessageData::from([action, atom.atom, 0, 1, 0]),
        };
        let _ = conn.send_event(
            false,
            root,
            EventMask::SUBSTRUCTURE_REDIRECT | EventMask::SUBSTRUCTURE_NOTIFY,
            event,
        );
    }
    let _ = conn.flush();
}

/// Clear WM window states that override an explicit size (maximized/fullscreen).
#[cfg(target_os = "linux")]
pub fn clear_maximized_state_x11(win: u32) {
    send_wm_state_x11(
        win,
        0,
        &[
            b"_NET_WM_STATE_MAXIMIZED_VERT",
            b"_NET_WM_STATE_MAXIMIZED_HORZ",
            b"_NET_WM_STATE_FULLSCREEN",
        ],
    );
}

#[cfg(target_os = "linux")]
pub fn set_always_on_top_x11(win: u32, always_on_top: bool) {
    let action = if always_on_top { 1 } else { 0 };
    send_wm_state_x11(win, action, &[b"_NET_WM_STATE_ABOVE"]);
}

#[cfg(target_os = "linux")]
pub fn raise_window_x11(win: u32) {
    use x11rb::protocol::xproto::{ConfigureWindowAux, ConnectionExt, StackMode};
    if let Ok((conn, _)) = x11rb::connect(None) {
        let _ = conn.configure_window(win, &ConfigureWindowAux::new().stack_mode(StackMode::ABOVE));
        let _ = conn.flush();
    }
}

#[cfg(target_os = "linux")]
pub fn get_window_geometry_x11(win: u32) -> Option<((f32, f32), (f32, f32))> {
    use x11rb::protocol::xproto::ConnectionExt;
    let (conn, screen_num) = x11rb::connect(None).ok()?;
    let root = conn.setup().roots[screen_num].root;
    let position = conn
        .translate_coordinates(win, root, 0, 0)
        .ok()?
        .reply()
        .ok()?;
    let size = conn.get_geometry(win).ok()?.reply().ok()?;
    Some((
        (position.dst_x as f32, position.dst_y as f32),
        (size.width as f32, size.height as f32),
    ))
}

#[cfg(windows)]
fn hwnd_from_isize(hwnd: isize) -> windows::Win32::Foundation::HWND {
    windows::Win32::Foundation::HWND(hwnd as *mut core::ffi::c_void)
}

#[cfg(windows)]
fn enable_dpi_awareness_win32() {
    use windows::Win32::UI::HiDpi::{
        DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, SetProcessDpiAwarenessContext,
    };
    unsafe {
        let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
    }
}

#[cfg(windows)]
pub fn dpi_scale_for_point_win32(px: i32, py: i32) -> f32 {
    use windows::Win32::Foundation::POINT;
    use windows::Win32::Graphics::Gdi::{MONITOR_DEFAULTTONEAREST, MonitorFromPoint};
    use windows::Win32::UI::HiDpi::{GetDpiForMonitor, MDT_EFFECTIVE_DPI};
    let monitor = unsafe { MonitorFromPoint(POINT { x: px, y: py }, MONITOR_DEFAULTTONEAREST) };
    let mut dpi_x = 96u32;
    let mut dpi_y = 96u32;
    if unsafe { GetDpiForMonitor(monitor, MDT_EFFECTIVE_DPI, &mut dpi_x, &mut dpi_y) }.is_ok()
        && dpi_x > 0
    {
        (dpi_x as f32 / 96.0).max(1.0)
    } else {
        1.0
    }
}

/// Strip DWM caption/frame styles that winit can re-apply on visibility or level updates.
#[cfg(windows)]
pub fn ensure_borderless_popup_style_win32(hwnd: isize) {
    use windows::Win32::Graphics::Dwm::{
        DWMWA_WINDOW_CORNER_PREFERENCE, DWMWCP_ROUND, DwmSetWindowAttribute,
    };
    use windows::Win32::UI::WindowsAndMessaging::{
        GWL_EXSTYLE, GWL_STYLE, GetWindowLongW, SWP_FRAMECHANGED, SWP_NOACTIVATE, SWP_NOMOVE,
        SWP_NOSIZE, SWP_NOZORDER, SetWindowLongW, SetWindowPos, WS_BORDER, WS_CAPTION,
        WS_CLIPCHILDREN, WS_CLIPSIBLINGS, WS_DLGFRAME, WS_EX_APPWINDOW, WS_EX_CLIENTEDGE,
        WS_EX_DLGMODALFRAME, WS_EX_TOOLWINDOW, WS_EX_WINDOWEDGE, WS_MAXIMIZEBOX, WS_MINIMIZEBOX,
        WS_POPUP, WS_SYSMENU, WS_THICKFRAME,
    };
    let handle = hwnd_from_isize(hwnd);
    unsafe {
        let cur_style = GetWindowLongW(handle, GWL_STYLE) as u32;
        let strip_style = (WS_CAPTION
            | WS_THICKFRAME
            | WS_MINIMIZEBOX
            | WS_MAXIMIZEBOX
            | WS_SYSMENU
            | WS_BORDER
            | WS_DLGFRAME)
            .0;
        let add_style = (WS_POPUP | WS_CLIPCHILDREN | WS_CLIPSIBLINGS).0;
        let target_style = (cur_style & !strip_style) | add_style;

        let cur_ex = GetWindowLongW(handle, GWL_EXSTYLE) as u32;
        let strip_ex =
            (WS_EX_APPWINDOW | WS_EX_WINDOWEDGE | WS_EX_DLGMODALFRAME | WS_EX_CLIENTEDGE).0;
        let target_ex = (cur_ex & !strip_ex) | WS_EX_TOOLWINDOW.0;

        let mut changed = false;
        if cur_style != target_style {
            SetWindowLongW(handle, GWL_STYLE, target_style as i32);
            changed = true;
        }
        if cur_ex != target_ex {
            SetWindowLongW(handle, GWL_EXSTYLE, target_ex as i32);
            changed = true;
        }
        if changed {
            let corner = DWMWCP_ROUND;
            let _ = DwmSetWindowAttribute(
                handle,
                DWMWA_WINDOW_CORNER_PREFERENCE,
                &corner as *const _ as *const core::ffi::c_void,
                std::mem::size_of_val(&corner) as u32,
            );
            let _ = SetWindowPos(
                handle,
                None,
                0,
                0,
                0,
                0,
                SWP_NOMOVE | SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE | SWP_FRAMECHANGED,
            );
        }
    }
}

#[cfg(windows)]
pub fn wake_win32_window(hwnd: isize) {
    use windows::Win32::UI::WindowsAndMessaging::{SW_SHOWNOACTIVATE, ShowWindowAsync};
    unsafe {
        let _ = ShowWindowAsync(hwnd_from_isize(hwnd), SW_SHOWNOACTIVATE);
    }
}

#[cfg(windows)]
pub fn show_and_raise_win32(hwnd: isize, always_on_top: bool, activate: bool) {
    use windows::Win32::UI::WindowsAndMessaging::{
        HWND_NOTOPMOST, HWND_TOPMOST, IsIconic, IsWindowVisible, SW_RESTORE, SW_SHOW,
        SW_SHOWNOACTIVATE, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE, SWP_SHOWWINDOW,
        SetForegroundWindow, SetWindowPos, ShowWindow,
    };
    let handle = hwnd_from_isize(hwnd);
    ensure_borderless_popup_style_win32(hwnd);
    unsafe {
        if IsIconic(handle).as_bool() {
            let _ = ShowWindow(handle, SW_RESTORE);
        }
        if !IsWindowVisible(handle).as_bool() {
            let _ = ShowWindow(handle, if activate { SW_SHOW } else { SW_SHOWNOACTIVATE });
        }
        if always_on_top {
            let _ = SetWindowPos(
                handle,
                Some(HWND_TOPMOST),
                0,
                0,
                0,
                0,
                SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE | SWP_SHOWWINDOW,
            );
        } else {
            // Brief TOPMOST->NOTOPMOST promotes an unactivated popup above the foreground app.
            let _ = SetWindowPos(
                handle,
                Some(HWND_TOPMOST),
                0,
                0,
                0,
                0,
                SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE | SWP_SHOWWINDOW,
            );
            let _ = SetWindowPos(
                handle,
                Some(HWND_NOTOPMOST),
                0,
                0,
                0,
                0,
                SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE,
            );
        }
        if activate {
            let _ = SetForegroundWindow(handle);
        }
    }
}

#[cfg(windows)]
pub fn move_resize_win32(hwnd: isize, x: i32, y: i32, w: i32, h: i32) {
    use windows::Win32::UI::WindowsAndMessaging::{SWP_NOACTIVATE, SWP_NOZORDER, SetWindowPos};
    ensure_borderless_popup_style_win32(hwnd);
    unsafe {
        let _ = SetWindowPos(
            hwnd_from_isize(hwnd),
            None,
            x,
            y,
            w,
            h,
            SWP_NOZORDER | SWP_NOACTIVATE,
        );
    }
}

#[cfg(windows)]
pub fn set_always_on_top_win32(hwnd: isize, always_on_top: bool) {
    use windows::Win32::UI::WindowsAndMessaging::{
        HWND_NOTOPMOST, HWND_TOPMOST, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE, SetWindowPos,
    };
    ensure_borderless_popup_style_win32(hwnd);
    let insert_after = if always_on_top {
        HWND_TOPMOST
    } else {
        HWND_NOTOPMOST
    };
    unsafe {
        let _ = SetWindowPos(
            hwnd_from_isize(hwnd),
            Some(insert_after),
            0,
            0,
            0,
            0,
            SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE,
        );
    }
}

#[cfg(windows)]
pub fn raise_window_win32(hwnd: isize) {
    show_and_raise_win32(hwnd, false, false);
}

pub fn set_always_on_top(x11_window: Option<u32>, win32_hwnd: Option<isize>, always_on_top: bool) {
    #[cfg(target_os = "linux")]
    if let Some(win) = x11_window {
        set_always_on_top_x11(win, always_on_top);
    }
    #[cfg(windows)]
    if let Some(hwnd) = win32_hwnd {
        set_always_on_top_win32(hwnd, always_on_top);
    }
    let _ = (x11_window, win32_hwnd, always_on_top);
}

pub fn raise_window(x11_window: Option<u32>, win32_hwnd: Option<isize>) {
    #[cfg(target_os = "linux")]
    if let Some(win) = x11_window {
        raise_window_x11(win);
    }
    #[cfg(windows)]
    if let Some(hwnd) = win32_hwnd {
        raise_window_win32(hwnd);
    }
    let _ = (x11_window, win32_hwnd);
}

#[cfg(windows)]
pub fn get_window_geometry_win32(hwnd: isize) -> Option<((f32, f32), (f32, f32))> {
    use windows::Win32::Foundation::RECT;
    use windows::Win32::UI::WindowsAndMessaging::GetWindowRect;
    let mut rect = RECT::default();
    unsafe { GetWindowRect(hwnd_from_isize(hwnd), &mut rect).ok()? };
    let w = (rect.right - rect.left).max(1) as f32;
    let h = (rect.bottom - rect.top).max(1) as f32;
    Some(((rect.left as f32, rect.top as f32), (w, h)))
}

#[cfg(windows)]
pub fn cursor_pos_win32() -> Option<(i32, i32)> {
    use windows::Win32::Foundation::POINT;
    use windows::Win32::UI::WindowsAndMessaging::GetCursorPos;
    let mut pt = POINT::default();
    unsafe { GetCursorPos(&mut pt).ok()? };
    Some((pt.x, pt.y))
}

#[cfg(windows)]
pub fn monitor_work_area_for_point(px: i32, py: i32) -> (f32, f32, f32, f32) {
    use windows::Win32::Foundation::POINT;
    use windows::Win32::Graphics::Gdi::{
        GetMonitorInfoW, MONITOR_DEFAULTTONEAREST, MONITORINFO, MonitorFromPoint,
    };
    let monitor = unsafe { MonitorFromPoint(POINT { x: px, y: py }, MONITOR_DEFAULTTONEAREST) };
    let mut info = MONITORINFO {
        cbSize: std::mem::size_of::<MONITORINFO>() as u32,
        ..Default::default()
    };
    if unsafe { GetMonitorInfoW(monitor, &mut info).as_bool() } {
        let w = (info.rcWork.right - info.rcWork.left).max(400) as f32;
        let h = (info.rcWork.bottom - info.rcWork.top).max(300) as f32;
        return (info.rcWork.left as f32, info.rcWork.top as f32, w, h);
    }
    (0.0, 0.0, 1920.0, 1080.0)
}

/// State geometry is always in physical pixels. Do not send a second logical
/// move after a native X11/Win32 move: mixed units make windows jump at high DPI.
pub fn apply_window_geometry(
    ctx: &eframe::egui::Context,
    x11_window: Option<u32>,
    win32_hwnd: Option<isize>,
    pos: (f32, f32),
    size: (f32, f32),
) {
    #[cfg(target_os = "linux")]
    if let Some(window) = x11_window {
        move_resize_x11(
            window,
            pos.0.round() as i32,
            pos.1.round() as i32,
            size.0.round() as u32,
            size.1.round() as u32,
        );
        return;
    }
    #[cfg(windows)]
    if let Some(hwnd) = win32_hwnd {
        move_resize_win32(
            hwnd,
            pos.0.round() as i32,
            pos.1.round() as i32,
            size.0.round() as i32,
            size.1.round() as i32,
        );
        return;
    }
    let _ = (x11_window, win32_hwnd);
    let scale = ctx.pixels_per_point();
    ctx.send_viewport_cmd(eframe::egui::ViewportCommand::OuterPosition(
        eframe::egui::pos2(pos.0 / scale, pos.1 / scale),
    ));
    ctx.send_viewport_cmd(eframe::egui::ViewportCommand::InnerSize(
        eframe::egui::vec2(size.0 / scale, size.1 / scale),
    ));
}

pub fn load_app_icon() -> Option<Arc<eframe::egui::IconData>> {
    let icon = eframe::icon_data::from_png_bytes(include_bytes!("../../assets/icon.png")).ok()?;
    Some(Arc::new(icon))
}

pub fn default_window_size(screen_w: f32, screen_h: f32) -> (f32, f32) {
    let w = (screen_w * 0.30).clamp(420.0, 760.0).round();
    let h = (screen_h * 0.35).clamp(300.0, 560.0).round();
    (w, h)
}

pub fn default_window_size_for_dpi(screen_w: f32, screen_h: f32, dpi_scale: f32) -> (f32, f32) {
    let scale = dpi_scale.max(1.0);
    let (lw, lh) = default_window_size(screen_w / scale, screen_h / scale);
    ((lw * scale).round(), (lh * scale).round())
}

pub fn run_gui() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    #[cfg(windows)]
    enable_dpi_awareness_win32();

    // 1. 单实例进程保护 (Single Instance Lock)
    let instance_check = match SingleInstance::check("yanxi") {
        Ok(check) => check,
        Err(e) => {
            eprintln!("[Yanxi] ⚠️ 进程锁初始化提示: {e}");
            return Ok(());
        }
    };

    let mut single_instance = match instance_check {
        InstanceCheck::Primary(inst) => inst,
        InstanceCheck::AlreadyRunning => {
            println!("[Yanxi] ⚡ 已存在正在运行的言蹊翻译实例，已发送指令唤醒现有悬浮窗。");
            return Ok(());
        }
    };

    let config = AppConfig::load();
    let cache = SQLiteCache::new()?;

    let state = Arc::new(Mutex::new(SharedPopupState::new(&config)));

    // 2. 建立双向异步通信管道
    let (tx_sel, rx_sel) = tokio::sync::mpsc::unbounded_channel::<SelectionEvent>();
    let (tx_cmd, rx_cmd) = tokio::sync::mpsc::unbounded_channel::<GuiCommand>();

    {
        let mut lock = state.lock().unwrap();
        lock.tx_command = Some(tx_cmd.clone());
    }

    // 3. 启动划词监听器 (X11 XFixes)
    let listener = create_selection_listener(config.selection.debounce_ms, 1, 3000);
    listener.start(tx_sel)?;

    // 4. 注册单实例唤醒监听
    let state_ipc = Arc::clone(&state);
    single_instance.listen_activations(move || {
        println!("[Yanxi] 🔔 收到外部进程激活唤醒指令");
        let (x11_win, win32_hwnd, is_pinned, ctx) = match state_ipc.lock() {
            Ok(mut lock) => {
                lock.mark_shown(std::time::Instant::now());
                lock.should_update_pos = true;
                lock.pending_win32_show = Some(true);
                (
                    lock.x11_window,
                    lock.win32_hwnd,
                    lock.is_pinned,
                    lock.ctx.clone(),
                )
            }
            Err(_) => return,
        };
        #[cfg(target_os = "linux")]
        if !is_pinned {
            if let Some(win) = x11_win {
                raise_window_x11(win);
            }
        }
        #[cfg(windows)]
        if let Some(hwnd) = win32_hwnd {
            wake_win32_window(hwnd);
        }
        let _ = (x11_win, win32_hwnd, is_pinned);
        if let Some(ctx) = ctx {
            ctx.request_repaint();
        }
    });

    // 5. 注册全局快捷键监听 (默认 Alt + Q，支持动态热重载)
    let (tx_hotkey, rx_hotkey) = std::sync::mpsc::channel::<String>();
    {
        let mut lock = state.lock().unwrap();
        lock.tx_hotkey = Some(tx_hotkey);
    }
    let state_hotkey = Arc::clone(&state);
    let state_hotkey_status = Arc::clone(&state);
    let initial_hotkey = config.selection.hotkey.clone();
    start_global_hotkey_listener(
        initial_hotkey,
        rx_hotkey,
        move || {
            // Read only the current selection, never a previously copied clipboard item.
            // The selection helper has a bounded wait and runs outside the state lock.
            let selection = crate::selection::get_current_selection();
            #[cfg(windows)]
            let win32_cursor_area = cursor_pos_win32().map(|(px, py)| {
                let (ox, oy, s_w, s_h) = monitor_work_area_for_point(px, py);
                (px, py, ox, oy, s_w, s_h)
            });

            let snapshot = if let Ok(mut lock) = state_hotkey.lock() {
                apply_hotkey_selection(&mut lock, selection.as_deref());
                let mut x11_move: Option<(u32, (f32, f32), (f32, f32))> = None;
                let mut x11_raise: Option<u32> = None;
                if lock.is_visible && !lock.is_pinned {
                    #[cfg(target_os = "linux")]
                    {
                        if let Ok((conn, screen_num)) = x11rb::connect(None) {
                            use x11rb::protocol::xproto::ConnectionExt;
                            let root = conn.setup().roots[screen_num].root;
                            let s_w = conn.setup().roots[screen_num].width_in_pixels as f32;
                            let s_h = conn.setup().roots[screen_num].height_in_pixels as f32;
                            lock.screen_size = (s_w, s_h);
                            if let Ok(cookie) = conn.query_pointer(root) {
                                if let Ok(reply) = cookie.reply() {
                                    let (px, py) = (reply.root_x as i32, reply.root_y as i32);
                                    lock.cursor_pos = (px, py);
                                    let (win_w, win_h) = lock.window_size;
                                    let target_pos = SharedPopupState::compute_target_pos(
                                        px, py, win_w, win_h, s_w, s_h,
                                    );
                                    lock.window_pos = Some(target_pos);
                                    if let Some(win) = lock.x11_window {
                                        x11_move = Some((win, target_pos, (win_w, win_h)));
                                    }
                                }
                            }
                        }
                        x11_raise = lock.x11_window;
                    }
                    #[cfg(windows)]
                    {
                        if let Some((px, py, ox, oy, s_w, s_h)) = win32_cursor_area {
                            lock.screen_origin = (ox, oy);
                            lock.screen_size = (s_w, s_h);
                            lock.cursor_pos = (px, py);
                            let (win_w, win_h) = lock.window_size;
                            let target_pos = SharedPopupState::compute_target_pos_in_bounds(
                                px,
                                py,
                                win_w,
                                win_h,
                                (ox, oy, s_w, s_h),
                            );
                            lock.window_pos = Some(target_pos);
                        }
                    }
                    lock.should_update_pos = true;
                }
                if lock.is_visible {
                    lock.pending_win32_show = Some(true);
                }
                Some((
                    lock.is_visible,
                    lock.win32_hwnd,
                    x11_move,
                    x11_raise,
                    lock.ctx.clone(),
                ))
            } else {
                None
            };

            if let Some((is_visible, win32_hwnd, x11_move, x11_raise, ctx)) = snapshot {
                #[cfg(target_os = "linux")]
                {
                    if let Some((win, pos, size)) = x11_move {
                        move_resize_x11(win, pos.0 as i32, pos.1 as i32, size.0 as u32, size.1 as u32);
                    }
                    if let Some(win) = x11_raise {
                        raise_window_x11(win);
                    }
                }
                #[cfg(windows)]
                if is_visible {
                    if let Some(hwnd) = win32_hwnd {
                        wake_win32_window(hwnd);
                    }
                }
                let _ = (is_visible, win32_hwnd, x11_move, x11_raise);
                if let Some(ctx) = ctx {
                    ctx.request_repaint();
                }
            }
        },
        move |ok, message| {
            println!("[Yanxi Hotkey] {message}");
            if let Ok(mut lock) = state_hotkey_status.lock() {
                lock.hotkey_feedback = Some((ok, message));
                lock.request_repaint();
            }
        },
    );

    #[cfg(windows)]
    let _win_tray = {
        let state_toggle = Arc::clone(&state);
        let state_settings = Arc::clone(&state);
        let state_exit = Arc::clone(&state);
        let config_tray = config.clone();
        tray_win32::start_tray_icon(
            move || {
                let win32_cursor_area = cursor_pos_win32().map(|(px, py)| {
                    let (ox, oy, s_w, s_h) = monitor_work_area_for_point(px, py);
                    (px, py, ox, oy, s_w, s_h)
                });
                let snapshot = if let Ok(mut lock) = state_toggle.lock() {
                    if lock.is_visible {
                        lock.hide_explicit();
                    } else {
                        lock.mark_shown(std::time::Instant::now());
                        if !lock.is_pinned {
                            if let Some((px, py, ox, oy, s_w, s_h)) = win32_cursor_area {
                                lock.screen_origin = (ox, oy);
                                lock.screen_size = (s_w, s_h);
                                lock.cursor_pos = (px, py);
                                let (win_w, win_h) = lock.window_size;
                                let target_pos = SharedPopupState::compute_target_pos_in_bounds(
                                    px,
                                    py,
                                    win_w,
                                    win_h,
                                    (ox, oy, s_w, s_h),
                                );
                                lock.window_pos = Some(target_pos);
                            }
                            lock.should_update_pos = true;
                        }
                        lock.pending_win32_show = Some(true);
                    }
                    Some((lock.is_visible, lock.win32_hwnd, lock.ctx.clone()))
                } else {
                    None
                };
                if let Some((is_visible, win32_hwnd, ctx)) = snapshot {
                    if is_visible {
                        if let Some(hwnd) = win32_hwnd {
                            wake_win32_window(hwnd);
                        }
                    }
                    if let Some(ctx) = ctx {
                        ctx.request_repaint();
                    }
                }
            },
            move || {
                let snapshot = if let Ok(mut lock) = state_settings.lock() {
                    lock.mark_shown(std::time::Instant::now());
                    lock.show_settings = true;
                    let active_p = lock.active_provider.clone();
                    lock.load_provider_settings(&config_tray, &active_p);
                    lock.pending_win32_show = Some(true);
                    Some((lock.win32_hwnd, lock.ctx.clone()))
                } else {
                    None
                };
                if let Some((win32_hwnd, ctx)) = snapshot {
                    if let Some(hwnd) = win32_hwnd {
                        wake_win32_window(hwnd);
                    }
                    if let Some(ctx) = ctx {
                        ctx.request_repaint();
                    }
                }
            },
            move || {
                let ctx = if let Ok(mut lock) = state_exit.lock() {
                    lock.pending_win32_quit = true;
                    lock.ctx.clone()
                } else {
                    None
                };
                if let Some(ctx) = ctx {
                    ctx.request_repaint();
                }
            },
        )
    };

    // 6. 启动后台 Tokio 异步工作运行时
    let state_clone = Arc::clone(&state);
    let config_clone = config.clone();
    let cache_clone = cache.clone();

    std::thread::Builder::new()
        .name("yanxi-async-worker".into())
        .spawn(move || {
            let rt = match tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()
            {
                Ok(r) => r,
                Err(e) => {
                    eprintln!("[Yanxi] 无法创建 Tokio 异步运行时: {e}");
                    return;
                }
            };

            rt.block_on(async {
                handle_worker_events(rx_sel, rx_cmd, state_clone, config_clone, cache_clone).await;
            });
        })?;

    // 7. 在主线程启动 eframe 原生无边框透明视口 (使用 OpenGL Glow 后端，配合 Utility/无任务栏窗口类型)
    #[cfg(target_os = "linux")]
    let (init_origin, init_screen, (init_w, init_h), init_scale) = x11rb::connect(None)
        .ok()
        .map(|(conn, screen_num)| {
            let root = &conn.setup().roots[screen_num];
            let sw = root.width_in_pixels as f32;
            let sh = root.height_in_pixels as f32;
            ((0.0, 0.0), (sw, sh), default_window_size(sw, sh), 1.0f32)
        })
        .unwrap_or(((0.0, 0.0), (1920.0, 1080.0), (500.0, 320.0), 1.0f32));
    #[cfg(windows)]
    let (init_origin, init_screen, (init_w, init_h), init_scale) = {
        let (px, py) = cursor_pos_win32().unwrap_or((0, 0));
        let scale = dpi_scale_for_point_win32(px, py);
        let (ox, oy, sw, sh) = monitor_work_area_for_point(px, py);
        (
            (ox, oy),
            (sw, sh),
            default_window_size_for_dpi(sw, sh, scale),
            scale,
        )
    };
    #[cfg(not(any(target_os = "linux", windows)))]
    let (init_origin, init_screen, (init_w, init_h), init_scale) =
        ((0.0, 0.0), (1920.0, 1080.0), (500.0, 320.0), 1.0f32);

    let (start_pos, start_size) = if let Ok(mut lock) = state.lock() {
        lock.screen_origin = init_origin;
        lock.screen_size = init_screen;
        if config.ui.pinned {
            let raw_size = config
                .ui
                .pinned_size
                .map(|(w, h)| (w.max(380.0 * init_scale), h.max(220.0 * init_scale)))
                .unwrap_or((init_w, init_h));
            let raw_pos = config.ui.pinned_pos.unwrap_or((
                init_origin.0 + ((init_screen.0 - raw_size.0) / 2.0).max(10.0),
                init_origin.1 + ((init_screen.1 - raw_size.1) / 2.0).max(10.0),
            ));
            let (clamped_pos, clamped_size) = SharedPopupState::clamp_window_to_bounds(
                raw_pos,
                raw_size,
                (init_origin.0, init_origin.1, init_screen.0, init_screen.1),
            );
            lock.window_pos = Some(clamped_pos);
            lock.window_size = clamped_size;
            (Some(clamped_pos), clamped_size)
        } else {
            lock.window_size = (init_w, init_h);
            (None, (init_w, init_h))
        }
    } else {
        (None, (init_w, init_h))
    };

    let initial_level = if config.ui.pinned {
        eframe::egui::WindowLevel::AlwaysOnTop
    } else {
        eframe::egui::WindowLevel::Normal
    };

    let mut viewport = eframe::egui::ViewportBuilder::default()
        .with_inner_size([start_size.0 / init_scale, start_size.1 / init_scale])
        .with_min_inner_size([380.0, 220.0])
        .with_decorations(false)
        .with_transparent(true)
        .with_window_level(initial_level)
        .with_active(false)
        .with_taskbar(false)
        .with_minimize_button(false)
        .with_maximize_button(false)
        .with_resizable(cfg!(not(windows)))
        .with_app_id("yanxi")
        .with_window_type(eframe::egui::X11WindowType::Utility);
    if let Some(icon) = load_app_icon() {
        viewport = viewport.with_icon(icon);
    }

    let native_options = eframe::NativeOptions {
        renderer: eframe::Renderer::Glow,
        persist_window: false,
        viewport,
        ..Default::default()
    };

    let initial_pinned = config.ui.pinned;
    let state_app = Arc::clone(&state);
    eframe::run_native(
        "yanxi-popup",
        native_options,
        Box::new(move |cc| {
            // 使用清新优雅的明亮主题，圆润高对比
            cc.egui_ctx.set_visuals(eframe::egui::Visuals::light());
            fonts::configure_cjk_fonts(&cc.egui_ctx);
            // Use our own native handle: matching windows by title can bind to a
            // different yanxi window when several instances are visible.
            #[cfg(target_os = "linux")]
            if let Some(win) = find_own_popup_window(std::process::id()) {
                apply_popup_window_role(win, initial_pinned);
                if let Some(pos) = start_pos {
                    move_resize_x11(
                        win,
                        pos.0.round() as i32,
                        pos.1.round() as i32,
                        start_size.0.round() as u32,
                        start_size.1.round() as u32,
                    );
                } else {
                    center_popup_window(win);
                }
                if let Ok(mut lock) = state_app.lock() {
                    lock.x11_window = Some(win);
                }
            }
            #[cfg(windows)]
            {
                use raw_window_handle::{HasWindowHandle, RawWindowHandle};
                if let Ok(handle) = cc.window_handle() {
                    if let RawWindowHandle::Win32(win32) = handle.as_raw() {
                        let hwnd = win32.hwnd.get();
                        ensure_borderless_popup_style_win32(hwnd);
                        let pos = start_pos.unwrap_or((
                            init_origin.0 + ((init_screen.0 - start_size.0) / 2.0).max(10.0),
                            init_origin.1 + ((init_screen.1 - start_size.1) / 2.0).max(10.0),
                        ));
                        set_always_on_top_win32(hwnd, initial_pinned);
                        move_resize_win32(
                            hwnd,
                            pos.0.round() as i32,
                            pos.1.round() as i32,
                            start_size.0.round() as i32,
                            start_size.1.round() as i32,
                        );
                        if !config.ui.start_hidden {
                            show_and_raise_win32(hwnd, initial_pinned, false);
                        }
                        if let Ok(mut lock) = state_app.lock() {
                            lock.win32_hwnd = Some(hwnd);
                            lock.window_pos = Some(pos);
                        }
                    }
                }
            }
            let _ = (start_pos, start_size, initial_pinned);
            if let Ok(mut lock) = state_app.lock() {
                lock.ctx = Some(cc.egui_ctx.clone());
            }
            Ok(Box::new(PopupApp::new(state_app, config)))
        }),
    )
    .map_err(|e| e.to_string())?;

    Ok(())
}

/// Find our own popup window by the process id the toolkit publishes.
/// Matching the title alone can select another running translation window.
#[cfg(target_os = "linux")]
fn find_own_popup_window(pid: u32) -> Option<u32> {
    use x11rb::protocol::xproto::{AtomEnum, ConnectionExt};
    let (conn, screen_num) = x11rb::connect(None).ok()?;
    let root = conn.setup().roots[screen_num].root;
    let pid_atom = conn
        .intern_atom(false, b"_NET_WM_PID")
        .ok()?
        .reply()
        .ok()?
        .atom;
    let name_atom = conn
        .intern_atom(false, b"_NET_WM_NAME")
        .ok()?
        .reply()
        .ok()?
        .atom;
    for _ in 0..50 {
        std::thread::sleep(std::time::Duration::from_millis(100));
        let tree = conn.query_tree(root).ok()?.reply().ok()?;
        for win in tree.children {
            let name = conn
                .get_property(false, win, name_atom, AtomEnum::ANY, 0, 128)
                .ok()
                .and_then(|cookie| cookie.reply().ok())
                .map(|reply| String::from_utf8_lossy(&reply.value).into_owned())
                .unwrap_or_default();
            if !name.contains("yanxi-popup") {
                continue;
            }
            let owner = conn
                .get_property(false, win, pid_atom, AtomEnum::CARDINAL, 0, 1)
                .ok()
                .and_then(|cookie| cookie.reply().ok())
                .and_then(|reply| reply.value32().and_then(|mut values| values.next()));
            if owner == Some(pid) {
                return Some(win);
            }
        }
    }
    None
}

/// Mark the window as a non-taskbar popup and sync always-on-top state.
#[cfg(target_os = "linux")]
fn apply_popup_window_role(win: u32, always_on_top: bool) {
    let Ok((conn, screen_num)) = x11rb::connect(None) else {
        return;
    };
    use x11rb::protocol::xproto::{AtomEnum, ConnectionExt, PropMode};
    let Ok(state_atom) = conn.intern_atom(false, b"_NET_WM_STATE").map(|c| c.reply()) else {
        return;
    };
    let Ok(state_atom) = state_atom else {
        return;
    };
    let mut names = vec!["_NET_WM_STATE_SKIP_TASKBAR", "_NET_WM_STATE_SKIP_PAGER"];
    if always_on_top {
        names.push("_NET_WM_STATE_ABOVE");
    }
    let atoms: Vec<u32> = names
        .iter()
        .filter_map(|name| conn.intern_atom(false, name.as_bytes()).ok()?.reply().ok())
        .map(|reply| reply.atom)
        .collect();
    let _ = conn.change_property32(
        PropMode::APPEND,
        win,
        state_atom.atom,
        AtomEnum::ATOM,
        &atoms,
    );
    let _ = conn.flush();
    let _ = screen_num;
}

/// Place a newly mapped window at the stored (or screen-centred) position.
#[cfg(target_os = "linux")]
fn center_popup_window(win: u32) {
    let Ok((conn, screen_num)) = x11rb::connect(None) else {
        return;
    };
    let screen = &conn.setup().roots[screen_num];
    let (sw, sh) = (
        screen.width_in_pixels as f32,
        screen.height_in_pixels as f32,
    );
    let (w, h) = default_window_size(sw, sh);
    move_resize_x11(
        win,
        ((sw - w) / 2.0).max(10.0) as i32,
        ((sh - h) / 2.0).max(10.0) as i32,
        w as u32,
        h as u32,
    );
}

fn apply_hotkey_selection(state: &mut SharedPopupState, selection: Option<&str>) {
    let now = std::time::Instant::now();
    match selection {
        Some(text) => match crate::selection::validate_selection_text(text) {
            Ok(clean) if state.is_visible && clean == state.edit_text && !state.show_settings => {
                state.hide_explicit();
            }
            Ok(clean) => {
                state.mark_shown(now);
                state.show_settings = false;
                state.edit_text = clean.clone();
                state.trigger_translate(&clean);
            }
            Err(message) => {
                state.mark_shown(now);
                state.show_settings = false;
                state.status = TranslationStatus::Error {
                    original_text: String::new(),
                    message,
                };
            }
        },
        None => {
            if state.is_visible {
                state.hide_explicit();
            } else {
                state.mark_shown(now);
            }
        }
    }
    state.request_repaint();
}

async fn handle_worker_events(
    mut rx_sel: UnboundedReceiver<SelectionEvent>,
    mut rx_cmd: UnboundedReceiver<GuiCommand>,
    state: Arc<Mutex<SharedPopupState>>,
    mut config: AppConfig,
    cache: SQLiteCache,
) {
    let mut next_request_id: u64 = 0;
    let mut latest_handle: Option<tokio::task::JoinHandle<()>> = None;

    loop {
        tokio::select! {
            Some(event) = rx_sel.recv() => {
                match event {
                    SelectionEvent::Selected { text, pos } => {
                        let (
                            mode,
                            is_visible,
                            is_pinned,
                            last_outside_dismiss_at,
                            s_origin,
                            s_size,
                            x11_win,
                            win32_hwnd,
                            window_size,
                        ) = {
                            let lock = match state.lock() {
                                Ok(l) => l,
                                Err(_) => continue,
                            };
                            (
                                lock.selection_mode,
                                lock.is_visible,
                                lock.is_pinned,
                                lock.last_outside_dismiss_at,
                                lock.screen_origin,
                                lock.screen_size,
                                lock.x11_window,
                                lock.win32_hwnd,
                                lock.window_size,
                            )
                        };

                        // 1. 取词模式策略拦截
                        if mode == SelectionMode::Manual {
                            // 手动模式：划选不触发弹窗
                            continue;
                        }
                        let recently_auto_dismissed = last_outside_dismiss_at
                            .is_some_and(|t| t.elapsed() < std::time::Duration::from_millis(700));
                        if mode == SelectionMode::Companion
                            && !is_visible
                            && !is_pinned
                            && !recently_auto_dismissed
                        {
                            // 伴随阅读模式：悬浮窗未打开且未固定，静默不打扰
                            continue;
                        }

                        let _ = (s_origin, s_size);
                        let (s_origin, s_size) = {
                            #[cfg(windows)]
                            {
                                let (ox, oy, sw, sh) = monitor_work_area_for_point(pos.0, pos.1);
                                ((ox, oy), (sw, sh))
                            }
                            #[cfg(not(windows))]
                            {
                                (s_origin, s_size)
                            }
                        };

                        let (win_w, win_h) = window_size;
                        let target_pos = SharedPopupState::compute_target_pos_in_bounds(
                            pos.0,
                            pos.1,
                            win_w,
                            win_h,
                            (s_origin.0, s_origin.1, s_size.0, s_size.1),
                        );
                        println!("[Yanxi] 划词事件触发 @ ({}, {}): \"{}\"", pos.0, pos.1, text);

                        let (provider_name, source_lang, target_lang, ctx_to_wake) = {
                            let mut lock = match state.lock() {
                                Ok(l) => l,
                                Err(_) => continue,
                            };
                            lock.screen_origin = s_origin;
                            lock.screen_size = s_size;
                            lock.cursor_pos = pos;
                            if !lock.is_pinned {
                                lock.window_pos = Some(target_pos);
                                lock.should_update_pos = true;
                            }
                            lock.pending_win32_show = Some(false);
                            lock.mark_shown(std::time::Instant::now());
                            lock.edit_text = text.clone();
                            lock.status = TranslationStatus::Loading {
                                original_text: text.clone(),
                            };
                            (
                                lock.active_provider.clone(),
                                lock.source_lang.clone(),
                                lock.target_lang.clone(),
                                lock.ctx.clone(),
                            )
                        };
                        if !is_pinned {
                            #[cfg(target_os = "linux")]
                            if let Some(win) = x11_win {
                                move_resize_x11(
                                    win,
                                    target_pos.0 as i32,
                                    target_pos.1 as i32,
                                    win_w as u32,
                                    win_h as u32,
                                );
                                raise_window_x11(win);
                            }
                            #[cfg(windows)]
                            if let Some(hwnd) = win32_hwnd {
                                wake_win32_window(hwnd);
                            }
                        } else {
                            #[cfg(windows)]
                            if let Some(hwnd) = win32_hwnd {
                                wake_win32_window(hwnd);
                            }
                        }
                        let _ = (x11_win, win32_hwnd);
                        if let Some(ctx) = ctx_to_wake {
                            ctx.request_repaint();
                        }

                        next_request_id += 1;
                        let req_id = next_request_id;
                        if let Ok(mut lock) = state.lock() {
                            lock.request_seq = req_id;
                            lock.request_repaint();
                        }
                        if let Some(handle) = latest_handle.take() {
                            handle.abort();
                        }
                        let state_task = Arc::clone(&state);
                        let cache_task = cache.clone();
                        let config_task = config.clone();
                        latest_handle = Some(tokio::spawn(async move {
                            perform_translation(
                                text,
                                source_lang,
                                target_lang,
                                provider_name,
                                config_task,
                                cache_task,
                                state_task,
                                req_id,
                            )
                            .await;
                        }));
                    }
                    SelectionEvent::Cleared { .. } => {
                        // 选区取消时不粗暴关闭悬浮窗，保留窗口供用户查阅译文、发音及复制。
                    }
                    SelectionEvent::OutsideClick { pos } => {
                        let (should_check, mode, x11_win, win32_hwnd) = match state.lock() {
                            Ok(lock) => (
                                lock.is_visible && !lock.is_pinned && !lock.show_settings,
                                lock.selection_mode,
                                lock.x11_window,
                                lock.win32_hwnd,
                            ),
                            Err(_) => continue,
                        };
                        if !should_check {
                            continue;
                        }
                        let native_rect = {
                            #[cfg(target_os = "linux")]
                            {
                                let _ = win32_hwnd;
                                x11_win.and_then(get_window_geometry_x11)
                            }
                            #[cfg(windows)]
                            {
                                let _ = x11_win;
                                win32_hwnd.and_then(get_window_geometry_win32)
                            }
                            #[cfg(not(any(target_os = "linux", windows)))]
                            {
                                let _ = (x11_win, win32_hwnd);
                                None
                            }
                        };
                        if mode == SelectionMode::Companion {
                            let seq = state.lock().ok().and_then(|mut lock| {
                                lock.schedule_outside_dismiss(
                                    pos,
                                    native_rect,
                                    std::time::Instant::now(),
                                )
                            });
                            if let Some(seq) = seq {
                                let state_timer = Arc::clone(&state);
                                let delay_ms =
                                    config.selection.debounce_ms.saturating_add(220).max(320);
                                tokio::spawn(async move {
                                    tokio::time::sleep(std::time::Duration::from_millis(delay_ms))
                                        .await;
                                    if let Ok(mut lock) = state_timer.lock() {
                                        lock.complete_scheduled_outside_dismiss(
                                            seq,
                                            std::time::Instant::now(),
                                        );
                                    }
                                });
                            }
                        } else if let Ok(mut lock) = state.lock() {
                            lock.handle_outside_click(pos, native_rect, std::time::Instant::now());
                        }
                    }
                }
            }
            Some(cmd) = rx_cmd.recv() => {
                match cmd {
                    GuiCommand::Translate { text, source_lang, target_lang, provider } => {
                        println!("[Yanxi] 交互翻译请求: \"{}\" -> [{} ({})]", text, provider, target_lang);
                        next_request_id += 1;
                        let req_id = next_request_id;
                        if let Ok(mut lock) = state.lock() {
                            lock.request_seq = req_id;
                            lock.request_repaint();
                        }
                        if let Some(handle) = latest_handle.take() {
                            handle.abort();
                        }
                        let state_task = Arc::clone(&state);
                        let cache_task = cache.clone();
                        let config_task = config.clone();
                        latest_handle = Some(tokio::spawn(async move {
                            perform_translation(
                                text,
                                source_lang,
                                target_lang,
                                provider,
                                config_task,
                                cache_task,
                                state_task,
                                req_id,
                            )
                            .await;
                        }));
                    }
                    GuiCommand::TestProvider { provider_name, provider_config } => {
                        println!("[Yanxi] 连通性测试: [{}]", provider_name);
                        let state_task = Arc::clone(&state);
                        tokio::spawn(async move {
                            let translator = create_translator(&provider_config);
                            let req = TranslationRequest::with_langs("Hello", "en", "zh-CN");
                            let feedback = match translator.translate(&req).await {
                                Ok(res) if res.is_success() => {
                                    (true, format!("连接正常 (耗时 {:.0}ms)", res.latency_ms))
                                }
                                Ok(res) => (false, format!("接口返回错误: {}", res.translated_text)),
                                Err(e) => (false, format!("连接失败: {e}")),
                            };
                            if let Ok(mut lock) = state_task.lock() {
                                lock.test_feedback = Some(feedback);
                                lock.request_repaint();
                            }
                        });
                    }
                    GuiCommand::PlayTts { text, accent } => {
                        println!("[Yanxi] 播放发音: \"{}\" ({:?})", text, accent);
                        tokio::spawn(async move {
                            crate::tts::play_pronunciation(&text, accent).await;
                        });
                    }
                    GuiCommand::SaveConfig(new_cfg) => {
                        let _ = new_cfg.save();
                        config = new_cfg;
                        println!("[Yanxi] 💾 配置已成功保存至磁盘");
                    }
                }
            }
        }
    }
}

async fn perform_translation(
    text: String,
    source_lang: String,
    target_lang: String,
    provider_name: String,
    config: AppConfig,
    cache: SQLiteCache,
    state: Arc<Mutex<SharedPopupState>>,
    request_id: u64,
) {
    let effective_target = if (source_lang == "auto" || source_lang == "zh-CN")
        && target_lang == "zh-CN"
        && text.chars().any(|c| ('\u{4e00}'..='\u{9fa5}').contains(&c))
    {
        "en"
    } else {
        target_lang.as_str()
    };
    let req = TranslationRequest::with_langs(&text, &source_lang, effective_target);

    if !is_latest(&state, request_id) {
        return;
    }

    // 1. 本地缓存秒开
    if let Some(cached) = cache.get(&req, &provider_name) {
        println!("[Yanxi] 本地缓存命中: \"{}\"", cached.translated_text);
        publish_status(&state, request_id, TranslationStatus::Success(cached));
        return;
    }

    // 2. 调用服务商接口
    let provider_cfg = match config.providers.get(&provider_name) {
        Some(cfg) => cfg,
        None => {
            publish_status(
                &state,
                request_id,
                TranslationStatus::Error {
                    original_text: text.clone(),
                    message: format!("找不到服务商 [{provider_name}] 配置"),
                },
            );
            return;
        }
    };

    let translator = create_translator(provider_cfg);
    let outcome = translator.translate(&req).await;

    if !is_latest(&state, request_id) {
        println!("[Yanxi] 丢弃过期翻译结果 (请求 #{request_id})");
        return;
    }

    match outcome {
        Ok(res) if res.is_success() => {
            println!(
                "[Yanxi] 翻译完成 [{:.0}ms]: \"{}\"",
                res.latency_ms, res.translated_text
            );
            cache.put(&res);
            publish_status(&state, request_id, TranslationStatus::Success(res));
        }
        Ok(res) => {
            eprintln!("[Yanxi] 翻译失败: {}", res.translated_text);
            publish_status(
                &state,
                request_id,
                TranslationStatus::Error {
                    original_text: text,
                    message: res.translated_text,
                },
            );
        }
        Err(e) => {
            eprintln!("[Yanxi] 请求异常: {e}");
            publish_status(
                &state,
                request_id,
                TranslationStatus::Error {
                    original_text: text,
                    message: e.to_string(),
                },
            );
        }
    }
}

fn publish_status(
    state: &Arc<Mutex<SharedPopupState>>,
    request_id: u64,
    status: TranslationStatus,
) {
    // Compare and publish under the same lock: an old request must not replace
    // the state after a newer request has become active.
    if let Ok(mut lock) = state.lock() {
        if lock.request_seq == request_id {
            lock.status = status;
            lock.request_repaint();
        }
    }
}

fn is_latest(state: &Arc<Mutex<SharedPopupState>>, request_id: u64) -> bool {
    state
        .lock()
        .map(|lock| lock.request_seq == request_id)
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shortcut_translates_selection_then_hides_and_reopens_same_word() {
        let mut config = AppConfig::default();
        config.ui.start_hidden = true;
        let mut state = SharedPopupState::new(&config);
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        state.tx_command = Some(tx);
        apply_hotkey_selection(&mut state, Some(" Hello world "));
        assert!(state.is_visible);
        assert_eq!(state.edit_text, "Hello world");
        assert!(
            matches!(rx.try_recv().unwrap(), GuiCommand::Translate { text, .. } if text == "Hello world")
        );
        apply_hotkey_selection(&mut state, Some("Hello world"));
        assert!(!state.is_visible);
        assert!(rx.try_recv().is_err());
        apply_hotkey_selection(&mut state, Some("Hello world"));
        assert!(state.is_visible);
        assert!(matches!(
            rx.try_recv().unwrap(),
            GuiCommand::Translate { .. }
        ));
    }

    #[test]
    fn empty_selection_only_toggles_and_oversized_selection_does_not_send_request() {
        let mut state = SharedPopupState::new(&AppConfig::default());
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        state.tx_command = Some(tx);
        state.is_visible = false;
        apply_hotkey_selection(&mut state, None);
        assert!(state.is_visible);
        assert!(state.edit_text.is_empty());
        assert!(rx.try_recv().is_err());
        apply_hotkey_selection(&mut state, Some(&"中".repeat(3001)));
        assert!(matches!(state.status, TranslationStatus::Error { .. }));
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn late_translation_does_not_replace_current_request() {
        let state = Arc::new(Mutex::new(SharedPopupState::new(&AppConfig::default())));
        state.lock().unwrap().request_seq = 2;
        let current = TranslationStatus::Loading {
            original_text: "latest".into(),
        };
        publish_status(&state, 2, current.clone());
        publish_status(
            &state,
            1,
            TranslationStatus::Error {
                original_text: "old".into(),
                message: "late error".into(),
            },
        );
        assert_eq!(state.lock().unwrap().status, current);
    }

    #[test]
    fn test_default_window_size_clamps() {
        assert_eq!(default_window_size(1366.0, 768.0), (420.0, 300.0));
        assert_eq!(default_window_size(1920.0, 1080.0), (576.0, 378.0));
        assert_eq!(default_window_size(2560.0, 1440.0), (760.0, 504.0));
        assert_eq!(default_window_size(3440.0, 1440.0), (760.0, 504.0));
        assert_eq!(default_window_size(800.0, 600.0), (420.0, 300.0));
        assert_eq!(default_window_size_for_dpi(2560.0, 1600.0, 2.0), (840.0, 600.0));
        assert_eq!(default_window_size_for_dpi(1920.0, 1080.0, 1.5), (630.0, 450.0));
    }

    #[test]
    fn load_app_icon_decodes_embedded_logo() {
        let icon = load_app_icon().expect("embedded icon.png should decode");
        assert_eq!(icon.width, 256);
        assert_eq!(icon.height, 256);
        assert_eq!(icon.rgba.len(), 256 * 256 * 4);
    }
}
