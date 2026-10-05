pub mod fonts;
pub mod popup;
pub mod state;

use std::sync::{Arc, Mutex};
use tokio::sync::mpsc::UnboundedReceiver;

use crate::cache::SQLiteCache;
use crate::config::{AppConfig, SelectionMode};
use crate::models::TranslationRequest;
use crate::selection::{create_selection_listener, start_global_hotkey_listener, SelectionEvent};
use crate::single_instance::{InstanceCheck, SingleInstance};
use crate::translator::create_translator;
use popup::PopupApp;
use state::{GuiCommand, SharedPopupState, TranslationStatus};

#[cfg(target_os = "linux")]
use x11rb::connection::Connection;
#[cfg(target_os = "linux")]
use x11rb::wrapper::ConnectionExt as _;

#[cfg(target_os = "linux")]
pub fn move_resize_x11(win: u32, x: i32, y: i32, w: u32, h: u32) {
    if let Ok((conn, screen_num)) = x11rb::connect(None) {
        use x11rb::protocol::xproto::{ClientMessageData, ClientMessageEvent, ConnectionExt, EventMask};
        let root = conn.setup().roots[screen_num].root;
        if let Ok(cookie) = conn.intern_atom(false, b"_NET_MOVERESIZE_WINDOW") {
            if let Ok(atom) = cookie.reply() {
                let flags = 0x0F01u32; // NorthWest + x + y + w + h
                let data = [flags, x.max(0) as u32, y.max(0) as u32, w, h];
                let event = ClientMessageEvent {
                    response_type: x11rb::protocol::xproto::CLIENT_MESSAGE_EVENT,
                    format: 32,
                    sequence: 0,
                    window: win,
                    type_: atom.atom,
                    data: ClientMessageData::from(data),
                };
                if let Ok(cookie) = conn.send_event(false, root, EventMask::SUBSTRUCTURE_REDIRECT | EventMask::SUBSTRUCTURE_NOTIFY, event) {
                    let _ = cookie.check();
                    let _ = conn.flush();
                    println!("[Yanxi X11] 📏 move_resize_x11 发送成功 0x{:08x}: pos=({}, {}), size={}x{}", win, x, y, w, h);
                }
            }
        }
    }
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
    let initial_hotkey = config.selection.hotkey.clone();
    start_global_hotkey_listener(initial_hotkey, rx_hotkey, move || {
        if let Ok(mut lock) = state_hotkey.lock() {
            lock.is_visible = !lock.is_visible;
            if lock.is_visible {
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
                                let win_w = if s_w > 2000.0 { 840.0 } else { 420.0 };
                                let win_h = if s_h > 1200.0 { 520.0 } else { 260.0 };
                                let target_pos = SharedPopupState::compute_target_pos(px, py, win_w, win_h, s_w, s_h);
                                lock.window_pos = Some(target_pos);
                                if let Some(win) = lock.x11_window {
                                    move_resize_x11(win, target_pos.0 as i32, target_pos.1 as i32, win_w as u32, win_h as u32);
                                }
                            }
                        }
                    }
                }
                lock.should_update_pos = true;
            }
            lock.request_repaint();
        }
    });

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
    let native_options = eframe::NativeOptions {
        renderer: eframe::Renderer::Glow,
        viewport: eframe::egui::ViewportBuilder::default()
            .with_inner_size([420.0, 260.0])
            .with_min_inner_size([320.0, 180.0])
            .with_decorations(false)
            .with_transparent(true)
            .with_always_on_top()
            .with_active(false)
            .with_resizable(true)
            .with_window_type(eframe::egui::X11WindowType::Utility),
        ..Default::default()
    };

    // 启动后异步增强 X11 窗口属性：设置 _NET_WM_STATE_SKIP_TASKBAR, SKIP_PAGER, ABOVE 并预置紧凑尺寸
    let state_x11 = Arc::clone(&state);
    #[cfg(target_os = "linux")]
    std::thread::spawn(move || {
        let Ok((conn, screen_num)) = x11rb::connect(None) else { return; };
        use x11rb::protocol::xproto::{AtomEnum, ConnectionExt, PropMode};
        let root = conn.setup().roots[screen_num].root;
        let screen_w = conn.setup().roots[screen_num].width_in_pixels as f32;
        let screen_h = conn.setup().roots[screen_num].height_in_pixels as f32;

        let Ok(net_wm_name) = conn.intern_atom(false, b"_NET_WM_NAME") else { return; };
        let Ok(net_wm_name_atom) = net_wm_name.reply() else { return; };
        let Ok(state_atom) = conn.intern_atom(false, b"_NET_WM_STATE") else { return; };
        let Ok(state_atom_r) = state_atom.reply() else { return; };
        let Ok(skip_taskbar) = conn.intern_atom(false, b"_NET_WM_STATE_SKIP_TASKBAR") else { return; };
        let Ok(skip_taskbar_r) = skip_taskbar.reply() else { return; };
        let Ok(skip_pager) = conn.intern_atom(false, b"_NET_WM_STATE_SKIP_PAGER") else { return; };
        let Ok(skip_pager_r) = skip_pager.reply() else { return; };
        let Ok(above) = conn.intern_atom(false, b"_NET_WM_STATE_ABOVE") else { return; };
        let Ok(above_r) = above.reply() else { return; };

        for _ in 0..50 {
            std::thread::sleep(std::time::Duration::from_millis(100));
            if let Ok(cookie) = conn.query_tree(root) {
                if let Ok(tree) = cookie.reply() {
                for win in tree.children {
                    if let Ok(name_prop) = conn.get_property(false, win, net_wm_name_atom.atom, AtomEnum::ANY, 0, 128) {
                        if let Ok(name_reply) = name_prop.reply() {
                            let name = String::from_utf8_lossy(&name_reply.value);
                            if name.contains("yanxi-popup") {
                                let states = [skip_taskbar_r.atom, skip_pager_r.atom, above_r.atom];
                                let _ = conn.change_property32(
                                    PropMode::APPEND,
                                    win,
                                    state_atom_r.atom,
                                    AtomEnum::ATOM,
                                    &states,
                                );
                                let _ = conn.flush();

                                if let Ok(mut lock) = state_x11.lock() {
                                    lock.x11_window = Some(win);
                                    lock.screen_size = (screen_w, screen_h);
                                }

                                let init_w = if screen_w > 2000.0 { 840 } else { 420 };
                                let init_h = if screen_h > 1200.0 { 520 } else { 260 };
                                let init_x = ((screen_w - init_w as f32) / 2.0).max(10.0) as i32;
                                let init_y = ((screen_h - init_h as f32) / 2.0).max(10.0) as i32;
                                move_resize_x11(win, init_x, init_y, init_w, init_h);

                                println!("[Yanxi GUI] 🪟 X11 浮窗属性增强就绪: Utility + SkipTaskbar + SkipPager + Above, 初始尺寸: {}x{}", init_w, init_h);
                                return;
                            }
                        }
                    }
                }
            }
        }
    }
});

    let state_app = Arc::clone(&state);
    eframe::run_native(
        "yanxi-popup",
        native_options,
        Box::new(move |cc| {
            // 使用清新优雅的明亮主题，圆润高对比
            cc.egui_ctx.set_visuals(eframe::egui::Visuals::light());
            fonts::configure_cjk_fonts(&cc.egui_ctx);
            if let Ok(mut lock) = state_app.lock() {
                lock.ctx = Some(cc.egui_ctx.clone());
            }
            Ok(Box::new(PopupApp::new(state_app, config)))
        }),
    )
    .map_err(|e| e.to_string())?;

    Ok(())
}

