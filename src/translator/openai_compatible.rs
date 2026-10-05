use reqwest::header::{HeaderMap, HeaderValue, AUTHORIZATION, CONTENT_TYPE};
use reqwest::Client;
use serde_json::Value;
use std::time::{Duration, Instant};

use crate::config::ProviderConfig;
use crate::models::{TranslationRequest, TranslationResult};

pub struct OpenAICompatibleTranslator {
    config: ProviderConfig,
    client: Client,
    endpoint: String,
}

impl OpenAICompatibleTranslator {
    pub fn new(config: ProviderConfig) -> Self {
        let base_url = config.base_url.trim_end_matches('/');
        let endpoint = if base_url.ends_with("/chat/completions") {
            base_url.to_string()
        } else {
            format!("{base_url}/chat/completions")
        };

        let mut headers = HeaderMap::new();
        headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
        if !config.api_key.trim().is_empty() {
            if let Ok(auth_val) = HeaderValue::from_str(&format!("Bearer {}", config.api_key.trim())) {
                headers.insert(AUTHORIZATION, auth_val);
            }
        }

        let timeout = if config.timeout_seconds > 0.0 {
            Duration::from_secs_f64(config.timeout_seconds)
        } else {
            Duration::from_secs(15)
        };

        let client = Client::builder()
            .default_headers(headers)
            .timeout(timeout)
            .build()
            .unwrap_or_else(|_| Client::new());

        Self {
            config,
            client,
            endpoint,
        }
    }

    fn build_messages(&self, req: &TranslationRequest) -> Vec<serde_json::Value> {
        let clean = req.clean_text();
        let mut prompt = format!(
            "Please translate the following text from {} into {}.\nOriginal text:\n{}",
            req.source_lang, req.target_lang, clean
        );

        if let Some(ref ctx) = req.context {
            prompt.push_str(&format!("\n\nContext information for reference:\n{ctx}"));
        }

        vec![
            serde_json::json!({
                "role": "system",
                "content": self.config.system_prompt
            }),
            serde_json::json!({
                "role": "user",
                "content": prompt
            }),
        ]
    }

    pub async fn translate(
        &self,
        req: &TranslationRequest,
    ) -> Result<TranslationResult, Box<dyn std::error::Error + Send + Sync>> {
        if self.config.api_key.trim().is_empty() {
            return Ok(TranslationResult::error(
                &req.text,
                format!("未配置 API Key，请先运行 `yanxi-cli --set-key {} <KEY>` 进行配置", self.config.name),
                &self.config.name,
            ));
        }

        let payload = serde_json::json!({
            "model": self.config.model,
            "messages": self.build_messages(req),
            "temperature": 0.3,
            "stream": false
        });

        let start = Instant::now();

        let resp = self
            .client
            .post(&self.endpoint)
            .header(AUTHORIZATION, format!("Bearer {}", self.config.api_key.trim()))
            .json(&payload)
            .send()
            .await;

        let latency = (start.elapsed().as_secs_f64() * 1000.0 * 10.0).round() / 10.0;

        match resp {
            Ok(response) => {
                let status = response.status();
                if status.is_success() {
                    let body: Value = response.json().await?;
                    if let Some(content) = body
                        .get("choices")
                        .and_then(|c| c.as_array())
                        .and_then(|arr| arr.first())
                        .and_then(|first| first.get("message"))
                        .and_then(|m| m.get("content"))
                        .and_then(|c| c.as_str())
                    {
                        Ok(TranslationResult {
                            original_text: req.clean_text(),
                            translated_text: content.trim().to_string(),
                            source_lang: req.source_lang.clone(),
                            target_lang: req.target_lang.clone(),
                            provider: self.config.name.clone(),
                            latency_ms: latency,
                            from_cache: false,
                            phonetic: None,
                            detected_lang: None,
                        })
                    } else {
                        Ok(TranslationResult::error(
                            &req.text,
                            "响应数据解析失败，未找到有效 choices[0].message.content",
                            &self.config.name,
                        ))
                    }
                } else if status.as_u16() == 401 {
                    Ok(TranslationResult::error(
                        &req.text,
                        "认证失败 (401)：API 密钥无效或未授权",
                        &self.config.name,
                    ))
                } else {
                    let err_text = response.text().await.unwrap_or_default();
                    let snippet = if err_text.len() > 200 {
                        format!("{}...", &err_text[..200])
                    } else {
                        err_text
                    };
                    Ok(TranslationResult::error(
                        &req.text,
                        format!("API 请求失败 HTTP {}: {snippet}", status.as_u16()),
                        &self.config.name,
                    ))
                }
            }
            Err(e) => {
                if e.is_timeout() {
                    Ok(TranslationResult::error(
                        &req.text,
                        format!("请求超时 ({}s)，请检查网络连接", self.config.timeout_seconds),
                        &self.config.name,
                    ))
                } else {
                    Ok(TranslationResult::error(
                        &req.text,
                        format!("网络请求异常: {e}"),
                        &self.config.name,
                    ))
                }
            }
        }
    }

    pub async fn test_connection(&self) -> (bool, String) {
        let req = TranslationRequest::with_langs("Hello", "en", "zh-CN");
        match self.translate(&req).await {
            Ok(res) if res.is_success() => (
                true,
                format!("连接成功！耗时: {}ms, 结果: {}", res.latency_ms, res.translated_text),
            ),
            Ok(res) => (false, res.translated_text),
            Err(e) => (false, format!("连接错误: {e}")),
        }
    }
}
