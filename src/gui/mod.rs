pub mod fonts;
pub mod popup;
pub mod state;

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
pub fn move_resize_x11(win: u32, x: i32, y: i32, w: u32, h: u32) {
    X11_CONN.with(|cell| {
        let mut opt = cell.borrow_mut();
        if opt.is_none() {
            if let Ok((conn, screen_num)) = x11rb::connect(None) {
                use x11rb::protocol::xproto::ConnectionExt;
                let atom_val =
                    if let Ok(cookie) = conn.intern_atom(false, b"_NET_MOVERESIZE_WINDOW") {
                        cookie.reply().map(|r| r.atom).ok()
                    } else {
                        None
                    };
                if let Some(atom) = atom_val {
                    *opt = Some((conn, screen_num, atom));
                }
            }
        }
        if let Some((conn, screen_num, atom)) = opt.as_ref() {
            use x11rb::protocol::xproto::{
                ClientMessageData, ClientMessageEvent, ConnectionExt, EventMask,
            };
            let root = conn.setup().roots[*screen_num].root;
            let flags = 0x0F01u32; // NorthWest + x + y + w + h
            let data = [flags, x.max(0) as u32, y.max(0) as u32, w, h];
            let event = ClientMessageEvent {
                response_type: x11rb::protocol::xproto::CLIENT_MESSAGE_EVENT,
                format: 32,
                sequence: 0,
                window: win,
                type_: *atom,
                data: ClientMessageData::from(data),
            };
            if let Ok(cookie) = conn.send_event(
                false,
                root,
                EventMask::SUBSTRUCTURE_REDIRECT | EventMask::SUBSTRUCTURE_NOTIFY,
                event,
            ) {
                let _ = cookie.check();
                let _ = conn.flush();
            }
        }
    });
}

#[cfg(target_os = "linux")]
pub fn get_window_position_x11(win: u32) -> Option<(i32, i32)> {
    if let Ok((conn, screen_num)) = x11rb::connect(None) {
        use x11rb::protocol::xproto::ConnectionExt;
        let root = conn.setup().roots[screen_num].root;
        if let Ok(cookie) = conn.translate_coordinates(win, root, 0, 0) {
            if let Ok(reply) = cookie.reply() {
                return Some((reply.dst_x as i32, reply.dst_y as i32));
            }
        }
    }
    None
}