async fn handle_worker_events(
    mut rx_sel: UnboundedReceiver<SelectionEvent>,
    mut rx_cmd: UnboundedReceiver<GuiCommand>,
    state: Arc<Mutex<SharedPopupState>>,
    mut config: AppConfig,
    cache: SQLiteCache,
) {
    loop {
        tokio::select! {
            Some(event) = rx_sel.recv() => {
                match event {
                    SelectionEvent::Selected { text, pos } => {
                        let (mode, is_visible, is_pinned, s_w, s_h, x11_win) = {
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

                        let win_w = if s_w > 2000.0 { 840.0 } else { 420.0 };
                        let win_h = if s_h > 1200.0 { 520.0 } else { 260.0 };
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

                        perform_translation(&text, &source_lang, &target_lang, &provider_name, &config, &cache, &state).await;
                    }
                    SelectionEvent::Cleared { .. } => {
                        if let Ok(mut lock) = state.lock() {
                            // 仅在完全自动划选模式下，取消选区时自动收起
                            if lock.selection_mode == SelectionMode::Automatic && !lock.is_pinned && lock.is_visible {
                                lock.is_visible = false;
                                lock.request_repaint();
                            }
                        }
                    }
                }
            }
            Some(cmd) = rx_cmd.recv() => {
                match cmd {
                    GuiCommand::Translate { text, source_lang, target_lang, provider } => {
                        println!("[Yanxi GUI] 交互翻译请求: \"{}\" -> [{} ({})]", text, provider, target_lang);
                        perform_translation(&text, &source_lang, &target_lang, &provider, &config, &cache, &state).await;
                    }
                    GuiCommand::TestProvider { provider_name, provider_config } => {
                        println!("[Yanxi GUI] 连通性测试: [{}]", provider_name);
                        let translator = create_translator(&provider_config);
                        let req = TranslationRequest::with_langs("Hello", "en", "zh-CN");
                        match translator.translate(&req).await {
                            Ok(res) if res.is_success() => {
                                if let Ok(mut lock) = state.lock() {
                                    lock.test_feedback = Some((true, format!("✅ 连接正常 (耗时 {:.0}ms)", res.latency_ms)));
                                    lock.request_repaint();
                                }
                            }
                            Ok(res) => {
                                if let Ok(mut lock) = state.lock() {
                                    lock.test_feedback = Some((false, format!("❌ 接口返回错误: {}", res.translated_text)));
                                    lock.request_repaint();
                                }
                            }
                            Err(e) => {
                                if let Ok(mut lock) = state.lock() {
                                    lock.test_feedback = Some((false, format!("❌ 连接失败: {e}")));
                                    lock.request_repaint();
                                }
                            }
                        }
                    }
                    GuiCommand::PlayTts { text, accent } => {
                        println!("[Yanxi GUI] 🔊 播放发音: \"{}\" ({:?})", text, accent);
                        crate::tts::play_pronunciation(&text, accent).await;
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
    text: &str,
    source_lang: &str,
    target_lang: &str,
    provider_name: &str,
    config: &AppConfig,
    cache: &SQLiteCache,
    state: &Arc<Mutex<SharedPopupState>>,
) {
    let req = TranslationRequest::with_langs(text, source_lang, target_lang);

    // 1. 本地缓存秒开
    if let Some(cached) = cache.get(&req, provider_name) {
        println!("[Yanxi GUI] ⚡ 本地缓存命中: \"{}\"", cached.translated_text);
        if let Ok(mut lock) = state.lock() {
            lock.status = TranslationStatus::Success(cached);
            lock.request_repaint();
        }
        return;
    }

    // 2. 调用服务商接口
    let provider_cfg = match config.providers.get(provider_name) {
        Some(cfg) => cfg,
        None => {
            if let Ok(mut lock) = state.lock() {
                lock.status = TranslationStatus::Error {
                    original_text: text.to_string(),
                    message: format!("找不到服务商 [{provider_name}] 配置"),
                };
                lock.request_repaint();
            }
            return;
        }
    };

    let translator = create_translator(provider_cfg);
    match translator.translate(&req).await {
        Ok(res) if res.is_success() => {
            println!("[Yanxi GUI] ✅ 翻译完成 [{:.0}ms]: \"{}\"", res.latency_ms, res.translated_text);
            cache.put(&res);
            if let Ok(mut lock) = state.lock() {
                lock.status = TranslationStatus::Success(res);
                lock.request_repaint();
            }
        }
        Ok(res) => {
            eprintln!("[Yanxi GUI] ❌ 翻译失败: {}", res.translated_text);
            if let Ok(mut lock) = state.lock() {
                lock.status = TranslationStatus::Error {
                    original_text: text.to_string(),
                    message: res.translated_text,
                };
                lock.request_repaint();
            }
        }
        Err(e) => {
            eprintln!("[Yanxi GUI] ❌ 请求异常: {e}");
            if let Ok(mut lock) = state.lock() {
                lock.status = TranslationStatus::Error {
                    original_text: text.to_string(),
                    message: e.to_string(),
                };
                lock.request_repaint();
            }
        }
    }
}
