use eframe::egui;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::config::{AppConfig, ProviderConfig, SelectionMode};
use crate::tts::{has_english_text, Accent};
use super::state::{GuiCommand, SharedPopupState, TranslationStatus};

pub struct PopupApp {
    state: Arc<Mutex<SharedPopupState>>,
    config: AppConfig,
    show_key_plain: bool,
    last_visible: Option<bool>,
    is_resizing_splitter: bool,
}

impl PopupApp {
    pub fn new(state: Arc<Mutex<SharedPopupState>>, config: AppConfig) -> Self {
        Self {
            state,
            config,
            show_key_plain: false,
            last_visible: None,
            is_resizing_splitter: false,
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

        // 处理鼠标划词时触发的位置更新与显隐控制
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

        // 8向窗口缩放探测 (边缘 8px，角落 14px)
        let outer_rect = ui.max_rect();
        if let Some(pointer_pos) = ui.input(|i| i.pointer.hover_pos()) {
            let margin = 8.0;
            let corner = 14.0;
            let left = pointer_pos.x <= outer_rect.min.x + margin;
            let right = pointer_pos.x >= outer_rect.max.x - margin;
            let top = pointer_pos.y <= outer_rect.min.y + margin;
            let bottom = pointer_pos.y >= outer_rect.max.y - margin;

            let c_left = pointer_pos.x <= outer_rect.min.x + corner;
            let c_right = pointer_pos.x >= outer_rect.max.x - corner;
            let c_top = pointer_pos.y <= outer_rect.min.y + corner;
            let c_bottom = pointer_pos.y >= outer_rect.max.y - corner;

            let resize_dir = if c_top && c_left {
                Some(egui::viewport::ResizeDirection::NorthWest)
            } else if c_top && c_right {
                Some(egui::viewport::ResizeDirection::NorthEast)
            } else if c_bottom && c_left {
                Some(egui::viewport::ResizeDirection::SouthWest)
            } else if c_bottom && c_right {
                Some(egui::viewport::ResizeDirection::SouthEast)
            } else if top {
                Some(egui::viewport::ResizeDirection::North)
            } else if bottom {
                Some(egui::viewport::ResizeDirection::South)
            } else if left {
                Some(egui::viewport::ResizeDirection::West)
            } else if right {
                Some(egui::viewport::ResizeDirection::East)
            } else {
                None
            };

            if let Some(dir) = resize_dir {
                let cursor_icon = match dir {
                    egui::viewport::ResizeDirection::NorthWest => egui::CursorIcon::ResizeNorthWest,
                    egui::viewport::ResizeDirection::NorthEast => egui::CursorIcon::ResizeNorthEast,
                    egui::viewport::ResizeDirection::SouthWest => egui::CursorIcon::ResizeSouthWest,
                    egui::viewport::ResizeDirection::SouthEast => egui::CursorIcon::ResizeSouthEast,
                    egui::viewport::ResizeDirection::North => egui::CursorIcon::ResizeNorth,
                    egui::viewport::ResizeDirection::South => egui::CursorIcon::ResizeSouth,
                    egui::viewport::ResizeDirection::West => egui::CursorIcon::ResizeWest,
                    egui::viewport::ResizeDirection::East => egui::CursorIcon::ResizeEast,
                };
                ui.ctx().set_cursor_icon(cursor_icon);

                if ui.input(|i| i.pointer.any_pressed()) {
                    ui.ctx().send_viewport_cmd(egui::ViewportCommand::BeginResize(dir));
                }
            }
        }

        // ==================== 主悬浮窗外部浅色容器 ====================
        egui::Frame::new()
            .fill(egui::Color32::from_rgb(246, 248, 250)) // 优雅柔和的浅灰背景 (#f6f8fa)
            .stroke(egui::Stroke::new(1.0, egui::Color32::from_rgb(208, 215, 222))) // 细致外边框 (#d0d7de)
            .corner_radius(12.0)
            .inner_margin(10.0)
            .show(ui, |ui| {
                // ==================== 1. 顶部操作栏 (Header Bar) ====================
                let header_response = ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing = egui::vec2(6.0, 0.0);

                    // 1.1 提供商就地切换下拉选单 (微软翻译 / DeepSeek / OpenAI / 智谱 / Ollama)
                    let p_label = provider_badge(&active_provider);
                    egui::ComboBox::from_id_salt("header_provider_select")
                        .selected_text(egui::RichText::new(p_label).size(12.0).color(egui::Color32::from_rgb(36, 41, 47)))
                        .show_ui(ui, |ui| {
                            for p in &available_providers {
                                let badge = provider_badge(p);
                                if ui.selectable_value(&mut active_provider, p.clone(), badge).clicked() {
                                    trigger_retranslate = true;
                                }
                            }
                        });

                    // 1.2 语言方向切换 (自动 ⇄ 中)
                    let lang_text = format!("{} ⇄ {}", lang_display_short(&source_lang), lang_display_short(&target_lang));
                    egui::ComboBox::from_id_salt("header_lang_select")
                        .selected_text(egui::RichText::new(lang_text).size(12.0).color(egui::Color32::from_rgb(36, 41, 47)))
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
                            ui.separator();
                            if ui.button("⇄ 互换语种").clicked() {
                                if source_lang == "auto" {
                                    source_lang = target_lang.clone();
                                    target_lang = "en".into();
                                } else {
                                    std::mem::swap(&mut source_lang, &mut target_lang);
                                }
                                trigger_retranslate = true;
                            }
                        });

                    // 弹性空白填充，将右侧控制按钮顶至右端
                    let available_w = ui.available_width();
                    let right_tools_w = 210.0;
                    if available_w > right_tools_w {
                        ui.add_space(available_w - right_tools_w);
                    }

                    // 1.3 取词模式切换 (划选即翻译 / 伴随阅读 / 手动模式)
                    let mode_badge = match selection_mode {
                        SelectionMode::Automatic => "划选即翻译",
                        SelectionMode::Companion => "伴随阅读",
                        SelectionMode::Manual => "手动模式",
                    };
                    egui::ComboBox::from_id_salt("header_mode_select")
                        .selected_text(egui::RichText::new(mode_badge).size(12.0).color(egui::Color32::from_rgb(36, 41, 47)))
                        .show_ui(ui, |ui| {
                            ui.selectable_value(&mut selection_mode, SelectionMode::Automatic, "划选即翻译 (任意划选自动弹窗)");
                            ui.selectable_value(&mut selection_mode, SelectionMode::Companion, "伴随阅读 (推荐，关闭时静默)");
                            ui.selectable_value(&mut selection_mode, SelectionMode::Manual, "手动模式 (仅按快捷键弹窗)");
                        });

                    // 1.4 📌 图钉固定按钮 (独立圆角卡扣式设计)
                    let pin_bg = if is_pinned {
                        egui::Color32::from_rgb(226, 232, 240)
                    } else {
                        egui::Color32::WHITE
                    };
                    let pin_stroke = if is_pinned {
                        egui::Stroke::new(1.2, egui::Color32::from_rgb(239, 68, 68))
                    } else {
                        egui::Stroke::new(1.0, egui::Color32::from_rgb(208, 215, 222))
                    };

                    let pin_btn = egui::Button::new(egui::RichText::new("📌").size(12.5))
                        .fill(pin_bg)
                        .stroke(pin_stroke)
                        .corner_radius(6.0);
                    if ui.add(pin_btn).on_hover_text("固定悬浮窗位置 (防止失焦自动收起)").clicked() {
                        lock.is_pinned = !is_pinned;
                    }

                    // 1.5 ⚙ 设置按钮
                    let settings_color = if show_settings {
                        egui::Color32::from_rgb(37, 99, 235)
                    } else {
                        egui::Color32::from_rgb(87, 96, 106)
                    };
                    if ui
                        .button(egui::RichText::new("⚙").size(14.0).color(settings_color))
                        .on_hover_text("管理服务商 API Key、自定义快捷键与参数设置")
                        .clicked()
                    {
                        lock.show_settings = !lock.show_settings;
                        if lock.show_settings {
                            let active_p = lock.active_provider.clone();
                            lock.load_provider_settings(&self.config, &active_p);
                        }
                    }

                    // 1.6 ✕ 关闭收起按钮
                    if ui
                        .button(egui::RichText::new("✕").size(13.0).color(egui::Color32::from_rgb(87, 96, 106)))
                        .on_hover_text("收起浮窗 (后台常驻，按 Alt+Q 随时呼出)")
                        .clicked()
                    {
                        lock.is_visible = false;
                        lock.show_settings = false;
                    }
                });