#[cfg(target_os = "linux")]
/// Clear WM window states that override an explicit size (maximized/fullscreen).
#[cfg(target_os = "linux")]
pub fn clear_maximized_state_x11(win: u32) {
    use x11rb::protocol::xproto::{
        ClientMessageData, ClientMessageEvent, ConnectionExt, EventMask,
    };
    let Ok((conn, screen_num)) = x11rb::connect(None) else {
        return;
    };
    let root = conn.setup().roots[screen_num].root;
    let Ok(state) = conn.intern_atom(false, b"_NET_WM_STATE").map(|c| c.reply()) else {
        return;
    };
    let Ok(state) = state else {
        return;
    };
    for name in [
        b"_NET_WM_STATE_MAXIMIZED_VERT".as_slice(),
        b"_NET_WM_STATE_MAXIMIZED_HORZ".as_slice(),
        b"_NET_WM_STATE_FULLSCREEN".as_slice(),
    ] {
        let Ok(atom) = conn.intern_atom(false, name).map(|c| c.reply()) else {
            continue;
        };
        let Ok(atom) = atom else {
            continue;
        };
        let event = ClientMessageEvent {
            response_type: x11rb::protocol::xproto::CLIENT_MESSAGE_EVENT,
            format: 32,
            sequence: 0,
            window: win,
            type_: state.atom,
            data: ClientMessageData::from([0u32, atom.atom, 0, 1, 0]),
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

/// State geometry is always in physical pixels. Do not send a second logical
/// move after a native X11 move: mixed units make windows jump at 2x scaling.
pub fn apply_window_geometry(
    ctx: &eframe::egui::Context,
    window: Option<u32>,
    pos: (f32, f32),
    size: (f32, f32),
) {
    #[cfg(target_os = "linux")]
    if let Some(window) = window {
        move_resize_x11(
            window,
            pos.0.round() as i32,
            pos.1.round() as i32,
            size.0.round() as u32,
            size.1.round() as u32,
        );
        return;
    }
    let scale = ctx.pixels_per_point();
    ctx.send_viewport_cmd(eframe::egui::ViewportCommand::OuterPosition(
        eframe::egui::pos2(pos.0 / scale, pos.1 / scale),
    ));
    ctx.send_viewport_cmd(eframe::egui::ViewportCommand::InnerSize(
        eframe::egui::vec2(size.0 / scale, size.1 / scale),
    ));
}

pub fn default_window_size(screen_w: f32, screen_h: f32) -> (f32, f32) {
    let w = (screen_w * 0.30).clamp(420.0, 760.0).round();
    let h = (screen_h * 0.35).clamp(300.0, 560.0).round();
    (w, h)
}

pub fn run_gui() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    // 1. 单实例进程保护 (Single Instance Lock)
    let instance_check = match SingleInstance::check("yanxi-gui") {
        Ok(check) => check,
        Err(e) => {
            eprintln!("[Yanxi GUI] ⚠️ 进程锁初始化提示: {e}");
            return Ok(());
        }
    };

    let mut single_instance = match instance_check {
        InstanceCheck::Primary(inst) => inst,
        InstanceCheck::AlreadyRunning => {
            println!("[Yanxi GUI] ⚡ 已存在正在运行的言蹊翻译实例，已发送指令唤醒现有悬浮窗。");
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
        println!("[Yanxi GUI] 🔔 收到外部进程激活唤醒指令");
        if let Ok(mut lock) = state_ipc.lock() {
            lock.is_visible = true;
            lock.should_update_pos = true;
            lock.request_repaint();
        }
    });

    // 5. 注册 X11 全局快捷键监听 (默认 Alt + Q，支持动态热重载)
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
            if let Ok(mut lock) = state_hotkey.lock() {
                apply_hotkey_selection(&mut lock, selection.as_deref());
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
                                        move_resize_x11(
                                            win,
                                            target_pos.0 as i32,
                                            target_pos.1 as i32,
                                            win_w as u32,
                                            win_h as u32,
                                        );
                                    }
                                }
                            }
                        }
                    }
                    lock.should_update_pos = true;
                }
                lock.request_repaint();
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
                    eprintln!("[Yanxi GUI] 无法创建 Tokio 异步运行时: {e}");
                    return;
                }
            };

            rt.block_on(async {
                handle_worker_events(rx_sel, rx_cmd, state_clone, config_clone, cache_clone).await;
            });
        })?;

    // 7. 在主线程启动 eframe 原生无边框透明视口 (使用 OpenGL Glow 后端，配合 Utility 窗口类型消除 X11 屏闪)
    #[cfg(target_os = "linux")]
    let (init_w, init_h) = x11rb::connect(None)
        .ok()
        .map(|(conn, screen_num)| {
            let root = &conn.setup().roots[screen_num];
            default_window_size(root.width_in_pixels as f32, root.height_in_pixels as f32)
        })
        .unwrap_or((500.0, 320.0));
    #[cfg(not(target_os = "linux"))]
    let (init_w, init_h) = (500.0, 320.0);

    if let Ok(mut lock) = state.lock() {
        lock.window_size = (init_w, init_h);
    }

    let native_options = eframe::NativeOptions {
        renderer: eframe::Renderer::Glow,
        viewport: eframe::egui::ViewportBuilder::default()
            .with_inner_size([init_w, init_h])
            .with_min_inner_size([380.0, 220.0])
            .with_decorations(false)
            .with_transparent(true)
            .with_always_on_top()
            .with_active(false)
            .with_resizable(true)
            .with_window_type(eframe::egui::X11WindowType::Utility),
        ..Default::default()
    };

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
                apply_popup_window_role(win);
                center_popup_window(win);
                if let Ok(mut lock) = state_app.lock() {
                    lock.x11_window = Some(win);
                }
            }
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

