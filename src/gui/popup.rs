use eframe::egui;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::config::{AppConfig, ProviderConfig, SelectionMode};
use super::state::{GuiCommand, SharedPopupState, TranslationStatus};

pub struct PopupApp {
    state: Arc<Mutex<SharedPopupState>>,
    config: AppConfig,
    show_key_plain: bool,
    last_visible: Option<bool>,
}

impl PopupApp {
    pub fn new(state: Arc<Mutex<SharedPopupState>>, config: AppConfig) -> Self {
        Self {
            state,
            config,
            show_key_plain: false,
            last_visible: None,
        }
    }
}

impl eframe::App for PopupApp {
    fn clear_color(&self, _visuals: &egui::Visuals) -> [f32; 4] {
        [0.0, 0.0, 0.0, 0.0] // 保证完全无边框透明
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        // 全局 Esc 快捷键：随时收起浮窗
        if ui.input(|i| i.key_pressed(egui::Key::Escape)) {
            if let Ok(mut lock) = self.state.lock() {
                lock.is_visible = false;
                lock.show_settings = false;
            }
        }

        let mut lock = match self.state.lock() {
            Ok(guard) => guard,
            Err(_) => return,
        };

        // 仅在显隐状态发生变化时才发送 Visible 命令，杜绝 X11 每帧重绘与暴风式闪烁卡顿！
        if self.last_visible != Some(lock.is_visible) {
            ui.ctx().send_viewport_cmd(egui::ViewportCommand::Visible(lock.is_visible));
            self.last_visible = Some(lock.is_visible);
        }

        if !lock.is_visible {
            return;
        }

        // 处理位置更新与显隐控制
        if lock.should_update_pos {
            if let Some(pos) = lock.window_pos {
                ui.ctx().send_viewport_cmd(egui::ViewportCommand::OuterPosition(egui::pos2(pos.0, pos.1)));
            }
            lock.should_update_pos = false;
        }

        let is_pinned = lock.is_pinned;
        let show_settings = lock.show_settings;
        let mut active_provider = lock.active_provider.clone();
        let mut source_lang = lock.source_lang.clone();
        let mut target_lang = lock.target_lang.clone();
        let mut selection_mode = lock.selection_mode;
        let available_providers = lock.available_providers.clone();
        let status = lock.status.clone();
        let is_copied = lock
            .copy_feedback_time
            .map(|t| t.elapsed() < Duration::from_millis(1500))
            .unwrap_or(false);

        let mut trigger_retranslate = false;

        // 主玻璃拟态暗黑卡片
        egui::Frame::new()
            .fill(egui::Color32::from_rgba_premultiplied(22, 27, 34, 248))
            .stroke(egui::Stroke::new(1.0, egui::Color32::from_rgb(48, 54, 61)))
            .corner_radius(12.0)
            .inner_margin(12.0)
            .show(ui, |ui| {
                // ==================== 1. 顶部操作栏 (Header Bar) ====================
                let header_response = ui.horizontal(|ui| {
                    // 品牌标识
                    ui.colored_label(egui::Color32::from_rgb(88, 166, 255), "言蹊翻译");

                    // 1.1 提供商就地切换下拉选单 (Provider ComboBox)
                    let p_label = provider_badge(&active_provider);
                    egui::ComboBox::from_id_salt("header_provider_select")
                        .selected_text(egui::RichText::new(p_label).size(11.5))
                        .show_ui(ui, |ui| {
                            for p in &available_providers {
                                let badge = provider_badge(p);
                                if ui.selectable_value(&mut active_provider, p.clone(), badge).clicked() {
                                    trigger_retranslate = true;
                                }
                            }
                        });

                    // 1.2 语言方向切换 (Language Direction ComboBox)
                    let lang_text = format!("{} ⇄ {}", lang_display_name(&source_lang), lang_display_name(&target_lang));
                    egui::ComboBox::from_id_salt("header_lang_select")
                        .selected_text(egui::RichText::new(lang_text).size(11.0))
                        .show_ui(ui, |ui| {
                            let common_langs = [
                                ("zh-CN", "简体中文"),
                                ("en", "English"),
                                ("ja", "日本語"),
                                ("ko", "한국어"),
                                ("fr", "Français"),
                                ("de", "Deutsch"),
                                ("es", "Español"),
                                ("ru", "Русский"),
                            ];
                            for (code, name) in common_langs {
                                if ui.selectable_value(&mut target_lang, code.to_string(), name).clicked() {
                                    trigger_retranslate = true;
                                }
                            }
                        });

                    // 一键互换源语言与目标语言
                    if ui.small_button("⇄").on_hover_text("互换源语言与目标语言并重译").clicked() {
                        if source_lang == "auto" {
                            source_lang = target_lang.clone();
                            target_lang = "en".into();
                        } else {
                            std::mem::swap(&mut source_lang, &mut target_lang);
                        }
                        trigger_retranslate = true;
                    }

                    // 1.3 取词模式切换 (Selection Mode ComboBox)
                    let mode_badge = match selection_mode {
                        SelectionMode::Companion => "📖 伴随阅读",
                        SelectionMode::Automatic => "⚡ 划选即弹",
                        SelectionMode::Manual => "✋ 手动查词",
                    };
                    egui::ComboBox::from_id_salt("header_mode_select")
                        .selected_text(egui::RichText::new(mode_badge).size(11.0))
                        .show_ui(ui, |ui| {
                            ui.selectable_value(&mut selection_mode, SelectionMode::Companion, "📖 伴随阅读 (推荐，关闭时静默)");
                            ui.selectable_value(&mut selection_mode, SelectionMode::Automatic, "⚡ 划选即弹窗 (随时划选自动弹)");
                            ui.selectable_value(&mut selection_mode, SelectionMode::Manual, "✋ 手动模式 (划选不弹窗)");
                        });

                    // 右侧控制按钮
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        // 关闭按钮
                        if ui
                            .button(egui::RichText::new("✕").size(12.0).color(egui::Color32::from_rgb(139, 148, 158)))
                            .on_hover_text("收起浮窗 (仍在后台运行，按 Alt+D 随时重新呼出)")
                            .clicked()
                        {
                            lock.is_visible = false;
                            lock.show_settings = false;
                        }

                        // ⚙ 设置按钮
                        let settings_color = if show_settings {
                            egui::Color32::from_rgb(88, 166, 255)
                        } else {
                            egui::Color32::from_rgb(139, 148, 158)
                        };
                        if ui
                            .button(egui::RichText::new("⚙").size(12.0).color(settings_color))
                            .on_hover_text("管理服务商 API Key、模型与连通性测试")
                            .clicked()
                        {
                            lock.show_settings = !lock.show_settings;
                            if lock.show_settings {
                                let active_p = lock.active_provider.clone();
                                lock.load_provider_settings(&self.config, &active_p);
                            }
                        }

                        // 📌 图钉固定按钮
                        let pin_color = if is_pinned {
                            egui::Color32::from_rgb(235, 179, 56)
                        } else {
                            egui::Color32::from_rgb(110, 118, 129)
                        };
                        let pin_text = if is_pinned { "📌 已固定" } else { "📌 固定" };
                        if ui
                            .button(egui::RichText::new(pin_text).size(11.0).color(pin_color))
                            .on_hover_text("固定悬浮窗位置 (防止点击外部或选区清除时自动收起)")
                            .clicked()
                        {
                            lock.is_pinned = !is_pinned;
                        }
                    });
                });

                // 按住标题栏拖动整个窗口
                if header_response.response.interact(egui::Sense::drag()).dragged() {
                    ui.ctx().send_viewport_cmd(egui::ViewportCommand::StartDrag);
                }

                ui.add_space(4.0);
                ui.separator();
                ui.add_space(4.0);

                // 更新状态同步
                if active_provider != lock.active_provider {
                    lock.active_provider = active_provider.clone();
                    self.config.active_provider = active_provider;
                    let _ = self.config.save();
                }
                if source_lang != lock.source_lang {
                    lock.source_lang = source_lang.clone();
                    self.config.source_lang = source_lang;
                    let _ = self.config.save();
                }
                if target_lang != lock.target_lang {
                    lock.target_lang = target_lang.clone();
                    self.config.target_lang = target_lang;
                    let _ = self.config.save();
                }
                if selection_mode != lock.selection_mode {
                    lock.selection_mode = selection_mode;
                    self.config.selection.set_mode(selection_mode);
                    let _ = self.config.save();
                }

                // ==================== 2. 设置面板视图 (Settings View) ====================
                if lock.show_settings {
                    egui::Frame::new()
                        .fill(egui::Color32::from_rgba_premultiplied(13, 17, 23, 220))
                        .stroke(egui::Stroke::new(1.0, egui::Color32::from_rgb(48, 54, 61)))
                        .corner_radius(8.0)
                        .inner_margin(10.0)
                        .show(ui, |ui| {
                            ui.horizontal(|ui| {
                                ui.colored_label(egui::Color32::from_rgb(88, 166, 255), "⚙ 服务商设置中心");
                                ui.add_space(10.0);

                                let mut cur_sel_p = lock.settings_provider.clone();
                                egui::ComboBox::from_id_salt("settings_provider_dropdown")
                                    .selected_text(provider_badge(&cur_sel_p))
                                    .show_ui(ui, |ui| {
                                        for p in &available_providers {
                                            ui.selectable_value(&mut cur_sel_p, p.clone(), provider_badge(p));
                                        }
                                    });

                                if cur_sel_p != lock.settings_provider {
                                    lock.load_provider_settings(&self.config, &cur_sel_p);
                                }
                            });

                            ui.add_space(6.0);

                            if lock.settings_provider == "microsoft" {
                                ui.label(
                                    egui::RichText::new("💡 微软官方通道默认无需填写 API Key，开箱免配置即用。若您有 Azure 翻译专线 Key，可填入下方。")
                                        .size(11.0)
                                        .color(egui::Color32::from_rgb(139, 148, 158)),
                                );
                            }

                            // API Key 输入
                            ui.horizontal(|ui| {
                                ui.label(egui::RichText::new("API Key:").size(12.0));
                                ui.add(
                                    egui::TextEdit::singleline(&mut lock.settings_api_key)
                                        .password(!self.show_key_plain)
                                        .desired_width(220.0),
                                );
                                let eye = if self.show_key_plain { "👁 隐藏" } else { "👁 显示" };
                                if ui.small_button(eye).clicked() {
                                    self.show_key_plain = !self.show_key_plain;
                                }
                            });

                            // Base URL 输入
                            ui.horizontal(|ui| {
                                ui.label(egui::RichText::new("Base URL:").size(12.0));
                                ui.add(
                                    egui::TextEdit::singleline(&mut lock.settings_base_url)
                                        .desired_width(260.0),
                                );
                            });

                            // Model 名称输入
                            ui.horizontal(|ui| {
                                ui.label(egui::RichText::new("模型名称:").size(12.0));
                                ui.add(
                                    egui::TextEdit::singleline(&mut lock.settings_model)
                                        .desired_width(200.0),
                                );
                            });

                            ui.add_space(8.0);

                            // 底部操作按钮
                            ui.horizontal(|ui| {
                                // 连通性测试按钮
                                if ui.button("⚡ 测试连通性").on_hover_text("发送测试请求验证该服务商配置是否有效").clicked() {
                                    let p_name = lock.settings_provider.clone();
                                    let p_cfg = ProviderConfig {
                                        name: p_name.clone(),
                                        provider_type: if p_name == "microsoft" { "microsoft".into() } else { "openai_compatible".into() },
                                        base_url: lock.settings_base_url.clone(),
                                        api_key: lock.settings_api_key.clone(),
                                        model: lock.settings_model.clone(),
                                        timeout_seconds: 10.0,
                                        system_prompt: "You are a professional translator.".into(),
                                    };
                                    lock.test_feedback = Some((true, "正在连接测试中...".into()));
                                    if let Some(ref tx) = lock.tx_command {
                                        let _ = tx.send(GuiCommand::TestProvider {
                                            provider_name: p_name,
                                            provider_config: p_cfg,
                                        });
                                    }
                                }

                                // 保存配置按钮
                                if ui.button("💾 保存配置").clicked() {
                                    let p_name = lock.settings_provider.clone();
                                    let mut p_cfg = self.config.providers.get(&p_name).cloned().unwrap_or_default();
                                    p_cfg.api_key = lock.settings_api_key.clone();
                                    p_cfg.base_url = lock.settings_base_url.clone();
                                    p_cfg.model = lock.settings_model.clone();
                                    self.config.providers.insert(p_name, p_cfg);
                                    let _ = self.config.save();
                                    if let Some(ref tx) = lock.tx_command {
                                        let _ = tx.send(GuiCommand::SaveConfig(self.config.clone()));
                                    }
                                    lock.test_feedback = Some((true, "✅ 配置已保存成功！".into()));
                                }

                                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                    if ui.button("✕ 返回翻译").clicked() {
                                        lock.show_settings = false;
                                    }
                                });
                            });

                            // 连通性测试结果提示
                            if let Some((ok, ref msg)) = lock.test_feedback {
                                ui.add_space(4.0);
                                let color = if ok {
                                    egui::Color32::from_rgb(86, 211, 100)
                                } else {
                                    egui::Color32::from_rgb(248, 81, 73)
                                };
                                ui.colored_label(color, msg);
                            }
                        });

