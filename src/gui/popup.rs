use eframe::egui;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use super::state::{SharedPopupState, TranslationStatus};

pub struct PopupApp {
    state: Arc<Mutex<SharedPopupState>>,
}

impl PopupApp {
    pub fn new(state: Arc<Mutex<SharedPopupState>>) -> Self {
        Self { state }
    }
}

impl eframe::App for PopupApp {
    fn clear_color(&self, _visuals: &egui::Visuals) -> [f32; 4] {
        [0.0, 0.0, 0.0, 0.0] // 保证完全无边框透明
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        ui.ctx().set_visuals(egui::Visuals::dark());

        let mut lock = match self.state.lock() {
            Ok(guard) => guard,
            Err(_) => return,
        };

        // 处理位置更新与显隐控制
        if lock.should_update_pos {
            if let Some(pos) = lock.window_pos {
                ui.ctx().send_viewport_cmd(egui::ViewportCommand::OuterPosition(egui::pos2(pos.0, pos.1)));
            }
            lock.should_update_pos = false;
        }

        if !lock.is_visible {
            ui.ctx().send_viewport_cmd(egui::ViewportCommand::Visible(false));
            return;
        } else {
            ui.ctx().send_viewport_cmd(egui::ViewportCommand::Visible(true));
        }

        let is_pinned = lock.is_pinned;
        let active_provider = lock.active_provider.clone();
        let target_lang = lock.target_lang.clone();
        let status = lock.status.clone();
        let is_copied = lock
            .copy_feedback_time
            .map(|t| t.elapsed() < Duration::from_millis(1500))
            .unwrap_or(false);

        // 主玻璃拟态暗黑卡片
        egui::Frame::new()
            .fill(egui::Color32::from_rgba_premultiplied(22, 27, 34, 245))
            .stroke(egui::Stroke::new(1.0, egui::Color32::from_rgb(48, 54, 61)))
            .corner_radius(12.0)
            .inner_margin(12.0)
            .show(ui, |ui| {
                // 1. 顶部操作栏 (Header Bar)
                let header_response = ui.horizontal(|ui| {
                    // 品牌标识
                    ui.colored_label(egui::Color32::from_rgb(88, 166, 255), "言蹊翻译");

                    // 提供商胶囊
                    let p_badge = match active_provider.as_str() {
                        "microsoft" => "🌐 微软官方免Key",
                        "deepseek" => "🤖 DeepSeek",
                        "openai" => "⚡ OpenAI",
                        other => other,
                    };
                    ui.label(
                        egui::RichText::new(format!("[{p_badge}]"))
                            .size(11.0)
                            .color(egui::Color32::from_rgb(139, 148, 158)),
                    );

                    // 语言方向
                    ui.label(
                        egui::RichText::new(format!("自动 ⇄ {target_lang}"))
                            .size(11.0)
                            .color(egui::Color32::from_rgb(110, 118, 129)),
                    );

                    // 右侧控制按钮
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        // 关闭按钮
                        if ui
                            .button(egui::RichText::new("✕").size(12.0).color(egui::Color32::from_rgb(139, 148, 158)))
                            .on_hover_text("关闭悬浮窗")
                            .clicked()
                        {
                            lock.is_visible = false;
                        }

                        // 图钉固定按钮
                        let pin_color = if is_pinned {
                            egui::Color32::from_rgb(235, 179, 56) // 高亮金黄色
                        } else {
                            egui::Color32::from_rgb(110, 118, 129) // 柔和灰
                        };
                        let pin_text = if is_pinned { "📌 已固定" } else { "📌 固定" };
                        if ui
                            .button(egui::RichText::new(pin_text).size(11.0).color(pin_color))
                            .on_hover_text("固定悬浮窗位置 (防止点击外部自动收起)")
                            .clicked()
                        {
                            lock.is_pinned = !is_pinned;
                        }
                    });
                });

                // 支持按住标题栏拖动整个窗口
                if header_response.response.interact(egui::Sense::drag()).dragged() {
                    ui.ctx().send_viewport_cmd(egui::ViewportCommand::StartDrag);
                }

                ui.add_space(4.0);
                ui.separator();
                ui.add_space(4.0);

                // 2. 原文与译文区域
                match status {
                    TranslationStatus::Idle => {
                        ui.label(
                            egui::RichText::new("等待划词选选中... 在任意窗口划选英文即可自动翻译")
                                .size(13.0)
                                .color(egui::Color32::from_rgb(139, 148, 158)),
                        );
                    }
                    TranslationStatus::Loading { original_text } => {
                        // 原文展示
                        egui::Frame::new()
                            .fill(egui::Color32::from_rgba_premultiplied(13, 17, 23, 180))
                            .corner_radius(6.0)
                            .inner_margin(8.0)
                            .show(ui, |ui| {
                                ui.label(
                                    egui::RichText::new(&original_text)
                                        .size(13.0)
                                        .color(egui::Color32::from_rgb(201, 209, 217)),
                                );
                            });

                        ui.add_space(8.0);
                        ui.horizontal(|ui| {
                            ui.spinner();
                            ui.colored_label(egui::Color32::from_rgb(88, 166, 255), "正在极速翻译中...");
                        });
                        ui.ctx().request_repaint_after(Duration::from_millis(80));
                    }
                    TranslationStatus::Success(res) => {
                        // 原文展示
                        egui::Frame::new()
                            .fill(egui::Color32::from_rgba_premultiplied(13, 17, 23, 180))
                            .corner_radius(6.0)
                            .inner_margin(8.0)
                            .show(ui, |ui| {
                                ui.label(
                                    egui::RichText::new(&res.original_text)
                                        .size(12.5)
                                        .color(egui::Color32::from_rgb(139, 148, 158)),
                                );
                            });

                        ui.add_space(8.0);

                        // 译文卡片展示
                        egui::Frame::new()
                            .fill(egui::Color32::from_rgba_premultiplied(28, 33, 40, 220))
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

                        // 3. 底部状态指示
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
                    TranslationStatus::Error { original_text, message } => {
                        egui::Frame::new()
                            .fill(egui::Color32::from_rgba_premultiplied(13, 17, 23, 180))
                            .corner_radius(6.0)
                            .inner_margin(8.0)
                            .show(ui, |ui| {
                                ui.label(
                                    egui::RichText::new(&original_text)
                                        .size(12.5)
                                        .color(egui::Color32::from_rgb(139, 148, 158)),
                                );
                            });

                        ui.add_space(6.0);
                        ui.colored_label(
                            egui::Color32::from_rgb(248, 81, 73),
                            format!("❌ 翻译异常: {message}"),
                        );
                    }
                }
            });
    }
}