/// Mark the window as a non-taskbar popup and keep it above normal windows.
#[cfg(target_os = "linux")]
fn apply_popup_window_role(win: u32) {
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
    let atoms: Vec<u32> = [
        "_NET_WM_STATE_SKIP_TASKBAR",
        "_NET_WM_STATE_SKIP_PAGER",
        "_NET_WM_STATE_ABOVE",
    ]
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
    match selection {
        Some(text) => match crate::selection::validate_selection_text(text) {
            Ok(clean) if state.is_visible && clean == state.edit_text && !state.show_settings => {
                state.is_visible = false;
                state.show_settings = false;
            }
            Ok(clean) => {
                state.is_visible = true;
                state.show_settings = false;
                state.edit_text = clean.clone();
                state.trigger_translate(&clean);
            }
            Err(message) => {
                state.is_visible = true;
                state.show_settings = false;
                state.status = TranslationStatus::Error {
                    original_text: String::new(),
                    message,
                };
            }
        },
        None => {
            state.is_visible = !state.is_visible;
            if !state.is_visible {
                state.show_settings = false;
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
                        let (mode, is_visible, is_pinned, s_w, s_h, x11_win, window_size) = {
                            let lock = match state.lock() {
                                Ok(l) => l,
                                Err(_) => continue,
                            };
                            (
                                lock.selection_mode,
                                lock.is_visible,
                                lock.is_pinned,
                                lock.screen_size.0,
                                lock.screen_size.1,
                                lock.x11_window,
                                lock.window_size,
                            )
                        };

                        // 1. 取词模式策略拦截
                        if mode == SelectionMode::Manual {
                            // 手动模式：划选不触发弹窗
                            continue;
                        }
                        if mode == SelectionMode::Companion && !is_visible && !is_pinned {
                            // 伴随阅读模式：悬浮窗未打开且未固定，静默不打扰
                            continue;
                        }

                        let (win_w, win_h) = window_size;
                        let target_pos = SharedPopupState::compute_target_pos(pos.0, pos.1, win_w, win_h, s_w, s_h);
                        println!("[Yanxi GUI] 划词事件触发 @ ({}, {}): \"{}\"", pos.0, pos.1, text);

                        let (provider_name, source_lang, target_lang) = {
                            let mut lock = match state.lock() {
                                Ok(l) => l,
                                Err(_) => continue,
                            };
                            lock.cursor_pos = pos;
                            if !lock.is_pinned {
                                lock.window_pos = Some(target_pos);
                                lock.should_update_pos = true;
                                #[cfg(target_os = "linux")]
                                if let Some(win) = x11_win {
                                    move_resize_x11(win, target_pos.0 as i32, target_pos.1 as i32, win_w as u32, win_h as u32);
                                }
                            }
                            lock.is_visible = true;
                            lock.edit_text = text.clone();
                            lock.status = TranslationStatus::Loading {
                                original_text: text.clone(),
                            };
                            lock.request_repaint();
                            (
                                lock.active_provider.clone(),
                                lock.source_lang.clone(),
                                lock.target_lang.clone(),
                            )
                        };

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
                        // 用户可通过快捷键 (默认 Alt+Q) 或右上角 "×" 轻松收起。
                    }
                }
            }
            Some(cmd) = rx_cmd.recv() => {
                match cmd {
                    GuiCommand::Translate { text, source_lang, target_lang, provider } => {
                        println!("[Yanxi GUI] 交互翻译请求: \"{}\" -> [{} ({})]", text, provider, target_lang);
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
                        println!("[Yanxi GUI] 连通性测试: [{}]", provider_name);
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
                        println!("[Yanxi GUI] 播放发音: \"{}\" ({:?})", text, accent);
                        tokio::spawn(async move {
                            crate::tts::play_pronunciation(&text, accent).await;
                        });
                    }
                    GuiCommand::SaveConfig(new_cfg) => {
                        let _ = new_cfg.save();
                        config = new_cfg;
                        println!("[Yanxi GUI] 💾 配置已成功保存至磁盘");
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
        println!("[Yanxi GUI] 本地缓存命中: \"{}\"", cached.translated_text);
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
        println!("[Yanxi GUI] 丢弃过期翻译结果 (请求 #{request_id})");
        return;
    }

    match outcome {
        Ok(res) if res.is_success() => {
            println!(
                "[Yanxi GUI] 翻译完成 [{:.0}ms]: \"{}\"",
                res.latency_ms, res.translated_text
            );
            cache.put(&res);
            publish_status(&state, request_id, TranslationStatus::Success(res));
        }
        Ok(res) => {
            eprintln!("[Yanxi GUI] 翻译失败: {}", res.translated_text);
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
            eprintln!("[Yanxi GUI] 请求异常: {e}");
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
    }
}
