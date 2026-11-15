use crate::config::{AppConfig, ProviderConfig, SelectionMode};
use crate::models::TranslationResult;
use crate::tts::Accent;
use std::time::Instant;
use tokio::sync::mpsc::UnboundedSender;

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
    PlayTts {
        text: String,
        accent: Accent,
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
    pub window_size: (f32, f32),
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
    pub settings_hotkey: String,
    pub test_feedback: Option<(bool, String)>,
    /// Set by the hotkey registration callback, independently of config saving.
    pub hotkey_feedback: Option<(bool, String)>,

    // 交互与显示
    pub splitter_ratio: f32,
    pub only_translation: bool,
    pub copy_feedback_time: Option<Instant>,
    pub ctx: Option<eframe::egui::Context>,
    pub tx_command: Option<UnboundedSender<GuiCommand>>,
    pub tx_hotkey: Option<std::sync::mpsc::Sender<String>>,
    pub x11_window: Option<u32>,
    pub screen_size: (f32, f32),
    pub request_seq: u64,
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
        for k in config.providers.keys() {
            if !available.contains(k) {
                available.push(k.clone());
            }
        }

        Self {
            is_visible: !config.ui.start_hidden,
            is_pinned: false,
            selection_mode: config.selection.get_mode(),
            status: TranslationStatus::Idle,
            cursor_pos: (0, 0),
            window_pos: None,
            window_size: (500.0, 320.0),
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
            settings_hotkey: config.selection.hotkey.clone(),
            test_feedback: None,
            hotkey_feedback: None,
            splitter_ratio: config.ui.splitter_ratio.clamp(0.2, 0.8),
            only_translation: false,
            copy_feedback_time: None,
            ctx: None,
            tx_command: None,
            tx_hotkey: None,
            x11_window: None,
            screen_size: (1920.0, 1080.0),
            request_seq: 0,
        }
    }

    pub fn trigger_translate(&mut self, text: &str) {
        let trimmed = match crate::selection::validate_selection_text(text) {
            Ok(text) => text,
            Err(message) => {
                self.status = TranslationStatus::Error {
                    original_text: text.to_string(),
                    message,
                };
                self.request_repaint();
                return;
            }
        };

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

    /// Reverse the actual translation direction, including automatic detection.
    /// A current successful translation becomes the input for the reverse query.
    pub fn swap_languages(&mut self) {
        let result = match &self.status {
            TranslationStatus::Success(result)
                if result.original_text.trim() == self.edit_text.trim() =>
            {
                Some(result.clone())
            }
            _ => None,
        };
        let source = if self.source_lang != "auto" {
            self.source_lang.clone()
        } else {
            result
                .as_ref()
                .and_then(|r| r.detected_lang.as_deref())
                .filter(|language| !language.is_empty() && *language != "auto")
                .map(normalize_language)
                .unwrap_or_else(|| infer_source_language(&self.edit_text, &self.target_lang))
        };
        let target = result
            .as_ref()
            .map(|r| r.target_lang.clone())
            .unwrap_or_else(|| {
                if self.source_lang == "auto" && self.target_lang == "zh-CN" && source == "zh-CN" {
                    "en".into()
                } else {
                    self.target_lang.clone()
                }
            });
        self.source_lang = target;
        self.target_lang = source;
        if let Some(result) = result {
            if !result.translated_text.trim().is_empty() {
                self.edit_text = result.translated_text;
            }
        }
        self.request_repaint();
    }

    pub fn toggle_pin(&mut self) {
        self.is_pinned = !self.is_pinned;
        // Discard any earlier automatic follow request. Manual movement remains available.
        self.should_update_pos = false;
        println!(
            "[Yanxi GUI] {}",
            if self.is_pinned {
                "窗口已固定"
            } else {
                "窗口已取消固定"
            }
        );
        self.request_repaint();
    }

    pub fn play_tts(&self, text: &str, accent: Accent) {
        let trimmed = text.trim();
        if trimmed.is_empty() {
            return;
        }
        if let Some(ref tx) = self.tx_command {
            let _ = tx.send(GuiCommand::PlayTts {
                text: trimmed.to_string(),
                accent,
            });
        }
    }

    pub fn update_hotkey(&mut self, new_hotkey: &str) {
        let trimmed = new_hotkey.trim().to_lowercase();
        self.hotkey_feedback = None;
        self.settings_hotkey = trimmed.clone();
        if let Some(ref tx) = self.tx_hotkey {
            let _ = tx.send(trimmed);
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
        self.settings_hotkey = config.selection.hotkey.clone();
        self.test_feedback = None;
    }

    pub fn request_repaint(&self) {
        if let Some(ref c) = self.ctx {
            c.request_repaint();
        }
    }

    /// 根据光标绝对坐标计算浮窗最佳显示位置，并做屏幕防溢出碰撞检测
    pub fn compute_target_pos(
        cursor_x: i32,
        cursor_y: i32,
        win_w: f32,
        win_h: f32,
        screen_w: f32,
        screen_h: f32,
    ) -> (f32, f32) {
        let mut x = cursor_x as f32 + 12.0;
        let mut y = cursor_y as f32 + 16.0;

        if x + win_w > screen_w - 20.0 {
            x = (cursor_x as f32 - win_w - 12.0).max(10.0);
        }

        if y + win_h > screen_h - 40.0 {
            y = (cursor_y as f32 - win_h - 16.0).max(10.0);
        }

        (x, y)
    }
}

fn normalize_language(language: &str) -> String {
    match language.to_ascii_lowercase().as_str() {
        "zh" | "zh-cn" | "zh-hans" => "zh-CN".into(),
        "zh-tw" | "zh-hant" => "zh-TW".into(),
        "en-us" | "en-gb" => "en".into(),
        _ => language.to_owned(),
    }
}

fn infer_source_language(text: &str, target: &str) -> String {
    // Cached results from upstream do not include detected_lang. Cover the
    // scripts we can identify directly; Latin text defaults to English.
    let language = if text.chars().any(|c| ('\u{3040}'..='\u{30ff}').contains(&c)) {
        "ja"
    } else if text.chars().any(|c| ('\u{ac00}'..='\u{d7af}').contains(&c)) {
        "ko"
    } else if text.chars().any(|c| ('\u{0400}'..='\u{04ff}').contains(&c)) {
        "ru"
    } else if text.chars().any(|c| ('\u{4e00}'..='\u{9fff}').contains(&c))
        || (text.trim().is_empty() && target == "en")
    {
        "zh-CN"
    } else {
        "en"
    };
    language.into()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn translated_state(
        source: &str,
        target: &str,
        input: &str,
        output: &str,
        detected: Option<&str>,
    ) -> SharedPopupState {
        let mut state = SharedPopupState::new(&AppConfig::default());
        state.source_lang = source.into();
        state.target_lang = target.into();
        state.edit_text = input.into();
        state.status = TranslationStatus::Success(TranslationResult {
            original_text: input.into(),
            translated_text: output.into(),
            source_lang: source.into(),
            target_lang: target.into(),
            provider: "microsoft".into(),
            latency_ms: 0.0,
            from_cache: detected.is_none(),
            phonetic: None,
            detected_lang: detected.map(str::to_owned),
        });
        state
    }

    #[test]
    fn swap_auto_translation_reverses_text_and_request_direction() {
        let mut state = translated_state("auto", "zh-CN", "Hello world", "你好，世界", Some("en"));
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        state.tx_command = Some(tx);
        state.swap_languages();
        let text = state.edit_text.clone();
        state.trigger_translate(&text);
        assert!(matches!(rx.try_recv().unwrap(), GuiCommand::Translate {
            text, source_lang, target_lang, ..
        } if text == "你好，世界" && source_lang == "zh-CN" && target_lang == "en"));
    }

    #[test]
    fn swap_cached_chinese_translation_uses_actual_result_target() {
        let mut state = translated_state("auto", "en", "你好，世界", "Hello, world", None);
        state.target_lang = "zh-CN".into(); // UI auto-direction, actual API result is English.
        state.swap_languages();
        assert_eq!(
            (&*state.source_lang, &*state.target_lang, &*state.edit_text),
            ("en", "zh-CN", "Hello, world")
        );
    }

    #[test]
    fn swap_without_a_result_is_enabled_and_keeps_input() {
        let mut state = SharedPopupState::new(&AppConfig::default());
        state.swap_languages();
        assert_eq!((&*state.source_lang, &*state.target_lang), ("zh-CN", "en"));
        assert!(state.edit_text.is_empty());
        state.edit_text = "你好".into();
        state.swap_languages();
        assert_eq!(
            (&*state.source_lang, &*state.target_lang, &*state.edit_text),
            ("en", "zh-CN", "你好")
        );
    }

    #[test]
    fn pin_discards_pending_follow_and_preserves_geometry_through_toggle() {
        let mut state = SharedPopupState::new(&AppConfig::default());
        state.window_pos = Some((120.0, 260.0));
        state.window_size = (760.0, 560.0);
        state.should_update_pos = true;
        state.toggle_pin();
        assert!(state.is_pinned);
        assert!(!state.should_update_pos);
        assert_eq!(state.window_pos, Some((120.0, 260.0)));
        state.toggle_pin();
        assert!(!state.is_pinned);
        assert_eq!(state.window_size, (760.0, 560.0));
    }

    #[test]
    fn startup_visibility_follows_preference() {
        let mut config = AppConfig::default();
        assert!(SharedPopupState::new(&config).is_visible);
        config.ui.start_hidden = true;
        assert!(!SharedPopupState::new(&config).is_visible);
    }

    #[test]
    fn oversized_input_reports_error_without_dispatching_translation() {
        let mut state = SharedPopupState::new(&AppConfig::default());
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        state.tx_command = Some(tx);
        state.trigger_translate(&"字".repeat(3001));
        assert!(matches!(state.status, TranslationStatus::Error { .. }));
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn new_hotkey_clears_registration_feedback() {
        let mut state = SharedPopupState::new(&AppConfig::default());
        assert!(state.hotkey_feedback.is_none());
        state.hotkey_feedback = Some((true, "old registration".into()));
        let (tx, rx) = std::sync::mpsc::channel();
        state.tx_hotkey = Some(tx);
        state.update_hotkey(" CTRL+ALT+T ");
        assert!(state.hotkey_feedback.is_none());
        assert_eq!(rx.recv().unwrap(), "ctrl+alt+t");
    }
}
