use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderConfig {
    pub name: String,
    pub provider_type: String, // "microsoft" | "openai_compatible"
    pub base_url: String,
    pub api_key: String,
    pub model: String,
    pub timeout_seconds: f64,
    pub system_prompt: String,
}

impl Default for ProviderConfig {
    fn default() -> Self {
        Self {
            name: "microsoft".into(),
            provider_type: "microsoft".into(),
            base_url: "".into(),
            api_key: "".into(),
            model: "".into(),
            timeout_seconds: 10.0,
            system_prompt: "You are a professional, accurate translator. Translate the input text naturally into the requested target language. Output ONLY the translated result without any quotes, conversational filler, or commentary.".into(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SelectionConfig {
    pub enable_x11_primary: bool,
    pub auto_popup_on_selection: bool,
    pub debounce_ms: u64,
}

impl Default for SelectionConfig {
    fn default() -> Self {
        Self {
            enable_x11_primary: true,
            auto_popup_on_selection: true,
            debounce_ms: 150,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppConfig {
    pub active_provider: String,
    pub target_lang: String,
    pub providers: HashMap<String, ProviderConfig>,
    pub selection: SelectionConfig,
}

impl Default for AppConfig {
    fn default() -> Self {
        let mut providers = HashMap::new();

        // 1. 微软官方 Edge/Bing 免 Key 极速通道 (默认推荐)
        providers.insert(
            "microsoft".into(),
            ProviderConfig {
                name: "microsoft".into(),
                provider_type: "microsoft".into(),
                base_url: "https://cn.bing.com".into(),
                api_key: "".into(),
                model: "".into(),
                timeout_seconds: 8.0,
                system_prompt: "".into(),
            },
        );

        // 2. DeepSeek 大模型翻译
        providers.insert(
            "deepseek".into(),
            ProviderConfig {
                name: "deepseek".into(),
                provider_type: "openai_compatible".into(),
                base_url: "https://api.deepseek.com/v1".into(),
                api_key: "".into(),
                model: "deepseek-chat".into(),
                timeout_seconds: 15.0,
                system_prompt: "You are a professional translator. Translate into the target language naturally, concisely and accurately without any markdown wrappers or quotes.".into(),
            },
        );

        // 3. OpenAI 兼容协议
        providers.insert(
            "openai".into(),
            ProviderConfig {
                name: "openai".into(),
                provider_type: "openai_compatible".into(),
                base_url: "https://api.openai.com/v1".into(),
                api_key: "".into(),
                model: "gpt-4o-mini".into(),
                timeout_seconds: 15.0,
                system_prompt: "You are a professional translator. Translate into the target language accurately and concisely.".into(),
            },
        );

        Self {
            active_provider: "microsoft".into(),
            target_lang: "zh-CN".into(),
            providers,
            selection: SelectionConfig::default(),
        }
    }
}

impl AppConfig {
    pub fn config_path() -> PathBuf {
        let dir = dirs::config_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join("yanxi");
        let _ = fs::create_dir_all(&dir);
        dir.join("config.json")
    }

    pub fn load() -> Self {
        let path = Self::config_path();
        if path.exists() {
            if let Ok(content) = fs::read_to_string(&path) {
                if let Ok(config) = serde_json::from_str::<Self>(&content) {
                    return config;
                }
            }
        }
        let default_config = Self::default();
        let _ = default_config.save();
        default_config
    }

    pub fn save(&self) -> std::io::Result<()> {
        let path = Self::config_path();
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let json = serde_json::to_string_pretty(self)?;
        fs::write(path, json)
    }

    pub fn get_active_provider_config(&self) -> ProviderConfig {
        self.providers
            .get(&self.active_provider)
            .cloned()
            .unwrap_or_default()
    }
}
