use regex::Regex;
use serde::{Deserialize, Serialize};
use std::sync::LazyLock;

static RE_HYPHEN: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"([a-zA-Z]+)-\s*\n\s*([a-zA-Z]+)").unwrap());

/// 翻译请求实体
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TranslationRequest {
    pub text: String,
    pub source_lang: String,
    pub target_lang: String,
    pub context: Option<String>,
}

impl TranslationRequest {
    pub fn new(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            source_lang: "auto".into(),
            target_lang: "zh-CN".into(),
            context: None,
        }
    }

    pub fn with_langs(
        text: impl Into<String>,
        source_lang: impl Into<String>,
        target_lang: impl Into<String>,
    ) -> Self {
        Self {
            text: text.into(),
            source_lang: source_lang.into(),
            target_lang: target_lang.into(),
            context: None,
        }
    }

    /// 清洗待翻译文本：
    /// 1. 统一换行符
    /// 2. 自动修复学术论文双栏 PDF 跨行连字符截断 (例如 convo-\n lutional -> convolutional)
    /// 3. 合并多余换行符与空行
    pub fn clean_text(&self) -> String {
        if self.text.is_empty() {
            return String::new();
        }

        // 统一换行符
        let text = self.text.replace("\r\n", "\n").replace('\r', "\n");

        // 修复 PDF 英文跨行断词连字符
        let fixed = RE_HYPHEN.replace_all(&text, "${1}${2}");

        // 合并多余换行
        let lines: Vec<&str> = fixed
            .lines()
            .map(|l| l.trim())
            .filter(|l| !l.is_empty())
            .collect();

        lines.join(" ")
    }
}

/// 翻译结果实体
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TranslationResult {
    pub original_text: String,
    pub translated_text: String,
    pub source_lang: String,
    pub target_lang: String,
    pub provider: String,
    pub latency_ms: f64,
    pub from_cache: bool,
    #[serde(default)]
    pub phonetic: Option<String>,
    #[serde(default)]
    pub detected_lang: Option<String>,
}

impl TranslationResult {
    pub fn is_success(&self) -> bool {
        !self.translated_text.is_empty() && !self.translated_text.starts_with("[Error]")
    }

    pub fn error(
        original_text: impl Into<String>,
        err_msg: impl Into<String>,
        provider: impl Into<String>,
    ) -> Self {
        Self {
            original_text: original_text.into(),
            translated_text: format!("[Error] {}", err_msg.into()),
            source_lang: "auto".into(),
            target_lang: "zh-CN".into(),
            provider: provider.into(),
            latency_ms: 0.0,
            from_cache: false,
            phonetic: None,
            detected_lang: None,
        }
    }
}