                // 允许按住标题栏拖动整个窗口
                if header_response.response.interact(egui::Sense::drag()).dragged() {
                    ui.ctx().send_viewport_cmd(egui::ViewportCommand::StartDrag);
                }

                ui.add_space(6.0);

                // 状态变动落盘
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
                        .fill(egui::Color32::WHITE)
                        .stroke(egui::Stroke::new(1.0, egui::Color32::from_rgb(208, 215, 222)))
                        .corner_radius(8.0)
                        .inner_margin(12.0)
                        .show(ui, |ui| {
                            ui.horizontal(|ui| {
                                ui.colored_label(egui::Color32::from_rgb(37, 99, 235), "⚙ 言蹊设置中心");
                                ui.add_space(8.0);

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

                                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                    if ui.button("✕ 返回翻译").clicked() {
                                        lock.show_settings = false;
                                    }
                                });
                            });

                            ui.separator();

                            if lock.settings_provider == "microsoft" {
                                ui.label(
                                    egui::RichText::new("💡 微软官方通道默认无需填写 API Key，开箱免配置即用。若您有 Azure 官方 Key，可填入下方。")
                                        .size(11.0)
                                        .color(egui::Color32::from_rgb(107, 114, 128)),
                                );
                            }

                            // 快捷键设置项
                            ui.horizontal(|ui| {
                                ui.label(egui::RichText::new("全局快捷键:").size(12.0));
                                ui.add(
                                    egui::TextEdit::singleline(&mut lock.settings_hotkey)
                                        .hint_text("如: alt+q, alt+d, ctrl+alt+t")
                                        .desired_width(180.0),
                                );
                                ui.label(
                                    egui::RichText::new("(保存后即时动态生效)")
                                        .size(11.0)
                                        .color(egui::Color32::from_rgb(107, 114, 128)),
                                );
                            });

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
                                        .desired_width(180.0),
                                );
                            });

                            ui.add_space(6.0);

                            // 底部操作按钮
                            ui.horizontal(|ui| {
                                if ui.button("⚡ 测试连通性").on_hover_text("测试当前服务商接口是否联通").clicked() {
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

                                if ui.button("💾 保存配置").clicked() {
                                    let p_name = lock.settings_provider.clone();
                                    let mut p_cfg = self.config.providers.get(&p_name).cloned().unwrap_or_default();
                                    p_cfg.api_key = lock.settings_api_key.clone();
                                    p_cfg.base_url = lock.settings_base_url.clone();
                                    p_cfg.model = lock.settings_model.clone();
                                    self.config.providers.insert(p_name, p_cfg);

                                    // 同步更新快捷键配置
                                    let new_hotkey = lock.settings_hotkey.trim().to_lowercase();
                                    if !new_hotkey.is_empty() {
                                        self.config.selection.hotkey = new_hotkey.clone();
                                        lock.update_hotkey(&new_hotkey);
                                    }

                                    let _ = self.config.save();
                                    if let Some(ref tx) = lock.tx_command {
                                        let _ = tx.send(GuiCommand::SaveConfig(self.config.clone()));
                                    }
                                    lock.test_feedback = Some((true, "✅ 配置已保存成功，热键已即时重载生效！".into()));
                                }
                            });

                            if let Some((ok, ref msg)) = lock.test_feedback {
                                ui.add_space(4.0);
                                let color = if ok {
                                    egui::Color32::from_rgb(22, 101, 52)
                                } else {
                                    egui::Color32::from_rgb(220, 38, 38)
                                };
                                ui.colored_label(color, msg);
                            }
                        });

                    return;
                }

                // ==================== 3. 主内容区域 (上下两张卡片 + 中间 Splitter) ====================
                let total_avail_h = ui.available_height().max(160.0);
                let splitter_h = 8.0;
                let cards_avail_h = (total_avail_h - splitter_h).max(140.0);

                let ratio = lock.splitter_ratio.clamp(0.25, 0.75);
                let card1_target_h = cards_avail_h * ratio;
                let card2_target_h = cards_avail_h * (1.0 - ratio);

                // 3.1 原文交互卡片 (Top Card - 白底圆角)
                egui::Frame::new()
                    .fill(egui::Color32::WHITE)
                    .stroke(egui::Stroke::new(1.0, egui::Color32::from_rgb(225, 228, 232)))
                    .corner_radius(8.0)
                    .inner_margin(egui::Margin::symmetric(10, 8))
                    .show(ui, |ui| {
                        ui.set_height(card1_target_h.max(60.0));

                        // 多行文本编辑框
                        let text_edit_response = ui.add(
                            egui::TextEdit::multiline(&mut lock.edit_text)
                                .hint_text("输入或划选文本，按回车立即翻译...")
                                .desired_rows(2)
                                .font(egui::TextStyle::Body)
                                .text_color(egui::Color32::from_rgb(31, 41, 55))
                                .frame(egui::Frame::NONE)
                                .desired_width(f32::INFINITY),
                        );

                        if text_edit_response.has_focus()
                            && ui.input(|i| i.key_pressed(egui::Key::Enter) && !i.modifiers.shift)
                        {
                            trigger_retranslate = true;
                        }

                        // 原文卡片底栏：左侧字符数，右侧发音/清空/翻译/复制
                        ui.with_layout(egui::Layout::bottom_up(egui::Align::Min), |ui| {
                            ui.horizontal(|ui| {
                                let char_count = lock.edit_text.chars().count();
                                ui.label(
                                    egui::RichText::new(format!("{char_count} 字符"))
                                        .size(11.5)
                                        .color(egui::Color32::from_rgb(107, 114, 128)),
                                );

                                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                    ui.spacing_mut().item_spacing = egui::vec2(10.0, 0.0);

                                    // 复制原文
                                    if ui
                                        .button(egui::RichText::new("复制").size(12.0).color(egui::Color32::from_rgb(75, 85, 99)))
                                        .on_hover_text("复制原文到剪贴板")
                                        .clicked()
                                    {
                                        if let Ok(mut clipboard) = arboard::Clipboard::new() {
                                            let _ = clipboard.set_text(&lock.edit_text);
                                        }
                                    }

                                    // 翻译按钮
                                    if ui
                                        .button(egui::RichText::new("翻译").size(12.0).color(egui::Color32::from_rgb(75, 85, 99)))
                                        .on_hover_text("立即发起翻译 (Enter)")
                                        .clicked()
                                    {
                                        trigger_retranslate = true;
                                    }

                                    // 清空按钮
                                    if !lock.edit_text.is_empty() {
                                        if ui
                                            .button(egui::RichText::new("清空").size(12.0).color(egui::Color32::from_rgb(75, 85, 99)))
                                            .on_hover_text("清空输入内容")
                                            .clicked()
                                        {
                                            lock.edit_text.clear();
                                            lock.status = TranslationStatus::Idle;
                                        }
                                    }

                                    // 英美发音按钮 (含英文字符才显示，免缓存流式秒播)
                                    if has_english_text(&lock.edit_text) {
                                        if ui
                                            .button(egui::RichText::new("美 🔊").size(12.0).color(egui::Color32::from_rgb(75, 85, 99)))
                                            .on_hover_text("美音发音 (免缓存流式播放)")
                                            .clicked()
                                        {
                                            lock.play_tts(&lock.edit_text, Accent::Us);
                                        }

                                        if ui
                                            .button(egui::RichText::new("英 🔊").size(12.0).color(egui::Color32::from_rgb(75, 85, 99)))
                                            .on_hover_text("英音发音 (免缓存流式播放)")
                                            .clicked()
                                        {
                                            lock.play_tts(&lock.edit_text, Accent::Uk);
                                        }
                                    }
                                });
                            });
                        });
                    });

                // ==================== 3.2 中间高度调节分割器 (Splitter) ====================
                let (splitter_rect, splitter_resp) = ui.allocate_exact_size(
                    egui::vec2(ui.available_width(), splitter_h),
                    egui::Sense::drag(),
                );

                if splitter_resp.hovered() || self.is_resizing_splitter {
                    ui.ctx().set_cursor_icon(egui::CursorIcon::ResizeVertical);
                }

                if splitter_resp.drag_started() {
                    self.is_resizing_splitter = true;
                }

                if self.is_resizing_splitter {
                    if ui.input(|i| i.pointer.any_down()) {
                        let delta_y = ui.input(|i| i.pointer.delta().y);
                        let new_ratio = (lock.splitter_ratio + delta_y / cards_avail_h).clamp(0.25, 0.75);
                        lock.splitter_ratio = new_ratio;
                        self.config.ui.splitter_ratio = new_ratio;
                    } else {
                        self.is_resizing_splitter = false;
                        let _ = self.config.save();
                    }
                }

                if splitter_resp.double_clicked() {
                    lock.splitter_ratio = 0.45;
                    self.config.ui.splitter_ratio = 0.45;
                    let _ = self.config.save();
                }

                // 绘制极具科技感的隐式居中小指示条
                let bar_color = if splitter_resp.hovered() || self.is_resizing_splitter {
                    egui::Color32::from_rgb(148, 163, 184)
                } else {
                    egui::Color32::from_rgb(226, 232, 240)
                };
                let bar_center = splitter_rect.center();
                let bar_rect = egui::Rect::from_center_size(bar_center, egui::vec2(28.0, 3.0));
                ui.painter().rect_filled(bar_rect, 1.5, bar_color);

                // ==================== 3.3 译文结果卡片 (Bottom Card - 白底圆角) ====================
                egui::Frame::new()
                    .fill(egui::Color32::WHITE)
                    .stroke(egui::Stroke::new(1.0, egui::Color32::from_rgb(225, 228, 232)))
                    .corner_radius(8.0)
                    .inner_margin(egui::Margin::symmetric(10, 8))
                    .show(ui, |ui| {
                        ui.set_height(card2_target_h.max(70.0));

                        // 内容展示区
                        egui::ScrollArea::vertical()
                            .id_salt("translation_content_scroll")
                            .auto_shrink([false, false])
                            .max_height(card2_target_h - 32.0)
                            .show(ui, |ui| {
                                match &status {
                                    TranslationStatus::Idle => {
                                        ui.label(
                                            egui::RichText::new("等待划词或输入... 在任意窗口划选，或直接在上方框中输入按回车")
                                                .size(13.0)
                                                .color(egui::Color32::from_rgb(156, 163, 175)),
                                        );
                                    }
                                    TranslationStatus::Loading { .. } => {
                                        ui.horizontal(|ui| {
                                            ui.spinner();
                                            ui.colored_label(egui::Color32::from_rgb(37, 99, 235), "正在极速翻译中...");
                                        });
                                        ui.ctx().request_repaint_after(Duration::from_millis(80));
                                    }
                                    TranslationStatus::Success(res) => {
                                        ui.label(
                                            egui::RichText::new(&res.translated_text)
                                                .size(16.0)
                                                .strong()
                                                .color(egui::Color32::from_rgb(17, 24, 39)),
                                        );

                                        if let Some(ref ph) = res.phonetic {
                                            ui.add_space(2.0);
                                            ui.label(
                                                egui::RichText::new(format!("音标/拼音: {ph}"))
                                                    .size(11.5)
                                                    .color(egui::Color32::from_rgb(180, 83, 9)),
                                            );
                                        }
                                    }
                                    TranslationStatus::Error { message, .. } => {
                                        ui.colored_label(
                                            egui::Color32::from_rgb(220, 38, 38),
                                            format!("❌ 翻译异常: {message}"),
                                        );
                                        ui.add_space(4.0);
                                        if ui.button("🔄 重试").clicked() {
                                            trigger_retranslate = true;
                                        }
                                    }
                                }
                            });

                        // 译文底栏：左侧本地缓存状态 / 耗时；右侧圆角复制按钮
                        ui.with_layout(egui::Layout::bottom_up(egui::Align::Min), |ui| {
                            ui.horizontal(|ui| {
                                if let TranslationStatus::Success(ref res) = status {
                                    if res.from_cache {
                                        ui.label(
                                            egui::RichText::new("本地缓存")
                                                .size(11.5)
                                                .color(egui::Color32::from_rgb(75, 85, 99)),
                                        );
                                    } else {
                                        ui.label(
                                            egui::RichText::new(format!("耗时: {:.0}ms", res.latency_ms))
                                                .size(11.5)
                                                .color(egui::Color32::from_rgb(107, 114, 128)),
                                        );
                                    }

                                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                        let btn_label = if is_copied { "✅ 已复制" } else { "复制" };
                                        let copy_btn = egui::Button::new(egui::RichText::new(btn_label).size(12.0).color(egui::Color32::from_rgb(31, 41, 55)))
                                            .fill(egui::Color32::WHITE)
                                            .stroke(egui::Stroke::new(1.0, egui::Color32::from_rgb(208, 215, 222)))
                                            .corner_radius(6.0);

                                        if ui.add(copy_btn).clicked() {
                                            if let Ok(mut clipboard) = arboard::Clipboard::new() {
                                                let _ = clipboard.set_text(&res.translated_text);
                                                lock.copy_feedback_time = Some(std::time::Instant::now());
                                            }
                                        }
                                    });
                                }
                            });
                        });
                    });
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
        "microsoft" => "微软翻译",
        "deepseek" => "DeepSeek",
        "openai" => "OpenAI",
        "zhipu" => "智谱GLM",
        "custom" => "本地Ollama",
        _ => "自定义",
    }
}

fn lang_display_short(lang: &str) -> &'static str {
    match lang {
        "auto" => "自动",
        "zh-CN" | "zh" => "中",
        "en" => "英",
        "ja" => "日",
        "ko" => "韩",
        "fr" => "法",
        "de" => "德",
        "es" => "西",
        "ru" => "俄",
        _ => "语",
    }
}
