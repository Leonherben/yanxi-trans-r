use std::time::Instant;
use tokio::sync::mpsc::UnboundedSender;
use crate::config::{AppConfig, ProviderConfig, SelectionMode};
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

#[derive(Debug, Clone)]
pub enum GuiCommand {
    Translate {
        text: String,
        source_lang: String,
        target_lang: String,
        provider: String,
    },
    TestProvider {
        provider_name: String,
        provider_config: ProviderConfig,
    },
    SaveConfig(AppConfig),
}

#[derive(Clone)]
pub struct SharedPopupState {
    pub is_visible: bool,
    pub is_pinned: bool,
    pub selection_mode: SelectionMode,
    pub status: TranslationStatus,
    pub cursor_pos: (i32, i32),
    pub window_pos: Option<(f32, f32)>,
    pub should_update_pos: bool,

    // 原文编辑与查词
    pub edit_text: String,

    // 提供商与语言
    pub active_provider: String,
    pub source_lang: String,
    pub target_lang: String,
    pub available_providers: Vec<String>,

    // 设置面板状态
    pub show_settings: bool,
    pub settings_provider: String,
    pub settings_api_key: String,
    pub settings_base_url: String,
    pub settings_model: String,
    pub test_feedback: Option<(bool, String)>,

    // 交互与显示
    pub only_translation: bool,
    pub copy_feedback_time: Option<Instant>,
    pub ctx: Option<eframe::egui::Context>,
    pub tx_command: Option<UnboundedSender<GuiCommand>>,
}

impl SharedPopupState {
    pub fn new(config: &AppConfig) -> Self {
        let active_p = config.active_provider.clone();
        let p_cfg = config.providers.get(&active_p).cloned().unwrap_or_default();

        let mut available = vec![
            "microsoft".to_string(),
            "deepseek".to_string(),
            "openai".to_string(),
            "zhipu".to_string(),
            "custom".to_string(),
        ];
        // 补全任何自定义的 provider
        for k in config.providers.keys() {
            if !available.contains(k) {
                available.push(k.clone());
            }
        }

        Self {
            is_visible: false,
            is_pinned: false,
            selection_mode: config.selection.get_mode(),
            status: TranslationStatus::Idle,
            cursor_pos: (0, 0),
            window_pos: None,
            should_update_pos: false,
            edit_text: String::new(),
            active_provider: active_p.clone(),
            source_lang: config.source_lang.clone(),
            target_lang: config.target_lang.clone(),
            available_providers: available,
            show_settings: false,
            settings_provider: active_p,
            settings_api_key: p_cfg.api_key,
            settings_base_url: p_cfg.base_url,
            settings_model: p_cfg.model,
            test_feedback: None,
            only_translation: false,
            copy_feedback_time: None,
            ctx: None,
            tx_command: None,
        }
    }

    pub fn trigger_translate(&mut self, text: &str) {
        let trimmed = text.trim();
        if trimmed.is_empty() {
            return;
        }

        self.status = TranslationStatus::Loading {
            original_text: trimmed.to_string(),
        };
        self.request_repaint();

        if let Some(ref tx) = self.tx_command {
            let _ = tx.send(GuiCommand::Translate {
                text: trimmed.to_string(),
                source_lang: self.source_lang.clone(),
                target_lang: self.target_lang.clone(),
                provider: self.active_provider.clone(),
            });
        }
    }

    pub fn load_provider_settings(&mut self, config: &AppConfig, provider_name: &str) {
        self.settings_provider = provider_name.to_string();
        if let Some(cfg) = config.providers.get(provider_name) {
            self.settings_api_key = cfg.api_key.clone();
            self.settings_base_url = cfg.base_url.clone();
            self.settings_model = cfg.model.clone();
        } else {
            self.settings_api_key.clear();
            self.settings_base_url.clear();
            self.settings_model.clear();
        }
        self.test_feedback = None;
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

        let screen_w = 1920.0;
        let screen_h = 1080.0;

        if x + win_w > screen_w - 20.0 {
            x = (cursor_x as f32 - win_w - 12.0).max(10.0);
        }

        if y + win_h > screen_h - 40.0 {
            y = (cursor_y as f32 - win_h - 16.0).max(10.0);
        }

        (x, y)
    }
}
