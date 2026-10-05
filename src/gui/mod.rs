pub mod popup;
pub mod state;

use std::sync::{Arc, Mutex};
use tokio::sync::mpsc::UnboundedReceiver;

use crate::cache::SQLiteCache;
use crate::config::AppConfig;
use crate::models::TranslationRequest;
use crate::selection::{create_selection_listener, SelectionEvent};
use crate::translator::create_translator;
use popup::PopupApp;
use state::{SharedPopupState, TranslationStatus};

pub fn run_gui() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let config = AppConfig::load();
    let cache = SQLiteCache::new()?;

    let state = Arc::new(Mutex::new(SharedPopupState::new(
        config.active_provider.clone(),
        config.target_lang.clone(),
    )));

    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    let listener = create_selection_listener(config.selection.debounce_ms, 1, 3000);
    listener.start(tx)?;

    // 启动独立后台异步运行时处理网络翻译与缓存读取
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
                handle_selection_events(rx, state_clone, config_clone, cache_clone).await;
            });
        })?;

    // 在主线程启动 eframe 原生无边框透明视口
    let native_options = eframe::NativeOptions {
        viewport: eframe::egui::ViewportBuilder::default()
            .with_inner_size([420.0, 240.0])
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
            if let Ok(mut lock) = state_app.lock() {
                lock.ctx = Some(cc.egui_ctx.clone());
            }
            Ok(Box::new(PopupApp::new(state_app)))
        }),
    )?;

    Ok(())
}

async fn handle_selection_events(
    mut rx: UnboundedReceiver<SelectionEvent>,
    state: Arc<Mutex<SharedPopupState>>,
    config: AppConfig,
    cache: SQLiteCache,
) {
    while let Some(event) = rx.recv().await {
        match event {
            SelectionEvent::Selected { text, pos } => {
                let target_pos = SharedPopupState::compute_target_pos(pos.0, pos.1, 420.0, 240.0);
                println!("[Yanxi GUI] 划词事件触发 @ ({}, {}): \"{}\"", pos.0, pos.1, text);

                let (provider_name, source_lang, target_lang) = {
                    let mut lock = match state.lock() {
                        Ok(l) => l,
                        Err(_) => continue,
                    };
                    lock.cursor_pos = pos;
                    lock.window_pos = Some(target_pos);
                    lock.should_update_pos = true;
                    lock.is_visible = true;
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

                let req = TranslationRequest::with_langs(&text, &source_lang, &target_lang);

                // 1. 尝试缓存优先秒开
                if let Some(cached) = cache.get(&req, &provider_name) {
                    println!("[Yanxi GUI] ⚡ 本地缓存命中: \"{}\"", cached.translated_text);
                    if let Ok(mut lock) = state.lock() {
                        lock.status = TranslationStatus::Success(cached);
                        lock.request_repaint();
                    }
                    continue;
                }

                // 2. 缓存未命中，调用网络接口
                let provider_cfg = match config.providers.get(&provider_name) {
                    Some(cfg) => cfg,
                    None => {
                        if let Ok(mut lock) = state.lock() {
                            lock.status = TranslationStatus::Error {
                                original_text: text.clone(),
                                message: format!("找不到服务商 [{provider_name}] 配置"),
                            };
                            lock.request_repaint();
                        }
                        continue;
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
                                original_text: text.clone(),
                                message: res.translated_text,
                            };
                            lock.request_repaint();
                        }
                    }
                    Err(e) => {
                        eprintln!("[Yanxi GUI] ❌ 请求异常: {e}");
                        if let Ok(mut lock) = state.lock() {
                            lock.status = TranslationStatus::Error {
                                original_text: text.clone(),
                                message: e.to_string(),
                            };
                            lock.request_repaint();
                        }
                    }
                }
            }
            SelectionEvent::Cleared { .. } => {
                if let Ok(mut lock) = state.lock() {
                    // 若未钉住浮窗，取消选区时自动收起
                    if !lock.is_pinned && lock.is_visible {
                        lock.is_visible = false;
                        lock.request_repaint();
                    }
                }
            }
        }
    }
}
