use eframe::egui;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use super::state::{GuiCommand, SharedPopupState, TranslationStatus};
use crate::config::{AppConfig, ProviderConfig, SelectionMode};
use crate::tts::{Accent, has_english_text};

pub struct PopupApp {
    state: Arc<Mutex<SharedPopupState>>,
    config: AppConfig,
    show_key_plain: bool,
    last_visible: Option<bool>,
    last_pinned: Option<bool>,
    was_focused: bool,
    is_resizing_splitter: bool,
    initial_sized: bool,
    last_geometry_check: Option<std::time::Instant>,
    geometry_checks: u8,
    placing: bool,
    active_resize: Option<egui::viewport::ResizeDirection>,
    drag_anchor: Option<((i32, i32), (f32, f32))>,
}

impl PopupApp {
    pub fn new(state: Arc<Mutex<SharedPopupState>>, config: AppConfig) -> Self {
        Self {
            state,
            config,
            show_key_plain: false,
            last_visible: None,
            last_pinned: None,
            was_focused: false,
            is_resizing_splitter: false,
            initial_sized: false,
            last_geometry_check: None,
            geometry_checks: 0,
            placing: false,
            active_resize: None,
            drag_anchor: None,
        }
    }
}

impl eframe::App for PopupApp {
    fn clear_color(&self, _visuals: &egui::Visuals) -> [f32; 4] {
        [0.0, 0.0, 0.0, 0.0] // 保证完全无边框透明
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.render(ui);
    }
}

