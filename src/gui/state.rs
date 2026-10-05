use std::time::Instant;
use crate::models::TranslationResult;

#[derive(Debug, Clone, PartialEq)]
pub enum TranslationStatus {
    Idle,
    Loading {
        original_text: String,
    },
    Success(TranslationResult),
    Error {
        original_text: String,
        message: String,
    },
}

#[derive(Clone)]
pub struct SharedPopupState {
    pub is_visible: bool,
    pub is_pinned: bool,
    pub status: TranslationStatus,
    pub cursor_pos: (i32, i32),
    pub window_pos: Option<(f32, f32)>,
    pub should_update_pos: bool,
    pub active_provider: String,
    pub source_lang: String,
    pub target_lang: String,
    pub copy_feedback_time: Option<Instant>,
    pub ctx: Option<eframe::egui::Context>,
}

impl SharedPopupState {
    pub fn new(active_provider: String, target_lang: String) -> Self {
        Self {
            is_visible: false,
            is_pinned: false,
            status: TranslationStatus::Idle,
            cursor_pos: (0, 0),
            window_pos: None,
            should_update_pos: false,
            active_provider,
            source_lang: "auto".into(),
            target_lang,
            copy_feedback_time: None,
            ctx: None,
        }
    }

    pub fn request_repaint(&self) {
        if let Some(ref c) = self.ctx {
            c.request_repaint();
        }
    }

    /// 根据光标绝对坐标计算浮窗最佳显示位置，并做屏幕防溢出碰撞检测
    pub fn compute_target_pos(cursor_x: i32, cursor_y: i32, win_w: f32, win_h: f32) -> (f32, f32) {
        let mut x = cursor_x as f32 + 12.0;
        let mut y = cursor_y as f32 + 16.0;

        // 默认常规显示屏分辨率参考 (或通过 X11 获取真实分辨率)
        let screen_w = 1920.0;
        let screen_h = 1080.0;

        if x + win_w > screen_w - 20.0 {
            // 右侧越界，翻转至光标左侧
            x = (cursor_x as f32 - win_w - 12.0).max(10.0);
        }

        if y + win_h > screen_h - 40.0 {
            // 底部越界，翻转至光标上方
            y = (cursor_y as f32 - win_h - 16.0).max(10.0);
        }

        (x, y)
    }
}
