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

    // 5. 注册 X11 全局快捷键监听 (Alt + D / Alt + Q 开关悬浮窗)
    let state_hotkey = Arc::clone(&state);
    start_global_hotkey_listener(move || {
        if let Ok(mut lock) = state_hotkey.lock() {
            lock.is_visible = !lock.is_visible;
            if lock.is_visible {
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

    // 7. 在主线程启动 eframe 原生无边框透明视口
    let native_options = eframe::NativeOptions {
        viewport: eframe::egui::ViewportBuilder::default()
            .with_inner_size([440.0, 260.0])
            .with_decorations(false)
            .with_transparent(true)
            .with_always_on_top()
            .with_resizable(true),
        ..Default::default()
    };

    let state_app = Arc::clone(&state);
    eframe::run_native(
        "yanxi-popup",
        native_options,
        Box::new(move |cc| {
            fonts::configure_cjk_fonts(&cc.egui_ctx);
            if let Ok(mut lock) = state_app.lock() {
                lock.ctx = Some(cc.egui_ctx.clone());
            }
            Ok(Box::new(PopupApp::new(state_app, config)))
        }),
    )?;

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
                        let (mode, is_visible, is_pinned) = {
                            let lock = match state.lock() {
                                Ok(l) => l,
                                Err(_) => continue,
                            };
                            (lock.selection_mode, lock.is_visible, lock.is_pinned)
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

                        let target_pos = SharedPopupState::compute_target_pos(pos.0, pos.1, 440.0, 260.0);
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