impl PopupApp {
    fn render(&mut self, ui: &mut egui::Ui) {
        // 全局 Esc 快捷键：随时收起浮窗
        if ui.input(|i| i.key_pressed(egui::Key::Escape)) {
            if let Ok(mut lock) = self.state.lock() {
                lock.hide_explicit();
            }
        }

        let mut lock = match self.state.lock() {
            Ok(guard) => guard,
            Err(_) => return,
        };

        sync_window_level(&mut self.last_pinned, ui.ctx(), &lock);

        // Window state is stored in physical pixels; one helper owns all conversions.
        if self.last_visible != Some(lock.is_visible) {
            ui.ctx()
                .send_viewport_cmd(egui::ViewportCommand::Visible(lock.is_visible));
            self.last_visible = Some(lock.is_visible);
            if lock.is_visible {
                lock.last_shown_at = Some(std::time::Instant::now());
                #[cfg(windows)]
                if let Some(hwnd) = lock.win32_hwnd {
                    crate::gui::ensure_borderless_popup_style_win32(hwnd);
                }
                if !lock.is_pinned {
                    crate::gui::raise_window(lock.x11_window, lock.win32_hwnd);
                }
                ui.ctx().request_repaint();
                // Re-arm geometry repair: a remap can resize/move the window again.
                self.geometry_checks = 0;
                self.placing = false;
                let state = Arc::clone(&self.state);
                let ctx = ui.ctx().clone();
                // Mapping the native window may resize or move it. Restore the
                // stored geometry (which is the pinned position when pinned).
                // Read it again after the delay so a drag during it is not lost.
                std::thread::spawn(move || {
                    // Remap is asynchronous: re-assert a few times instead of once.
                    for _ in 0..8 {
                        std::thread::sleep(Duration::from_millis(60));
                        if let Ok(lock) = state.lock() {
                            if !lock.is_visible {
                                return;
                            }
                            if let Some(pos) = lock.window_pos {
                                crate::gui::apply_window_geometry(
                                    &ctx,
                                    lock.x11_window,
                                    lock.win32_hwnd,
                                    pos,
                                    lock.window_size,
                                );
                            }
                        }
                    }
                });
            } else {
                self.was_focused = false;
                self.drag_anchor = None;
                lock.is_popup_open_in_ui = false;
            }
        }

        if !lock.is_visible {
            return;
        }

        #[cfg(windows)]
        if let Some(hwnd) = lock.win32_hwnd {
            crate::gui::ensure_borderless_popup_style_win32(hwnd);
        }

        let focused = ui.input(|i| i.focused);
        lock.is_popup_open_in_ui = ui.ctx().any_popup_open()
            || self.active_resize.is_some()
            || self.is_resizing_splitter
            || self.drag_anchor.is_some();
        if self.was_focused
            && !focused
            && !lock.is_pinned
            && !lock.show_settings
            && !lock.is_popup_open_in_ui
            && lock
                .last_shown_at
                .is_some_and(|t| t.elapsed() >= Duration::from_millis(200))
        {
            lock.dismiss_from_outside(std::time::Instant::now());
            ui.ctx()
                .send_viewport_cmd(egui::ViewportCommand::Visible(false));
            self.last_visible = Some(false);
            self.was_focused = false;
            return;
        }
        self.was_focused = focused;

        if !self.initial_sized {
            let pos = lock.window_pos.unwrap_or_else(|| {
                let (ox, oy) = lock.screen_origin;
                let (sw, sh) = lock.screen_size;
                let (w, h) = lock.window_size;
                (ox + ((sw - w) / 2.0).max(10.0), oy + ((sh - h) / 2.0).max(10.0))
            });
            crate::gui::apply_window_geometry(
                ui.ctx(),
                lock.x11_window,
                lock.win32_hwnd,
                pos,
                lock.window_size,
            );
            lock.window_pos = Some(pos);
            self.initial_sized = true;
        }

        if lock.should_update_pos {
            if !lock.is_pinned {
                if let Some(pos) = lock.window_pos {
                    crate::gui::apply_window_geometry(
                        ui.ctx(),
                        lock.x11_window,
                        lock.win32_hwnd,
                        pos,
                        lock.window_size,
                    );
                }
            }
            lock.should_update_pos = false;
        }

        // Some window managers ignore the first size request and map this utility
        // window at workarea size. If that happens the fixed 28pt header stretches
        // across the screen and controls no longer match the layout. Re-assert the
        // stored size a few times after showing, never during a user drag.
        let correcting = self.active_resize.is_none()
            && !self.is_resizing_splitter
            && self.drag_anchor.is_none();
        let due = self
            .last_geometry_check
            .map(|last| last.elapsed() >= Duration::from_millis(120))
            .unwrap_or(true);
        // Repair post-map WM resizing/repositioning only during the short remap window
        // so manual OS/WM moves (e.g. Alt+Drag) are never fought afterwards.
        let limit_reached = self.geometry_checks >= 20;
        if correcting && due && !limit_reached {
            self.last_geometry_check = Some(std::time::Instant::now());
            self.geometry_checks += 1;
            #[cfg(target_os = "linux")]
            if let Some(win) = lock.x11_window {
                if let Some((actual_pos, actual_size)) = crate::gui::get_window_geometry_x11(win) {
                    let off = (actual_size.0 - lock.window_size.0).abs() > 4.0
                        || (actual_size.1 - lock.window_size.1).abs() > 4.0;
                    let moved = lock
                        .window_pos
                        .map(|pos| {
                            (pos.0 - actual_pos.0).abs() > 2.0 || (pos.1 - actual_pos.1).abs() > 2.0
                        })
                        .unwrap_or(false);
                    if off || (lock.is_pinned && moved && self.geometry_checks <= 2) {
                        // A window manager may ignore the first size request, map a
                        // utility window at workarea size, and move it again on
                        // remap. Re-assert the stored geometry, which is the pinned
                        // position while pinned.
                        let pos = if lock.is_pinned {
                            lock.window_pos.unwrap_or(actual_pos)
                        } else {
                            actual_pos
                        };

                        self.placing = true;
                        println!(
                            "[Yanxi] 修正窗口几何: {:?}@{:?} -> {:?}@{:?}",
                            actual_size, actual_pos, lock.window_size, pos
                        );
                        // A maximized/fullscreen WM state ignores explicit sizes.
                        crate::gui::clear_maximized_state_x11(win);
                        crate::gui::apply_window_geometry(
                            ui.ctx(),
                            Some(win),
                            lock.win32_hwnd,
                            pos,
                            lock.window_size,
                        );
                        lock.window_pos = Some(pos);
                    } else {
                        if lock.window_pos.is_none() {
                            lock.window_pos = Some(actual_pos);
                        }
                        self.placing = false;
                    }
                }
            }
            #[cfg(windows)]
            if let Some(hwnd) = lock.win32_hwnd {
                if let Some((actual_pos, actual_size)) = crate::gui::get_window_geometry_win32(hwnd) {
                    let off = (actual_size.0 - lock.window_size.0).abs() > 4.0
                        || (actual_size.1 - lock.window_size.1).abs() > 4.0;
                    let moved = lock
                        .window_pos
                        .map(|pos| {
                            (pos.0 - actual_pos.0).abs() > 2.0 || (pos.1 - actual_pos.1).abs() > 2.0
                        })
                        .unwrap_or(false);
                    if off || (lock.is_pinned && moved && self.geometry_checks <= 2) {
                        let pos = if lock.is_pinned {
                            lock.window_pos.unwrap_or(actual_pos)
                        } else {
                            actual_pos
                        };
                        self.placing = true;
                        crate::gui::apply_window_geometry(
                            ui.ctx(),
                            lock.x11_window,
                            Some(hwnd),
                            pos,
                            lock.window_size,
                        );
                        lock.window_pos = Some(pos);
                    } else {
                        if lock.window_pos.is_none() {
                            lock.window_pos = Some(actual_pos);
                        }
                        self.placing = false;
                    }
                }
            }
        }

        let is_pinned = lock.is_pinned;
        let show_settings = lock.show_settings;
        let mut source_lang = lock.source_lang.clone();
        let mut target_lang = lock.target_lang.clone();
        let available_providers = lock.available_providers.clone();
        let status = lock.status.clone();
        let is_copied = lock
            .copy_feedback_time
            .map(|t| t.elapsed() < Duration::from_millis(1500))
            .unwrap_or(false);

        let mut trigger_retranslate = false;

        let outer_rect = ui.max_rect();
        if !ui.ctx().any_popup_open() {
            // 1. 如果当前正在通过四角/边缘拉伸缩放 (软件直控，松手立停，绝不锁死鼠标)
            if let Some(dir) = self.active_resize {
                if ui.input(|i| i.pointer.primary_down()) {
                    let cursor_icon = match dir {
                        egui::viewport::ResizeDirection::SouthEast => {
                            egui::CursorIcon::ResizeSouthEast
                        }
                        egui::viewport::ResizeDirection::SouthWest => {
                            egui::CursorIcon::ResizeSouthWest
                        }
                        egui::viewport::ResizeDirection::NorthEast => {
                            egui::CursorIcon::ResizeNorthEast
                        }
                        egui::viewport::ResizeDirection::NorthWest => {
                            egui::CursorIcon::ResizeNorthWest
                        }
                        egui::viewport::ResizeDirection::South => egui::CursorIcon::ResizeSouth,
                        egui::viewport::ResizeDirection::East => egui::CursorIcon::ResizeEast,
                        egui::viewport::ResizeDirection::West => egui::CursorIcon::ResizeWest,
                        _ => egui::CursorIcon::Default,
                    };
                    ui.ctx().set_cursor_icon(cursor_icon);

                    let delta = ui.input(|i| i.pointer.delta());
                    if delta.length_sq() > 0.0 {
                        let ppp = ui.ctx().pixels_per_point();
                        {
                            let (mut cur_x, mut cur_y) =
                                lock.window_pos.unwrap_or_else(|| current_window_position(&lock));
                            let (mut cur_w, mut cur_h) = lock.window_size;
                            let min_w = 380.0 * ppp;
                            let min_h = 220.0 * ppp;

                            let dx = delta.x * ppp;
                            let dy = delta.y * ppp;

                            match dir {
                                egui::viewport::ResizeDirection::SouthEast => {
                                    cur_w = (cur_w + dx).max(min_w);
                                    cur_h = (cur_h + dy).max(min_h);
                                }
                                egui::viewport::ResizeDirection::SouthWest => {
                                    let new_w = (cur_w - dx).max(min_w);
                                    cur_x += cur_w - new_w;
                                    cur_w = new_w;
                                    cur_h = (cur_h + dy).max(min_h);
                                }
                                egui::viewport::ResizeDirection::NorthEast => {
                                    cur_w = (cur_w + dx).max(min_w);
                                    let new_h = (cur_h - dy).max(min_h);
                                    cur_y += cur_h - new_h;
                                    cur_h = new_h;
                                }
                                egui::viewport::ResizeDirection::NorthWest => {
                                    let new_w = (cur_w - dx).max(min_w);
                                    cur_x += cur_w - new_w;
                                    cur_w = new_w;
                                    let new_h = (cur_h - dy).max(min_h);
                                    cur_y += cur_h - new_h;
                                    cur_h = new_h;
                                }
                                egui::viewport::ResizeDirection::South => {
                                    cur_h = (cur_h + dy).max(min_h);
                                }
                                egui::viewport::ResizeDirection::East => {
                                    cur_w = (cur_w + dx).max(min_w);
                                }
                                egui::viewport::ResizeDirection::West => {
                                    let new_w = (cur_w - dx).max(min_w);
                                    cur_x += cur_w - new_w;
                                    cur_w = new_w;
                                }
                                _ => {}
                            }

                            lock.window_pos = Some((cur_x, cur_y));
                            lock.window_size = (cur_w, cur_h);
                            crate::gui::apply_window_geometry(
                                ui.ctx(),
                                lock.x11_window,
                                lock.win32_hwnd,
                                (cur_x, cur_y),
                                (cur_w, cur_h),
                            );
                        }
                    }
                } else {
                    if lock.is_pinned {
                        self.config.ui.pinned_pos = lock.window_pos;
                        self.config.ui.pinned_size = Some(lock.window_size);
                        persist_ui_config(&lock, &self.config);
                    }
                    self.active_resize = None;
                }
            } else if let Some(pointer_pos) = ui.input(|i| i.pointer.hover_pos()) {
                // 2. 检测边缘与四角悬浮和点击 (软件直控，绝不调用系统级 BeginResize，杜绝 X11 指针抓手死锁)
                let margin = 6.0;
                let corner = 14.0;
                let right = pointer_pos.x >= outer_rect.max.x - margin;
                let bottom = pointer_pos.y >= outer_rect.max.y - margin;
                let left = pointer_pos.y > 36.0 && pointer_pos.x <= outer_rect.min.x + margin;

                let c_right = pointer_pos.x >= outer_rect.max.x - corner;
                let c_bottom = pointer_pos.y >= outer_rect.max.y - corner;
                let c_left = pointer_pos.x <= outer_rect.min.x + corner;
                let c_top = pointer_pos.y <= outer_rect.min.y + 6.0;

                let resize_dir = if c_bottom && c_right {
                    Some(egui::viewport::ResizeDirection::SouthEast)
                } else if c_bottom && c_left {
                    Some(egui::viewport::ResizeDirection::SouthWest)
                } else if c_top && pointer_pos.x <= outer_rect.min.x + 6.0 {
                    Some(egui::viewport::ResizeDirection::NorthWest)
                } else if c_top && pointer_pos.x >= outer_rect.max.x - 6.0 {
                    Some(egui::viewport::ResizeDirection::NorthEast)
                } else if bottom {
                    Some(egui::viewport::ResizeDirection::South)
                } else if right && pointer_pos.y > 36.0 {
                    Some(egui::viewport::ResizeDirection::East)
                } else if left {
                    Some(egui::viewport::ResizeDirection::West)
                } else {
                    None
                };

                if let Some(dir) = resize_dir {
                    let cursor_icon = match dir {
                        egui::viewport::ResizeDirection::SouthEast => {
                            egui::CursorIcon::ResizeSouthEast
                        }
                        egui::viewport::ResizeDirection::SouthWest => {
                            egui::CursorIcon::ResizeSouthWest
                        }
                        egui::viewport::ResizeDirection::NorthEast => {
                            egui::CursorIcon::ResizeNorthEast
                        }
                        egui::viewport::ResizeDirection::NorthWest => {
                            egui::CursorIcon::ResizeNorthWest
                        }
                        egui::viewport::ResizeDirection::South => egui::CursorIcon::ResizeSouth,
                        egui::viewport::ResizeDirection::East => egui::CursorIcon::ResizeEast,
                        egui::viewport::ResizeDirection::West => egui::CursorIcon::ResizeWest,
                        _ => egui::CursorIcon::Default,
                    };
                    ui.ctx().set_cursor_icon(cursor_icon);

                    if ui.input(|i| i.pointer.primary_pressed()) {
                        if lock.window_pos.is_none() {
                            lock.window_pos = Some(current_window_position(&lock));
                        }
                        self.active_resize = Some(dir);
                    }
                }
            }
        }

        // ==================== 主悬浮窗外部容器 ====================
        egui::Frame::new()
            .fill(theme::SURFACE)
            .stroke(egui::Stroke::new(1.0, theme::LINE))
            .corner_radius(14.0)
            .shadow(egui::epaint::Shadow {
                offset: [0, 10],
                blur: 26,
                spread: 0,
                color: egui::Color32::from_black_alpha(24),
            })
            .inner_margin(egui::Margin::ZERO)
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.set_height(ui.available_height());

                // Every header control uses the same 28-point cell and center.
                let header_frame = egui::Frame::new()
                    .inner_margin(egui::Margin::symmetric(10, 6))
                    .show(ui, |ui| {
                        ui.allocate_ui_with_layout(
                            egui::vec2(ui.available_width(), HEADER_HEIGHT),
                            egui::Layout::left_to_right(egui::Align::Center),
                            |ui| {
                                ui.spacing_mut().item_spacing = egui::vec2(2.0, 0.0);
                                if language_selector(ui, "source", &mut source_lang, true).changed() {
                                    trigger_retranslate = true;
                                }
                                if icon_button(ui, Icon::Swap, false, true, "互换源语言与目标语言").clicked() {
                                    // Commit dropdown edits before asking state to resolve auto.
                                    lock.source_lang = source_lang.clone();
                                    lock.target_lang = target_lang.clone();
                                    lock.swap_languages();
                                    source_lang = lock.source_lang.clone();
                                    target_lang = lock.target_lang.clone();
                                    trigger_retranslate = !lock.edit_text.trim().is_empty();
                                    // swap_languages already updated state, so persist explicitly.
                                    self.config.source_lang = source_lang.clone();
                                    self.config.target_lang = target_lang.clone();
                                    let _ = self.config.save();
                                    sync_worker_config(&lock, &self.config);
                                }
                                if language_selector(ui, "target", &mut target_lang, false).changed() {
                                    trigger_retranslate = true;
                                }
                                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                    ui.spacing_mut().item_spacing.x = 4.0;
                                    if icon_button(ui, Icon::Close, false, true, "收起浮窗 (Alt+Q 呼出)").clicked() {
                                        lock.hide_explicit();
                                    }
                                    if icon_button(ui, Icon::Sliders, show_settings, true, "管理翻译服务、取词模式与快捷键").clicked() {
                                        lock.show_settings = !lock.show_settings;
                                        if lock.show_settings {
                                            let active_p = lock.active_provider.clone();
                                            lock.load_provider_settings(&self.config, &active_p);
                                        }
                                    }
                                    let pin_tip = if is_pinned {
                                        "取消固定（恢复自动隐藏与跟随）"
                                    } else {
                                        "固定在最上方（置顶、固定位置且不自动隐藏）"
                                    };
                                    if icon_button(ui, Icon::Pin, is_pinned, true, pin_tip).clicked() {
                                        #[cfg(target_os = "linux")]
                                        if let Some(win) = lock.x11_window {
                                            if let Some((pos, size)) = crate::gui::get_window_geometry_x11(win) {
                                                lock.window_pos = Some(pos);
                                                lock.window_size = size;
                                            }
                                        }
                                        #[cfg(windows)]
                                        if let Some(hwnd) = lock.win32_hwnd {
                                            if let Some((pos, size)) = crate::gui::get_window_geometry_win32(hwnd) {
                                                lock.window_pos = Some(pos);
                                                lock.window_size = size;
                                            }
                                        }
                                        lock.toggle_pin();
                                        sync_window_level(&mut self.last_pinned, ui.ctx(), &lock);
                                        self.config.ui.pinned = lock.is_pinned;
                                        self.config.ui.pinned_pos = lock.window_pos;
                                        self.config.ui.pinned_size = Some(lock.window_size);
                                        persist_ui_config(&lock, &self.config);
                                    }
                                    // Only the gap between language controls and tools drags.
                                    let remaining_w = ui.available_width();
                                    if remaining_w > 0.0 {
                                        let (rect, response) = ui.allocate_exact_size(
                                            egui::vec2(remaining_w, HEADER_HEIGHT), egui::Sense::drag(),
                                        );
                                        record_control(ui, "drag", rect);
                                        if response.drag_started() || (response.dragged() && self.drag_anchor.is_none()) {
                                            let start_win = current_window_position(&lock);
                                            lock.window_pos = Some(start_win);
                                            if let Some(start_cursor) = crate::gui::cursor_pos_screen() {
                                                self.drag_anchor = Some((start_cursor, start_win));
                                            }
                                        }
                                        if response.dragged() {
                                            let target = if let (Some((start_cursor, start_win)), Some(cur_cursor)) =
                                                (self.drag_anchor, crate::gui::cursor_pos_screen())
                                            {
                                                let dx = (cur_cursor.0 - start_cursor.0) as f32;
                                                let dy = (cur_cursor.1 - start_cursor.1) as f32;
                                                Some(((start_win.0 + dx).round(), (start_win.1 + dy).round()))
                                            } else {
                                                let delta = ui.input(|i| i.pointer.delta()) * ui.ctx().pixels_per_point();
                                                (delta.length_sq() > 0.0).then(|| {
                                                    let (x, y) = lock.window_pos.unwrap_or_else(|| current_window_position(&lock));
                                                    ((x + delta.x).round(), (y + delta.y).round())
                                                })
                                            };
                                            if let Some(pos) = target {
                                                if lock.window_pos != Some(pos) {
                                                    lock.window_pos = Some(pos);
                                                    crate::gui::apply_window_geometry(
                                                        ui.ctx(), lock.x11_window, lock.win32_hwnd, pos, lock.window_size,
                                                    );
                                                }
                                            }
                                            ui.ctx().request_repaint();
                                        } else if (self.drag_anchor.take().is_some()
                                            || response.drag_stopped())
                                            && lock.is_pinned
                                        {
                                            self.config.ui.pinned_pos = lock.window_pos;
                                            self.config.ui.pinned_size = Some(lock.window_size);
                                            persist_ui_config(&lock, &self.config);
                                        }
                                    }
                                });
                            },
                        );
                    });

                let header_bottom = header_frame.response.rect.max.y;
                ui.painter().hline(
                    ui.max_rect().x_range(),
                    header_bottom,
                    egui::Stroke::new(1.0, theme::LINE),
                );

                // 状态变动落盘
                if source_lang != lock.source_lang {
                    lock.source_lang = source_lang.clone();
                    self.config.source_lang = source_lang;
                    let _ = self.config.save();
                    sync_worker_config(&lock, &self.config);
                }
                if target_lang != lock.target_lang {
                    lock.target_lang = target_lang.clone();
                    self.config.target_lang = target_lang;
                    let _ = self.config.save();
                    sync_worker_config(&lock, &self.config);
                }

                // ==================== 2. 设置面板视图 (Settings View) ====================
                if lock.show_settings {
                    egui::Frame::new()
                        .inner_margin(egui::Margin {
                            left: 14,
                            right: 14,
                            top: 12,
                            bottom: 10,
                        })
                        .show(ui, |ui| {
                            egui::ScrollArea::vertical()
                                .id_salt("settings_scroll")
                                .auto_shrink([false, false])
                                .max_height(ui.available_height().max(0.0))
                                .show(ui, |ui| {
                            let widgets = &mut ui.style_mut().visuals.widgets;
                            widgets.inactive.weak_bg_fill = theme::FILL;
                            widgets.inactive.bg_stroke = egui::Stroke::NONE;
                            widgets.hovered.weak_bg_fill = theme::LINE;
                            widgets.hovered.bg_stroke = egui::Stroke::NONE;
                            widgets.active.weak_bg_fill = theme::LINE;
                            widgets.active.bg_stroke = egui::Stroke::NONE;
                            ui.style_mut().visuals.extreme_bg_color = theme::FILL;
                            ui.spacing_mut().item_spacing.y = 6.0;

                            ui.horizontal(|ui| {
                                ui.label(
                                    egui::RichText::new("设置")
                                        .size(theme::FS_BODY)
                                        .strong()
                                        .color(theme::INK),
                                );
                                ui.add_space(14.0);

                                ui.label(
                                    egui::RichText::new("翻译引擎")
                                        .size(theme::FS_ACTION)
                                        .color(theme::SUB),
                                );
                                let mut cur_sel_p = lock.settings_provider.clone();
                                egui::ComboBox::from_id_salt("settings_provider_dropdown")
                                    .width(108.0)
                                    .selected_text(
                                        egui::RichText::new(provider_badge(&cur_sel_p))
                                            .size(theme::FS_ACTION)
                                            .color(theme::INK),
                                    )
                                    .show_ui(ui, |ui| {
                                        for p in &available_providers {
                                            ui.selectable_value(&mut cur_sel_p, p.clone(), provider_badge(p));
                                        }
                                    })
                                    .response
                                    .on_hover_text("切换后立即生效并自动重新翻译当前文本");

                                if cur_sel_p != lock.settings_provider {
                                    lock.load_provider_settings(&self.config, &cur_sel_p);
                                    lock.active_provider = cur_sel_p.clone();
                                    self.config.active_provider = cur_sel_p.clone();
                                    let _ = self.config.save();
                                    sync_worker_config(&lock, &self.config);
                                    let text = lock.edit_text.clone();
                                    if !text.trim().is_empty() {
                                        lock.trigger_translate(&text);
                                    }
                                }

                                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                    if text_button(ui, "返回", false).clicked() {
                                        lock.show_settings = false;
                                    }
                                });
                            });

                            if lock.settings_provider == "microsoft" {
                                ui.label(
                                    egui::RichText::new("微软官方通道默认无需填写 API Key，开箱即用；如有 Azure Key 可填入下方。")
                                        .size(10.5)
                                        .color(theme::FAINT),
                                );
                            }

                            // 取词模式设置项
                            ui.horizontal(|ui| {
                                setting_label(ui, "取词模式");
                                let mut mode = lock.selection_mode;
                                egui::ComboBox::from_id_salt("settings_mode_select")
                                    .width(108.0)
                                    .selected_text(
                                        egui::RichText::new(mode_label(mode))
                                            .size(theme::FS_ACTION)
                                            .color(theme::INK),
                                    )
                                    .show_ui(ui, |ui| {
                                        ui.selectable_value(
                                            &mut mode,
                                            SelectionMode::Companion,
                                            "热键唤醒 · 连续划词 (默认：平时静默，弹窗打开后选词自动翻)",
                                        );
                                        ui.selectable_value(
                                            &mut mode,
                                            SelectionMode::Automatic,
                                            "始终划选翻译 (任意划选直接自动弹窗)",
                                        );
                                        ui.selectable_value(
                                            &mut mode,
                                            SelectionMode::Manual,
                                            "仅快捷键/手动 (鼠标划选不自动弹窗)",
                                        );
                                    });
                                if mode != lock.selection_mode {
                                    lock.selection_mode = mode;
                                    self.config.selection.set_mode(mode);
                                    let _ = self.config.save();
                                    sync_worker_config(&lock, &self.config);
                                }
                            });

                            // 快捷键设置项
                            ui.horizontal(|ui| {
                                setting_label(ui, "快捷键");
                                let response = ui.add(
                                    egui::TextEdit::singleline(&mut lock.settings_hotkey)
                                        .hint_text("如: alt+q")
                                        .font(egui::FontId::proportional(theme::FS_ACTION))
                                        .desired_width(150.0),
                                );
                                if response.changed() {
                                    lock.hotkey_feedback = None;
                                }
                            });
                            ui.horizontal(|ui| {
                                setting_label(ui, "启动");
                                if ui
                                    .checkbox(&mut self.config.ui.start_hidden, "启动时隐藏浮窗")
                                    .changed()
                                {
                                    let _ = self.config.save();
                                }
                            });
                            if let Some((ok, msg)) = &lock.hotkey_feedback {
                                ui.add(egui::Label::new(egui::RichText::new(msg)
                                    .size(theme::FS_ACTION)
                                    .color(if *ok { theme::OK } else { theme::ERR })).wrap());
                            }

                            // API Key 输入
                            ui.horizontal(|ui| {
                                setting_label(ui, "API Key");
                                ui.add(
                                    egui::TextEdit::singleline(&mut lock.settings_api_key)
                                        .password(!self.show_key_plain)
                                        .font(egui::FontId::proportional(theme::FS_ACTION))
                                        .desired_width((ui.available_width() - 48.0).max(40.0)),
                                );
                                let eye = if self.show_key_plain { "隐藏" } else { "显示" };
                                if text_button(ui, eye, false).clicked() {
                                    self.show_key_plain = !self.show_key_plain;
                                }
                            });

                            // Base URL 输入
                            ui.horizontal(|ui| {
                                setting_label(ui, "Base URL");
                                ui.add(
                                    egui::TextEdit::singleline(&mut lock.settings_base_url)
                                        .font(egui::FontId::proportional(theme::FS_ACTION))
                                        .desired_width(ui.available_width()),
                                );
                            });

                            // Model 名称输入
                            ui.horizontal(|ui| {
                                setting_label(ui, "模型");
                                ui.add(
                                    egui::TextEdit::singleline(&mut lock.settings_model)
                                        .font(egui::FontId::proportional(theme::FS_ACTION))
                                        .desired_width(ui.available_width()),
                                );
                            });

                            ui.add_space(6.0);

                            // 底部操作按钮
                            ui.horizontal(|ui| {
                                let test_btn = egui::Button::new(
                                    egui::RichText::new("测试连接")
                                        .size(theme::FS_ACTION)
                                        .color(theme::SUB),
                                )
                                .fill(theme::FILL)
                                .stroke(egui::Stroke::NONE)
                                .corner_radius(6.0);
                                if ui.add(test_btn).on_hover_text("测试当前服务商接口是否联通").clicked() {
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

                                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                    let save_btn = egui::Button::new(
                                        egui::RichText::new("保存")
                                            .size(theme::FS_ACTION)
                                            .color(egui::Color32::WHITE),
                                    )
                                    .fill(theme::INK)
                                    .stroke(egui::Stroke::NONE)
                                    .corner_radius(6.0);
                                    if ui.add(save_btn).clicked() {
                                        let new_hotkey = lock.settings_hotkey.trim().to_lowercase();
                                        if let Err(error) = crate::selection::hotkey::validate_hotkey(&new_hotkey) {
                                            lock.hotkey_feedback = Some((false, error));
                                            lock.test_feedback = None;
                                            return;
                                        }
                                        let p_name = lock.settings_provider.clone();
                                        let mut p_cfg = self.config.providers.get(&p_name).cloned().unwrap_or_default();
                                        p_cfg.api_key = lock.settings_api_key.clone();
                                        p_cfg.base_url = lock.settings_base_url.clone();
                                        p_cfg.model = lock.settings_model.clone();
                                        self.config.providers.insert(p_name, p_cfg);

                                        // 同步更新快捷键配置
                                        if new_hotkey != self.config.selection.hotkey {
                                            self.config.selection.hotkey = new_hotkey.clone();
                                            lock.update_hotkey(&new_hotkey);
                                        }

                                        match self.config.save() {
                                            Ok(()) => {
                                                if let Some(ref tx) = lock.tx_command {
                                                    let _ = tx.send(GuiCommand::SaveConfig(self.config.clone()));
                                                }
                                                lock.test_feedback = Some((true, "配置保存成功".into()));
                                            }
                                            Err(error) => {
                                                lock.test_feedback = Some((false, format!("配置保存失败：{error}")));
                                            }
                                        }
                                    }
                                });
                            });

                            if let Some((ok, ref msg)) = lock.test_feedback {
                                ui.add_space(2.0);
                                ui.horizontal(|ui| {
                                    status_dot(ui, if ok { theme::OK } else { theme::ERR });
                                    ui.add(egui::Label::new(
                                        egui::RichText::new(msg)
                                            .size(theme::FS_ACTION)
                                            .color(if ok { theme::SUB } else { theme::ERR }),
                                    ).wrap());
                                });
                            }
                            });
                        });

                    return;
                }

                // Reserve bounded pane rectangles; long text scrolls inside them.
                let (content, _) = ui.allocate_exact_size(ui.available_size(), egui::Sense::hover());
                let splitter_h = 8.0;
                let cards_h = (content.height() - splitter_h).max(0.0);
                let ratio = lock.splitter_ratio.clamp(0.2, 0.8);
                let source_h = if lock.only_translation {
                    28.0
                } else {
                    (cards_h * ratio).clamp(56.0, (cards_h - 56.0).max(56.0))
                };
                let source_rect = egui::Rect::from_min_size(content.min, egui::vec2(content.width(), source_h));
                let splitter_rect = egui::Rect::from_min_size(
                    egui::pos2(content.left(), source_rect.bottom()),
                    egui::vec2(content.width(), splitter_h),
                );
                let translation_rect = egui::Rect::from_min_max(
                    egui::pos2(content.left(), splitter_rect.bottom()), content.max,
                );
                ui.scope_builder(egui::UiBuilder::new().max_rect(source_rect.shrink2(egui::vec2(12.0, 2.0))), |ui| {
                    let footer_rect = pane_footer_rect(ui.max_rect());
                    if !lock.only_translation {
                        let edit_h = (footer_rect.top() - ui.max_rect().top() - 4.0).max(0.0);
                        let response = egui::ScrollArea::vertical()
                            .id_salt("source_content_scroll")
                            .min_scrolled_height(0.0)
                            .auto_shrink([false, false])
                            .max_height(edit_h)
                            .show(ui, |ui| {
                                ui.add(egui::TextEdit::multiline(&mut lock.edit_text)
                                    .hint_text("输入或划选文本，按回车翻译")
                                    .desired_width(f32::INFINITY)
                                    .desired_rows(1)
                                    .font(egui::FontId::proportional(13.0))
                                    .text_color(theme::INK)
                                    .frame(egui::Frame::NONE))
                            }).inner;
                        if response.has_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter) && !i.modifiers.shift) {
                            trigger_retranslate = true;
                        }
                    }
                    footer_row(ui, footer_rect, "source_footer", |ui| {
                        if text_button(ui, if lock.only_translation { "展开原文" } else { "收起原文" }, false).clicked() {
                            lock.only_translation = !lock.only_translation;
                        }
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            ui.spacing_mut().item_spacing.x = 2.0;
                            if text_button(ui, "翻译", true).clicked() { trigger_retranslate = true; }
                            if text_button(ui, "复制", false).on_hover_text("复制原文").clicked() {
                                if let Ok(mut clipboard) = arboard::Clipboard::new() {
                                    let _ = clipboard.set_text(&lock.edit_text);
                                }
                            }
                            if !lock.only_translation {
                                if text_button(ui, "清空", false).clicked() {
                                    lock.edit_text.clear();
                                    lock.status = TranslationStatus::Idle;
                                }
                                if has_english_text(&lock.edit_text) {
                                    if text_button(ui, "US", false).on_hover_text("美音").clicked() { lock.play_tts(&lock.edit_text, Accent::Us); }
                                    if text_button(ui, "UK", false).on_hover_text("英音").clicked() { lock.play_tts(&lock.edit_text, Accent::Uk); }
                                }
                            }
                        });
                    });
                });

                let splitter_resp = ui.interact(splitter_rect, ui.id().with("pane_splitter"), egui::Sense::drag());
                if splitter_resp.hovered() || self.is_resizing_splitter {
                    ui.ctx().set_cursor_icon(egui::CursorIcon::ResizeVertical);
                }
                if splitter_resp.drag_started() { self.is_resizing_splitter = true; }
                if self.is_resizing_splitter {
                    if ui.input(|i| i.pointer.any_down()) {
                        lock.only_translation = false;
                        lock.splitter_ratio = (lock.splitter_ratio + ui.input(|i| i.pointer.delta().y) / cards_h.max(1.0)).clamp(0.2, 0.8);
                        self.config.ui.splitter_ratio = lock.splitter_ratio;
                    } else {
                        self.is_resizing_splitter = false;
                        let _ = self.config.save();
                    }
                }
                if splitter_resp.double_clicked() {
                    lock.splitter_ratio = crate::config::UIConfig::default().splitter_ratio;
                    self.config.ui.splitter_ratio = lock.splitter_ratio;
                    let _ = self.config.save();
                }
                ui.painter().hline((content.left() + 12.0)..=(content.right() - 12.0), splitter_rect.center().y,
                    egui::Stroke::new(1.0, theme::LINE));
                ui.painter().rect_filled(egui::Rect::from_center_size(splitter_rect.center(), egui::vec2(30.0, 3.0)), 2.0, theme::LINE_STRONG);

                ui.scope_builder(egui::UiBuilder::new().max_rect(translation_rect.shrink2(egui::vec2(12.0, 2.0))), |ui| {
                    let footer_rect = pane_footer_rect(ui.max_rect());
                    let scroll_h = (footer_rect.top() - ui.max_rect().top() - 4.0).max(0.0);
                    egui::ScrollArea::vertical().id_salt("translation_content_scroll")
                        // The default minimum scroll height exceeds a compact pane's
                        // text budget; honor the explicit footer reservation instead.
                        .min_scrolled_height(0.0)
                        .auto_shrink([false, false]).max_height(scroll_h).show(ui, |ui| {
                            match &status {
                                TranslationStatus::Idle => { ui.add(egui::Label::new(egui::RichText::new("划选文本，或输入后回车翻译").color(theme::FAINT)).wrap()); }
                                TranslationStatus::Loading { .. } => {
                                    ui.horizontal(|ui| { ui.spinner(); ui.colored_label(theme::SUB, "正在翻译…"); });
                                    ui.ctx().request_repaint_after(Duration::from_millis(80));
                                }
                                TranslationStatus::Success(result) => {
                                    ui.add(egui::Label::new(egui::RichText::new(&result.translated_text).size(theme::FS_TRANS).color(theme::INK)).wrap());
                                    if let Some(phonetic) = &result.phonetic {
                                        ui.add(egui::Label::new(egui::RichText::new(phonetic).size(theme::FS_ACTION).color(theme::SUB)).wrap());
                                    }
                                }
                                TranslationStatus::Error { message, .. } => {
                                    ui.add(egui::Label::new(egui::RichText::new(format!("翻译异常：{message}")).color(theme::ERR)).wrap());
                                    if text_button(ui, "重试", true).clicked() { trigger_retranslate = true; }
                                }
                            }
                        });
                    footer_row(ui, footer_rect, "translation_footer", |ui| {
                        let label = match &status {
                            TranslationStatus::Success(result) if result.from_cache => "缓存".into(),
                            TranslationStatus::Success(result) => format!("{:.0} ms", result.latency_ms),
                            TranslationStatus::Loading { .. } => "翻译中".into(),
                            TranslationStatus::Error { .. } => "失败".into(),
                            TranslationStatus::Idle => "就绪".into(),
                        };
                        let label = ui.label(egui::RichText::new(label).size(theme::FS_ACTION).color(theme::SUB));
                        record_control(ui, "translation_status", label.rect);
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            ui.spacing_mut().item_spacing.x = 2.0;
                            if text_button(ui, if is_copied { "已复制" } else { "复制译文" }, true).clicked() {
                                let text = match &status {
                                    TranslationStatus::Success(result) => result.translated_text.clone(),
                                    _ => lock.edit_text.clone(),
                                };
                                if !text.is_empty() {
                                    if let Ok(mut clipboard) = arboard::Clipboard::new() {
                                        if clipboard.set_text(text).is_ok() {
                                            lock.copy_feedback_time = Some(std::time::Instant::now());
                                        }
                                    }
                                }
                            }
                            if let TranslationStatus::Success(result) = &status {
                                if has_english_text(&result.translated_text) {
                                    if text_button(ui, "US", false).on_hover_text("美音").clicked() { lock.play_tts(&result.translated_text, Accent::Us); }
                                    if text_button(ui, "UK", false).on_hover_text("英音").clicked() { lock.play_tts(&result.translated_text, Accent::Uk); }
                                }
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

const HEADER_HEIGHT: f32 = 28.0;
const FOOTER_HEIGHT: f32 = 24.0;

fn current_window_position(state: &SharedPopupState) -> (f32, f32) {
    #[cfg(target_os = "linux")]
    if let Some(window) = state.x11_window {
        if let Some((x, y)) = crate::gui::get_window_position_x11(window) {
            return (x as f32, y as f32);
        }
    }
    #[cfg(windows)]
    if let Some(hwnd) = state.win32_hwnd {
        if let Some((pos, _)) = crate::gui::get_window_geometry_win32(hwnd) {
            return pos;
        }
    }
    state.window_pos.unwrap_or((0.0, 0.0))
}

// Private geometry observations let tests click the real widgets, without a UI mode.
fn record_control(_ui: &egui::Ui, _name: &str, _rect: egui::Rect) {
    #[cfg(test)]
    _ui.ctx()
        .data_mut(|data| data.insert_temp(egui::Id::new(("popup_control", _name)), _rect));
}

fn language_selector(
    ui: &mut egui::Ui,
    name: &str,
    lang: &mut String,
    auto: bool,
) -> egui::Response {
    let width = if auto { 64.0 } else { 52.0 };
    let (rect, mut response) =
        ui.allocate_exact_size(egui::vec2(width, HEADER_HEIGHT), egui::Sense::click());
    record_control(ui, name, rect);
    if response.hovered() {
        ui.painter().rect_filled(rect, 6.0, theme::FILL);
    }
    let galley = ui.painter().layout_no_wrap(
        lang_display_short(lang).into(),
        egui::FontId::proportional(theme::FS_BODY),
        theme::INK,
    );
    let text_rect = egui::Rect::from_min_max(
        rect.min + egui::vec2(6.0, 0.0),
        rect.max - egui::vec2(20.0, 0.0),
    );
    ui.painter()
        .galley(text_rect.center() - galley.size() * 0.5, galley, theme::INK);
    let arrow = egui::pos2(rect.right() - 11.0, rect.center().y);
    ui.painter().add(egui::Shape::convex_polygon(
        vec![
            arrow + egui::vec2(-3.5, -2.0),
            arrow + egui::vec2(3.5, -2.0),
            arrow + egui::vec2(0.0, 2.0),
        ],
        theme::SUB,
        egui::Stroke::NONE,
    ));
    let before = lang.clone();
    egui::Popup::menu(&response)
        .id(ui.make_persistent_id(("language_menu", name)))
        .width(160.0)
        .show(|ui| {
            if auto {
                ui.selectable_value(lang, "auto".into(), "自动检测");
            }
            for (code, label) in COMMON_LANGS {
                ui.selectable_value(lang, code.into(), label);
            }
        });
    if *lang != before {
        response.mark_changed();
    }
    response
}

fn pane_footer_rect(rect: egui::Rect) -> egui::Rect {
    egui::Rect::from_min_max(
        egui::pos2(rect.left(), rect.bottom() - FOOTER_HEIGHT),
        rect.max,
    )
}

fn footer_row(
    ui: &mut egui::Ui,
    rect: egui::Rect,
    name: &str,
    contents: impl FnOnce(&mut egui::Ui),
) {
    record_control(ui, name, rect);
    ui.scope_builder(
        egui::UiBuilder::new()
            .max_rect(rect)
            .layout(egui::Layout::left_to_right(egui::Align::Center)),
        |ui| {
            ui.set_height(FOOTER_HEIGHT);
            ui.spacing_mut().item_spacing.y = 0.0;
            contents(ui);
        },
    );
}

fn sync_window_level(
    last_pinned: &mut Option<bool>,
    ctx: &egui::Context,
    state: &SharedPopupState,
) {
    if *last_pinned == Some(state.is_pinned) {
        return;
    }
    let level = if state.is_pinned {
        egui::WindowLevel::AlwaysOnTop
    } else {
        egui::WindowLevel::Normal
    };
    ctx.send_viewport_cmd(egui::ViewportCommand::WindowLevel(level));
    crate::gui::set_always_on_top(state.x11_window, state.win32_hwnd, state.is_pinned);
    *last_pinned = Some(state.is_pinned);
}

fn sync_worker_config(state: &SharedPopupState, config: &AppConfig) {
    if let Some(tx) = &state.tx_command {
        let _ = tx.send(GuiCommand::SaveConfig(config.clone()));
    }
}

fn persist_ui_config(state: &SharedPopupState, config: &AppConfig) {
    if state.tx_command.is_some() {
        sync_worker_config(state, config);
    } else if !cfg!(test) {
        let _ = config.save();
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

fn mode_label(mode: SelectionMode) -> &'static str {
    match mode {
        SelectionMode::Automatic => "划选翻译",
        SelectionMode::Companion => "按需划词",
        SelectionMode::Manual => "手动模式",
    }
}

const COMMON_LANGS: [(&str, &str); 8] = [
    ("zh-CN", "简体中文"),
    ("en", "英语 (English)"),
    ("ja", "日语 (日本語)"),
    ("ko", "韩语 (Korean)"),
    ("fr", "法语 (Français)"),
    ("de", "德语 (Deutsch)"),
    ("es", "西语 (Español)"),
    ("ru", "俄语 (Русский)"),
];

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

mod theme {
    use eframe::egui::Color32;

    pub const SURFACE: Color32 = Color32::WHITE;
    pub const INK: Color32 = Color32::from_rgb(17, 24, 39);
    pub const SUB: Color32 = Color32::from_rgb(107, 114, 128);
    pub const FAINT: Color32 = Color32::from_rgb(156, 163, 175);
    pub const LINE: Color32 = Color32::from_rgb(233, 234, 236);
    pub const LINE_STRONG: Color32 = Color32::from_rgb(223, 225, 229);
    pub const FILL: Color32 = Color32::from_rgb(245, 246, 248);
    pub const ACTIVE_FILL: Color32 = Color32::from_rgb(219, 234, 254);
    pub const ACTIVE: Color32 = Color32::from_rgb(37, 99, 235);
    pub const OK: Color32 = Color32::from_rgb(16, 185, 129);
    pub const ERR: Color32 = Color32::from_rgb(239, 68, 68);

    pub const FS_ACTION: f32 = 11.5;
    pub const FS_BODY: f32 = 12.5;
    pub const FS_TRANS: f32 = 16.0;
}

#[derive(Clone, Copy)]
enum Icon {
    Pin,
    Sliders,
    Close,
    Swap,
}

fn paint_icon(
    painter: &egui::Painter,
    rect: egui::Rect,
    icon: Icon,
    color: egui::Color32,
    active: bool,
) {
    let k = rect.width().min(rect.height()) / 16.0;
    let p = |x: f32, y: f32| egui::pos2(rect.min.x + x * k, rect.min.y + y * k);
    let stroke = egui::Stroke::new(1.6, color);
    match icon {
        Icon::Close => {
            painter.line_segment([p(4.5, 4.5), p(11.5, 11.5)], stroke);
            painter.line_segment([p(11.5, 4.5), p(4.5, 11.5)], stroke);
        }
        Icon::Swap => {
            painter.line_segment([p(2.0, 5.5), p(13.0, 5.5)], stroke);
            painter.line_segment([p(10.8, 3.3), p(13.0, 5.5)], stroke);
            painter.line_segment([p(10.8, 7.7), p(13.0, 5.5)], stroke);
            painter.line_segment([p(14.0, 10.5), p(3.0, 10.5)], stroke);
            painter.line_segment([p(5.2, 8.3), p(3.0, 10.5)], stroke);
            painter.line_segment([p(5.2, 12.7), p(3.0, 10.5)], stroke);
        }
        Icon::Sliders => {
            painter.line_segment([p(2.0, 5.5), p(14.0, 5.5)], stroke);
            painter.line_segment([p(2.0, 10.5), p(14.0, 10.5)], stroke);
            painter.circle_filled(p(10.0, 5.5), 1.7 * k.max(1.0), color);
            painter.circle_filled(p(6.0, 10.5), 1.7 * k.max(1.0), color);
        }
        Icon::Pin => {
            let pin = |x: f32, y: f32| {
                if active {
                    p(x, y)
                } else {
                    let (dx, dy) = (x - 8.0, y - 8.0);
                    p(8.0 + (dx + dy) * 0.70710677, 8.0 + (dy - dx) * 0.70710677)
                }
            };
            let head = vec![
                pin(5.0, 2.5),
                pin(11.0, 2.5),
                pin(10.0, 4.0),
                pin(10.0, 7.0),
                pin(12.0, 9.0),
                pin(4.0, 9.0),
                pin(6.0, 7.0),
                pin(6.0, 4.0),
            ];
            if active {
                for points in [
                    vec![pin(5.0, 2.5), pin(11.0, 2.5), pin(11.0, 4.0), pin(5.0, 4.0)],
                    vec![pin(6.0, 4.0), pin(10.0, 4.0), pin(10.0, 7.0), pin(6.0, 7.0)],
                    vec![pin(6.0, 7.0), pin(10.0, 7.0), pin(12.0, 9.0), pin(4.0, 9.0)],
                ] {
                    painter.add(egui::Shape::convex_polygon(
                        points,
                        color,
                        egui::Stroke::NONE,
                    ));
                }
            } else {
                painter.add(egui::Shape::closed_line(head, stroke));
            }
            painter.line_segment([pin(8.0, 9.0), pin(8.0, 14.0)], stroke);
        }
    }
}

fn icon_button(
    ui: &mut egui::Ui,
    icon: Icon,
    active: bool,
    enabled: bool,
    tooltip: &str,
) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(
        egui::vec2(HEADER_HEIGHT, HEADER_HEIGHT),
        egui::Sense::click(),
    );
    record_control(
        ui,
        match icon {
            Icon::Swap => "swap",
            Icon::Pin => "pin",
            Icon::Sliders => "settings",
            Icon::Close => "close",
        },
        rect,
    );
    if ui.is_rect_visible(rect) {
        if enabled && active {
            ui.painter().rect_filled(rect, 6.0, theme::ACTIVE_FILL);
            ui.painter().rect_stroke(
                rect,
                6.0,
                egui::Stroke::new(1.0, theme::ACTIVE),
                egui::StrokeKind::Inside,
            );
        } else if enabled && resp.hovered() {
            ui.painter().rect_filled(rect, 6.0, theme::FILL);
        }
        let color = if !enabled {
            theme::FAINT
        } else if active {
            theme::ACTIVE
        } else if resp.hovered() {
            theme::INK
        } else {
            theme::SUB
        };
        paint_icon(
            ui.painter(),
            egui::Rect::from_center_size(rect.center(), egui::vec2(16.0, 16.0)),
            icon,
            color,
            active,
        );
    }
    resp.on_hover_text(tooltip)
}

fn text_button(ui: &mut egui::Ui, label: &str, strong: bool) -> egui::Response {
    let color = if strong { theme::INK } else { theme::SUB };
    let galley = ui.painter().layout_no_wrap(
        label.to_owned(),
        egui::FontId::proportional(theme::FS_ACTION),
        color,
    );
    let size = egui::vec2(galley.size().x + 12.0, FOOTER_HEIGHT);
    let (rect, resp) = ui.allocate_exact_size(size, egui::Sense::click());
    record_control(ui, label, rect);
    if ui.is_rect_visible(rect) {
        if resp.hovered() {
            ui.painter().rect_filled(rect, 5.0, theme::FILL);
        }
        ui.painter()
            .galley(rect.center() - galley.size() * 0.5, galley, color);
    }
    resp
}

fn setting_label(ui: &mut egui::Ui, text: &str) {
    ui.allocate_ui_with_layout(
        egui::vec2(66.0, 20.0),
        egui::Layout::left_to_right(egui::Align::Center),
        |ui| {
            ui.label(
                egui::RichText::new(text)
                    .size(theme::FS_ACTION)
                    .color(theme::SUB),
            );
        },
    );
}

fn status_dot(ui: &mut egui::Ui, color: egui::Color32) {
    let (rect, _) = ui.allocate_exact_size(egui::vec2(8.0, 8.0), egui::Sense::hover());
    ui.painter().circle_filled(rect.center(), 3.0, color);
}

#[cfg(test)]
mod layout_tests {
    use super::*;
    use crate::models::TranslationResult;

    fn render(
        size: egui::Vec2,
        status: TranslationStatus,
        settings: bool,
        ratio: f32,
        collapsed: bool,
    ) -> (egui::FullOutput, egui::Rect) {
        let mut config = AppConfig::default();
        config.ui.splitter_ratio = ratio;
        let mut state = SharedPopupState::new(&config);
        state.is_visible = true;
        state.show_settings = settings;
        state.only_translation = collapsed;
        state.edit_text = "Long source text with words that wrap and scroll.\n".repeat(100);
        state.settings_base_url = "https://example.com/".repeat(50);
        state.test_feedback = Some((false, "Long provider failure feedback. ".repeat(100)));
        state.hotkey_feedback = Some((
            false,
            "Registration feedback with a long explanation. ".repeat(20),
        ));
        state.status = status;
        let mut app = PopupApp::new(Arc::new(Mutex::new(state)), config);
        let ctx = egui::Context::default();
        let mut used = egui::Rect::NOTHING;
        let mut run = || {
            ctx.run_ui(
                egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, size)),
                    ..Default::default()
                },
                |ui| {
                    app.render(ui);
                    used = ui.min_rect();
                },
            )
        };
        run().drop_without_applying_deltas();
        let output = run();
        (output, used)
    }

    fn texts(output: &egui::FullOutput) -> Vec<(String, egui::Rect)> {
        fn collect(shape: &egui::Shape, result: &mut Vec<(String, egui::Rect)>) {
            match shape {
                egui::Shape::Text(text) => result.push((
                    text.galley.text().to_string(),
                    egui::Rect::from_min_size(text.pos, text.galley.size()),
                )),
                egui::Shape::Vec(shapes) => {
                    for shape in shapes {
                        collect(shape, result);
                    }
                }
                _ => {}
            }
        }
        let mut result = Vec::new();
        for shape in &output.shapes {
            collect(&shape.shape, &mut result);
        }
        result
    }

    fn control_rect(ctx: &egui::Context, name: &str) -> egui::Rect {
        ctx.data(|data| data.get_temp(egui::Id::new(("popup_control", name))))
            .unwrap_or_else(|| panic!("missing control geometry: {name}"))
    }

    fn interaction_frame(
        app: &mut PopupApp,
        ctx: &egui::Context,
        events: Vec<egui::Event>,
    ) -> egui::FullOutput {
        ctx.run_ui(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(380.0, 280.0),
                )),
                events,
                ..Default::default()
            },
            |ui| app.render(ui),
        )
    }

    fn interactive_app(
        config: AppConfig,
        mut state: SharedPopupState,
    ) -> (PopupApp, egui::Context) {
        state.is_visible = true;
        let mut app = PopupApp::new(Arc::new(Mutex::new(state)), config);
        // Avoid mapping a native window in this headless interaction harness.
        app.last_visible = Some(true);
        app.initial_sized = true;
        let ctx = egui::Context::default();
        interaction_frame(&mut app, &ctx, vec![]).drop_without_applying_deltas();
        interaction_frame(&mut app, &ctx, vec![]).drop_without_applying_deltas();
        (app, ctx)
    }

    fn click_control(app: &mut PopupApp, ctx: &egui::Context, name: &str) {
        let pos = control_rect(ctx, name).center();
        for pressed in [true, false] {
            interaction_frame(
                app,
                ctx,
                vec![
                    egui::Event::PointerMoved(pos),
                    egui::Event::PointerButton {
                        pos,
                        button: egui::PointerButton::Primary,
                        pressed,
                        modifiers: egui::Modifiers::NONE,
                    },
                ],
            )
            .drop_without_applying_deltas();
        }
        interaction_frame(app, ctx, vec![]).drop_without_applying_deltas();
    }

    #[test]
    fn pointer_click_swap_in_auto_mode_reverses_result_and_sends_one_translation() {
        let config = AppConfig::default();
        let mut state = SharedPopupState::new(&config);
        state.edit_text = "Hello world".into();
        state.status = TranslationStatus::Success(TranslationResult {
            translated_text: "你好，世界".into(),
            detected_lang: Some("en".into()),
            ..TranslationResult::error("Hello world", "unused", "microsoft")
        });
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        state.tx_command = Some(tx);
        let (mut app, ctx) = interactive_app(config, state);
        click_control(&mut app, &ctx, "swap");
        {
            let state = app.state.lock().unwrap();
            assert_eq!((&*state.source_lang, &*state.target_lang), ("zh-CN", "en"));
            assert_eq!(state.edit_text, "你好，世界");
            assert!(
                matches!(&state.status, TranslationStatus::Loading { original_text } if original_text == "你好，世界")
            );
        }
        assert_eq!(
            (&*app.config.source_lang, &*app.config.target_lang),
            ("zh-CN", "en")
        );
        let mut saves = 0;
        let mut translations = 0;
        while let Ok(command) = rx.try_recv() {
            match command {
                GuiCommand::SaveConfig(config) => {
                    saves += 1;
                    assert_eq!(
                        (config.source_lang.as_str(), config.target_lang.as_str()),
                        ("zh-CN", "en")
                    );
                }
                GuiCommand::Translate {
                    text,
                    source_lang,
                    target_lang,
                    ..
                } => {
                    translations += 1;
                    assert_eq!(
                        (text.as_str(), source_lang.as_str(), target_lang.as_str()),
                        ("你好，世界", "zh-CN", "en")
                    );
                }
                other => panic!("unexpected command: {other:?}"),
            }
        }
        assert_eq!((saves, translations), (1, 1));
    }

    #[test]
    fn pointer_click_swap_with_empty_auto_input_changes_direction_without_translation() {
        let config = AppConfig::default();
        let mut state = SharedPopupState::new(&config);
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        state.tx_command = Some(tx);
        let (mut app, ctx) = interactive_app(config, state);
        click_control(&mut app, &ctx, "swap");
        let state = app.state.lock().unwrap();
        assert_eq!((&*state.source_lang, &*state.target_lang), ("zh-CN", "en"));
        assert_eq!(state.status, TranslationStatus::Idle);
        assert!(matches!(rx.try_recv(), Ok(GuiCommand::SaveConfig(_))));
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn pointer_click_pin_twice_restores_state_and_preserves_geometry_across_hide() {
        let config = AppConfig::default();
        let mut state = SharedPopupState::new(&config);
        state.window_pos = Some((321.0, 187.0));
        state.window_size = (760.0, 560.0);
        let (mut app, ctx) = interactive_app(config, state);
        click_control(&mut app, &ctx, "pin");
        assert!(app.state.lock().unwrap().is_pinned);
        let output = interaction_frame(&mut app, &ctx, vec![]);
        let pin = control_rect(&ctx, "pin");
        assert!(output.shapes.iter().any(|shape| matches!(&shape.shape,
            egui::Shape::Rect(rect) if rect.rect == pin && rect.fill == theme::ACTIVE_FILL)));
        output.drop_without_applying_deltas();
        {
            let mut state = app.state.lock().unwrap();
            state.should_update_pos = true;
            state.is_visible = false;
        }
        interaction_frame(&mut app, &ctx, vec![]).drop_without_applying_deltas();
        app.state.lock().unwrap().is_visible = true;
        interaction_frame(&mut app, &ctx, vec![]).drop_without_applying_deltas();
        {
            let state = app.state.lock().unwrap();
            assert!(state.is_pinned);
            assert!(!state.should_update_pos);
            assert_eq!(state.window_pos, Some((321.0, 187.0)));
            assert_eq!(state.window_size, (760.0, 560.0));
        }
        click_control(&mut app, &ctx, "pin");
        let state = app.state.lock().unwrap();
        assert!(!state.is_pinned);
        assert!(!state.should_update_pos);
        assert_eq!(state.window_pos, Some((321.0, 187.0)));
        assert_eq!(state.window_size, (760.0, 560.0));
    }

    #[test]
    fn pin_toggles_window_level_saves_config_and_guards_focus_loss() {
        let config = AppConfig::default();
        let mut state = SharedPopupState::new(&config);
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        state.tx_command = Some(tx);
        state.window_pos = Some((240.0, 180.0));
        state.window_size = (520.0, 340.0);
        let (mut app, ctx) = interactive_app(config, state);

        // Pinning emits AlwaysOnTop and saves pinned config + geometry on the click frame.
        let pin_pos = control_rect(&ctx, "pin").center();
        interaction_frame(
            &mut app,
            &ctx,
            vec![
                egui::Event::PointerMoved(pin_pos),
                egui::Event::PointerButton {
                    pos: pin_pos,
                    button: egui::PointerButton::Primary,
                    pressed: true,
                    modifiers: egui::Modifiers::NONE,
                },
            ],
        )
        .drop_without_applying_deltas();
        let out = interaction_frame(
            &mut app,
            &ctx,
            vec![
                egui::Event::PointerMoved(pin_pos),
                egui::Event::PointerButton {
                    pos: pin_pos,
                    button: egui::PointerButton::Primary,
                    pressed: false,
                    modifiers: egui::Modifiers::NONE,
                },
            ],
        );
        let commands = &out
            .viewport_output
            .get(&egui::ViewportId::ROOT)
            .unwrap()
            .commands;
        assert!(commands.contains(&egui::ViewportCommand::WindowLevel(
            egui::WindowLevel::AlwaysOnTop
        )));
        out.drop_without_applying_deltas();
        assert!(matches!(
            rx.try_recv(),
            Ok(GuiCommand::SaveConfig(cfg))
                if cfg.ui.pinned
                    && cfg.ui.pinned_pos == Some((240.0, 180.0))
                    && cfg.ui.pinned_size == Some((520.0, 340.0))
        ));

        // Simulate past grace period and focus loss while pinned -> stays visible.
        app.state.lock().unwrap().last_shown_at =
            Some(std::time::Instant::now() - Duration::from_millis(500));
        ctx.run_ui(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(380.0, 280.0),
                )),
                focused: false,
                ..Default::default()
            },
            |ui| app.render(ui),
        )
        .drop_without_applying_deltas();
        assert!(app.state.lock().unwrap().is_visible);

        // Unpin emits Normal window level, saves pinned=false, and focus loss hides.
        let pin_pos = control_rect(&ctx, "pin").center();
        interaction_frame(
            &mut app,
            &ctx,
            vec![
                egui::Event::PointerMoved(pin_pos),
                egui::Event::PointerButton {
                    pos: pin_pos,
                    button: egui::PointerButton::Primary,
                    pressed: true,
                    modifiers: egui::Modifiers::NONE,
                },
            ],
        )
        .drop_without_applying_deltas();
        let out = interaction_frame(
            &mut app,
            &ctx,
            vec![
                egui::Event::PointerMoved(pin_pos),
                egui::Event::PointerButton {
                    pos: pin_pos,
                    button: egui::PointerButton::Primary,
                    pressed: false,
                    modifiers: egui::Modifiers::NONE,
                },
            ],
        );
        let commands = &out
            .viewport_output
            .get(&egui::ViewportId::ROOT)
            .unwrap()
            .commands;
        assert!(commands.contains(&egui::ViewportCommand::WindowLevel(
            egui::WindowLevel::Normal
        )));
        out.drop_without_applying_deltas();
        assert!(matches!(
            rx.try_recv(),
            Ok(GuiCommand::SaveConfig(cfg)) if !cfg.ui.pinned
        ));

        app.state.lock().unwrap().last_shown_at =
            Some(std::time::Instant::now() - Duration::from_millis(500));
        ctx.run_ui(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(380.0, 280.0),
                )),
                focused: false,
                ..Default::default()
            },
            |ui| app.render(ui),
        )
        .drop_without_applying_deltas();
        assert!(!app.state.lock().unwrap().is_visible);
    }

    #[test]
    fn header_and_footer_controls_share_vertical_centers() {
        let config = AppConfig::default();
        let mut state = SharedPopupState::new(&config);
        state.edit_text = "Hello world".into();
        state.status = TranslationStatus::Success(TranslationResult {
            translated_text: "Translated example".into(),
            ..TranslationResult::error("Hello world", "unused", "microsoft")
        });
        let (mut app, ctx) = interactive_app(config, state);
        let output = interaction_frame(&mut app, &ctx, vec![]);
        let header_y = control_rect(&ctx, "source").center().y;
        for name in [
            "source", "swap", "target", "pin", "settings", "close", "drag",
        ] {
            let rect = control_rect(&ctx, name);
            assert!(
                (rect.height() - HEADER_HEIGHT).abs() < 0.1,
                "{name}: {rect:?}"
            );
            assert!((rect.center().y - header_y).abs() < 0.1, "{name}: {rect:?}");
        }
        let drag = control_rect(&ctx, "drag");
        assert!(drag.width() >= 24.0);
        for name in ["source", "swap", "target", "pin", "settings", "close"] {
            assert!(
                !drag.intersects(control_rect(&ctx, name)),
                "drag overlaps {name}"
            );
        }
        for (row, names) in [
            ("source_footer", vec!["收起原文", "清空", "复制", "翻译"]),
            (
                "translation_footer",
                vec!["translation_status", "复制译文", "UK", "US"],
            ),
        ] {
            let row_rect = control_rect(&ctx, row);
            assert!((row_rect.height() - FOOTER_HEIGHT).abs() < 0.1);
            for name in names {
                let rect = control_rect(&ctx, name);
                assert!(
                    (rect.center().y - row_rect.center().y).abs() < 0.1,
                    "{row}/{name}: {rect:?}, row={row_rect:?}"
                );
            }
        }
        let text = texts(&output);
        for (label, rect) in &text {
            let expected_y = match label.as_str() {
                "自动" | "中" => Some(header_y),
                "收起原文" | "清空" | "复制" | "翻译" => {
                    Some(control_rect(&ctx, "source_footer").center().y)
                }
                "复制译文" | "0 ms" => {
                    Some(control_rect(&ctx, "translation_footer").center().y)
                }
                _ => None,
            };
            if let Some(y) = expected_y {
                assert!(
                    (rect.center().y - y).abs() < 0.6,
                    "text {label}: {rect:?}, center={y}"
                );
            }
        }
        output.drop_without_applying_deltas();
    }

    #[test]
    fn compact_main_controls_fit_with_long_content_and_no_provider_switch() {
        for size in [egui::vec2(380.0, 220.0), egui::vec2(500.0, 320.0)] {
            for ratio in [0.2, 0.33, 0.45, 0.8] {
                for collapsed in [false, true] {
                    let status = TranslationStatus::Success(TranslationResult {
                        translated_text: "Long translated text requiring scrolling. ".repeat(100),
                        provider: "PROVIDER_MUST_STAY_IN_SETTINGS".into(),
                        ..TranslationResult::error("source", "unused", "microsoft")
                    });
                    let (output, used) = render(size, status, false, ratio, collapsed);
                    let bounds = egui::Rect::from_min_size(egui::Pos2::ZERO, size).expand(0.5);
                    assert!(
                        bounds.contains_rect(used),
                        "{size:?} ratio={ratio} collapsed={collapsed}: {used:?}"
                    );
                    let text = texts(&output);
                    for label in ["翻译", "复制", "复制译文", "US", "UK", "0 ms"] {
                        let matches: Vec<_> =
                            text.iter().filter(|(value, _)| value == label).collect();
                        assert!(!matches.is_empty(), "missing {label}");
                        for (_, rect) in matches {
                            assert!(bounds.contains_rect(*rect), "{label}: {rect:?}");
                        }
                    }
                    for provider in [
                        "微软翻译",
                        "DeepSeek",
                        "OpenAI",
                        "智谱GLM",
                        "本地Ollama",
                        "PROVIDER_MUST_STAY_IN_SETTINGS",
                    ] {
                        assert!(
                            !text.iter().any(|(value, _)| value.contains(provider)),
                            "main UI exposed {provider}"
                        );
                    }
                    output.drop_without_applying_deltas();
                }
            }
        }
    }

    #[test]
    fn settings_with_long_feedback_scrolls_inside_compact_window() {
        let size = egui::vec2(380.0, 220.0);
        let (output, used) = render(size, TranslationStatus::Idle, true, 0.33, false);
        let bounds = egui::Rect::from_min_size(egui::Pos2::ZERO, size).expand(0.5);
        assert!(bounds.contains_rect(used), "{used:?}");
        for label in ["设置", "返回", "微软翻译"] {
            let text = texts(&output);
            assert!(
                text.iter()
                    .any(|(value, rect)| value == label && bounds.contains_rect(*rect)),
                "missing visible {label}"
            );
        }
        output.drop_without_applying_deltas();
    }

    #[test]
    fn long_error_text_does_not_push_footer_outside_window() {
        let size = egui::vec2(380.0, 220.0);
        let (output, used) = render(
            size,
            TranslationStatus::Error {
                original_text: "source".into(),
                message: "Long error message ".repeat(200),
            },
            false,
            0.33,
            false,
        );
        let bounds = egui::Rect::from_min_size(egui::Pos2::ZERO, size).expand(0.5);
        assert!(bounds.contains_rect(used), "{used:?}");
        assert!(
            texts(&output)
                .iter()
                .any(|(value, rect)| value == "复制译文" && bounds.contains_rect(*rect))
        );
        output.drop_without_applying_deltas();
    }
}
