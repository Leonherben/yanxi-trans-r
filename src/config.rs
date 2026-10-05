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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum SelectionMode {
    #[default]
    Companion,  // 伴随阅读 (默认)：仅浮窗可见/固定时划词才自动刷新；关闭时静默
    Automatic,  // 划选即弹窗：任意划选均自动弹窗
    Manual,     // 手动模式：划选不弹窗
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SelectionConfig {
    pub enable_x11_primary: bool,
    pub auto_popup_on_selection: bool,
    #[serde(default)]
    pub auto_popup_only_when_visible: bool,
    #[serde(default)]
    pub mode: SelectionMode,
    pub debounce_ms: u64,
}

impl Default for SelectionConfig {
    fn default() -> Self {
        Self {
            enable_x11_primary: true,
            auto_popup_on_selection: true,
            auto_popup_only_when_visible: true,
            mode: SelectionMode::Companion,
            debounce_ms: 150,
        }
    }
}

impl SelectionConfig {
    pub fn get_mode(&self) -> SelectionMode {
        if !self.auto_popup_on_selection {
            SelectionMode::Manual
        } else if self.auto_popup_only_when_visible || self.mode == SelectionMode::Companion {
            SelectionMode::Companion
        } else {
            self.mode
        }
    }

    pub fn set_mode(&mut self, mode: SelectionMode) {
        self.mode = mode;
        match mode {
            SelectionMode::Manual => {
                self.auto_popup_on_selection = false;
                self.auto_popup_only_when_visible = false;
            }
            SelectionMode::Companion => {
                self.auto_popup_on_selection = true;
                self.auto_popup_only_when_visible = true;
            }
            SelectionMode::Automatic => {
                self.auto_popup_on_selection = true;
                self.auto_popup_only_when_visible = false;
            }
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppConfig {
    pub active_provider: String,
    #[serde(default = "default_source_lang")]
    pub source_lang: String,
    pub target_lang: String,
    pub providers: HashMap<String, ProviderConfig>,
    pub selection: SelectionConfig,
}

fn default_source_lang() -> String {
    "auto".into()
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

        // 4. 智谱 GLM 大模型
        providers.insert(
            "zhipu".into(),
            ProviderConfig {
                name: "zhipu".into(),
                provider_type: "openai_compatible".into(),
                base_url: "https://open.bigmodel.cn/api/paas/v4".into(),
                api_key: "".into(),
                model: "glm-4-flash".into(),
                timeout_seconds: 15.0,
                system_prompt: "You are a professional translator. Translate naturally and concisely.".into(),
            },
        );

        // 5. 本地 Ollama 模型
        providers.insert(
            "custom".into(),
            ProviderConfig {
                name: "custom".into(),
                provider_type: "openai_compatible".into(),
                base_url: "http://localhost:11434/v1".into(),
                api_key: "ollama".into(),
                model: "qwen2.5:1.5b".into(),
                timeout_seconds: 20.0,
                system_prompt: "You are a professional translator. Translate naturally and concisely.".into(),
            },
        );

        Self {
            active_provider: "microsoft".into(),
            source_lang: "auto".into(),
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
                if let Ok(mut config) = serde_json::from_str::<Self>(&content) {
                    let default_cfg = Self::default();
                    let mut modified = false;
                    for (k, v) in default_cfg.providers {
                        if !config.providers.contains_key(&k) {
                            config.providers.insert(k, v);
                            modified = true;
                        }
                    }
                    if modified {
                        let _ = config.save();
                    }
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