                    return;
                }

                // ==================== 3. 主翻译内容区域 (Translation Views) ====================
                // 3.1 原文交互卡片 (Original Text Card)
                egui::Frame::new()
                    .fill(egui::Color32::from_rgba_premultiplied(13, 17, 23, 200))
                    .corner_radius(8.0)
                    .inner_margin(8.0)
                    .show(ui, |ui| {
                        // 多行文本编辑与手动键入查词
                        let text_edit_response = ui.add(
                            egui::TextEdit::multiline(&mut lock.edit_text)
                                .hint_text("在此键入或粘贴文本，按回车 (Enter) 立即翻译...")
                                .desired_rows(2)
                                .desired_width(f32::INFINITY),
                        );

                        // 按下 Enter (非 Shift+Enter) 触发即时翻译
                        if text_edit_response.has_focus()
                            && ui.input(|i| i.key_pressed(egui::Key::Enter) && !i.modifiers.shift)
                        {
                            trigger_retranslate = true;
                        }

                        // 原文卡片底栏：字符统计 + 清空 + 翻译 + 复制
                        ui.horizontal(|ui| {
                            let char_count = lock.edit_text.chars().count();
                            ui.label(
                                egui::RichText::new(format!("{char_count} 字符"))
                                    .size(11.0)
                                    .color(egui::Color32::from_rgb(110, 118, 129)),
                            );

                            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                // 复制原文
                                if ui.small_button("📋 复制").on_hover_text("复制原文到剪贴板").clicked() {
                                    if let Ok(mut clipboard) = arboard::Clipboard::new() {
                                        let _ = clipboard.set_text(&lock.edit_text);
                                    }
                                }

                                // 翻译按钮
                                if ui.small_button("⚡ 翻译").on_hover_text("立即发起翻译 (Enter)").clicked() {
                                    trigger_retranslate = true;
                                }

                                // 清空按钮
                                if !lock.edit_text.is_empty() {
                                    if ui.small_button("🗑 清空").on_hover_text("清空输入内容").clicked() {
                                        lock.edit_text.clear();
                                        lock.status = TranslationStatus::Idle;
                                    }
                                }
                            });
                        });
                    });

                ui.add_space(6.0);

                // 3.2 译文卡片展示 (Translation Card)
                match status {
                    TranslationStatus::Idle => {
                        ui.label(
                            egui::RichText::new("等待划词或输入... 在任意窗口划选，或直接在上方框中输入按回车")
                                .size(13.0)
                                .color(egui::Color32::from_rgb(139, 148, 158)),
                        );
                    }
                    TranslationStatus::Loading { .. } => {
                        ui.horizontal(|ui| {
                            ui.spinner();
                            ui.colored_label(egui::Color32::from_rgb(88, 166, 255), "正在极速翻译中...");
                        });
                        ui.ctx().request_repaint_after(Duration::from_millis(80));
                    }
                    TranslationStatus::Success(res) => {
                        egui::Frame::new()
                            .fill(egui::Color32::from_rgba_premultiplied(28, 33, 40, 240))
                            .stroke(egui::Stroke::new(1.0, egui::Color32::from_rgb(56, 139, 253)))
                            .corner_radius(8.0)
                            .inner_margin(10.0)
                            .show(ui, |ui| {
                                ui.horizontal(|ui| {
                                    ui.label(
                                        egui::RichText::new(format!("👉 {}", res.translated_text))
                                            .size(15.5)
                                            .strong()
                                            .color(egui::Color32::from_rgb(86, 211, 100)),
                                    );

                                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                        let btn_label = if is_copied { "✅ 已复制" } else { "📋 复制" };
                                        if ui.button(egui::RichText::new(btn_label).size(11.0)).clicked() {
                                            if let Ok(mut clipboard) = arboard::Clipboard::new() {
                                                let _ = clipboard.set_text(&res.translated_text);
                                                lock.copy_feedback_time = Some(std::time::Instant::now());
                                            }
                                        }
                                    });
                                });

                                if let Some(ref ph) = res.phonetic {
                                    ui.add_space(4.0);
                                    ui.label(
                                        egui::RichText::new(format!("拼音/注音: {ph}"))
                                            .size(12.0)
                                            .color(egui::Color32::from_rgb(227, 179, 65)),
                                    );
                                }
                            });

                        ui.add_space(6.0);

                        // 3.3 底部延迟与缓存状态
                        ui.horizontal(|ui| {
                            if res.from_cache {
                                ui.label(
                                    egui::RichText::new("⚡ 本地缓存秒开 (<5ms)")
                                        .size(11.0)
                                        .color(egui::Color32::from_rgb(235, 179, 56)),
                                );
                            } else {
                                ui.label(
                                    egui::RichText::new(format!("耗时: {:.0}ms", res.latency_ms))
                                        .size(11.0)
                                        .color(egui::Color32::from_rgb(110, 118, 129)),
                                );
                            }
                        });
                    }
                    TranslationStatus::Error { message, .. } => {
                        egui::Frame::new()
                            .fill(egui::Color32::from_rgba_premultiplied(40, 20, 20, 200))
                            .stroke(egui::Stroke::new(1.0, egui::Color32::from_rgb(248, 81, 73)))
                            .corner_radius(8.0)
                            .inner_margin(10.0)
                            .show(ui, |ui| {
                                ui.colored_label(
                                    egui::Color32::from_rgb(248, 81, 73),
                                    format!("❌ 翻译异常: {message}"),
                                );
                                ui.add_space(6.0);
                                ui.horizontal(|ui| {
                                    if ui.button("🔄 重试").clicked() {
                                        trigger_retranslate = true;
                                    }
                                    if ui.button("⚙ 打开服务设置").clicked() {
                                        lock.show_settings = true;
                                        let active_p = lock.active_provider.clone();
                                        lock.load_provider_settings(&self.config, &active_p);
                                    }
                                });
                            });
                    }
                }
            });

        // 执行重译请求
        if trigger_retranslate {
            let text_to_trans = lock.edit_text.clone();
            lock.trigger_translate(&text_to_trans);
        }
    }
}

fn provider_badge(p: &str) -> &'static str {
    match p {
        "microsoft" => "🌐 微软官方免Key",
        "deepseek" => "🤖 DeepSeek",
        "openai" => "⚡ OpenAI",
        "zhipu" => "✨ 智谱GLM",
        "custom" => "🦙 本地Ollama",
        _ => "🔧 自定义",
    }
}

fn lang_display_name(lang: &str) -> &'static str {
    match lang {
        "auto" => "自动识别",
        "zh-CN" | "zh" => "中文",
        "en" => "英语",
        "ja" => "日语",
        "ko" => "韩语",
        "fr" => "法语",
        "de" => "德语",
        "es" => "西语",
        "ru" => "俄语",
        _ => "目标语",
    }
}
