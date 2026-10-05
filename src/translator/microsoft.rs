use regex::Regex;
use reqwest::header::{HeaderMap, HeaderValue, ACCEPT_LANGUAGE, REFERER, USER_AGENT};
use reqwest::Client;
use serde_json::Value;
use std::sync::{Arc, LazyLock, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use crate::config::ProviderConfig;
use crate::models::{TranslationRequest, TranslationResult};

static RE_IG: LazyLock<Regex> = LazyLock::new(|| Regex::new(r#"IG:"([A-Fa-f0-9]+)""#).unwrap());
static RE_IID: LazyLock<Regex> = LazyLock::new(|| Regex::new(r#"data-iid="([^"]+)""#).unwrap());
static RE_ABUSE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"params_AbusePreventionHelper\s*=\s*\[(\d+),"([^"]+)",(\d+)\]"#).unwrap());

#[derive(Clone, Debug)]
struct BingSession {
    ig: String,
    iid: String,
    key: String,
    token: String,
    expires_at: f64,
    base_host: String,
}

impl BingSession {
    fn is_valid(&self) -> bool {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs_f64();
        now < self.expires_at
    }
}

pub struct MicrosoftTranslator {
    config: ProviderConfig,
    client: Client,
    session: Arc<Mutex<Option<BingSession>>>,
}

impl MicrosoftTranslator {
    pub fn new(config: ProviderConfig) -> Self {
        let mut headers = HeaderMap::new();
        headers.insert(
            USER_AGENT,
            HeaderValue::from_static(
                "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/124.0.0.0 Safari/537.36 Edg/124.0.0.0",
            ),
        );
        headers.insert(
            ACCEPT_LANGUAGE,
            HeaderValue::from_static("zh-CN,zh;q=0.9,en;q=0.8"),
        );
        headers.insert(
            REFERER,
            HeaderValue::from_static("https://www.bing.com/translator"),
        );

        let timeout = if config.timeout_seconds > 0.0 {
            Duration::from_secs_f64(config.timeout_seconds)
        } else {
            Duration::from_secs(10)
        };

        let client = Client::builder()
            .default_headers(headers)
            .timeout(timeout)
            .cookie_store(true)
            .build()
            .unwrap_or_else(|_| Client::new());

        Self {
            config,
            client,
            session: Arc::new(Mutex::new(None)),
        }
    }

    fn map_lang(lang: &str, is_source: bool) -> &str {
        match lang.to_lowercase().as_str() {
            "zh-cn" | "zh" => "zh-Hans",
            "zh-tw" | "zh-hk" => "zh-Hant",
            "auto" => {
                if is_source {
                    "auto-detect"
                } else {
                    "zh-Hans"
                }
            }
            "en" => "en",
            "ja" => "ja",
            "ko" => "ko",
            "fr" => "fr",
            "de" => "de",
            "es" => "es",
            "ru" => "ru",
            _ => lang,
        }
    }

    async fn get_session(&self, force_refresh: bool) -> Result<BingSession, Box<dyn std::error::Error + Send + Sync>> {
        {
            let lock = self.session.lock().unwrap();
            if !force_refresh {
                if let Some(ref s) = *lock {
                    if s.is_valid() {
                        return Ok(s.clone());
                    }
                }
            }
        }

        // 优先使用 www.bing.com 以防国内 cn.bing.com 出现重定向差异
        let candidate_urls = [
            "https://www.bing.com/translator",
            "https://cn.bing.com/translator",
        ];

        let mut last_err = String::new();
        for url in candidate_urls {
            match self.client.get(url).send().await {
                Ok(resp) => {
                    let base_host = resp.url().host_str().unwrap_or("www.bing.com").to_string();
                    if let Ok(html) = resp.text().await {
                        let ig = RE_IG.captures(&html).and_then(|c| c.get(1)).map(|m| m.as_str().to_string());
                        let iid = RE_IID.captures(&html).and_then(|c| c.get(1)).map(|m| m.as_str().to_string());
                        let abuse = RE_ABUSE.captures(&html);

                        if let (Some(ig), Some(iid), Some(abuse)) = (ig, iid, abuse) {
                            let key = abuse.get(1).map(|m| m.as_str().to_string()).unwrap_or_default();
                            let token = abuse.get(2).map(|m| m.as_str().to_string()).unwrap_or_default();
                            let interval_ms = abuse.get(3).and_then(|m| m.as_str().parse::<f64>().ok()).unwrap_or(3600000.0);

                            let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs_f64();
                            let expires_at = now + (interval_ms / 1000.0 - 300.0).max(300.0);

                            let session = BingSession {
                                ig,
                                iid,
                                key,
                                token,
                                expires_at,
                                base_host,
                            };
                            let mut lock = self.session.lock().unwrap();
                            *lock = Some(session.clone());
                            return Ok(session);
                        }
                    }
                }
                Err(e) => {
                    last_err = e.to_string();
                }
            }
        }

        Err(format!("无法从必应翻译主页解析动态会话凭证 ({last_err})").into())
    }

    async fn translate_free_bing(&self, req: &TranslationRequest) -> Result<TranslationResult, Box<dyn std::error::Error + Send + Sync>> {
        let clean = req.clean_text();
        let src_lang = Self::map_lang(&req.source_lang, true);
        let tgt_lang = Self::map_lang(&req.target_lang, false);

        let start = Instant::now();

        for attempt in 0..2 {
            let session = self.get_session(attempt > 0).await?;

            let candidate_hosts = [
                session.base_host.as_str(),
                "www.bing.com",
                "cn.bing.com",
            ];

            let params = [
                ("fromLang", src_lang),
                ("text", clean.as_str()),
                ("to", tgt_lang),
                ("token", session.token.as_str()),
                ("key", session.key.as_str()),
            ];

            for host in candidate_hosts {
                let url = format!(
                    "https://{}/ttranslatev3?isVertical=1&&IG={}&IID={}",
                    host, session.ig, session.iid
                );

                let resp = self.client.post(&url).form(&params).send().await;
                if let Ok(res) = resp {
                    if !res.status().is_success() {
                        continue;
                    }
                    if let Ok(json) = res.json::<Value>().await {
                        // Check if token expired
                        if let Some(code) = json.get("statusCode").and_then(|c| c.as_i64()) {
                            if code == 205 {
                                break; // retry attempt with fresh session
                            }
                        }

                        if let Some(arr) = json.as_array() {
                            if let Some(first) = arr.first() {
                                let detected = first
                                    .get("detectedLanguage")
                                    .and_then(|d| d.get("language"))
                                    .and_then(|l| l.as_str())
                                    .map(|s| s.to_string());

                                if let Some(translations) = first.get("translations").and_then(|t| t.as_array()) {
                                    if let Some(trans_first) = translations.first() {
                                        if let Some(text) = trans_first.get("text").and_then(|t| t.as_str()) {
                                            let phonetic = trans_first
                                                .get("transliteration")
                                                .and_then(|t| t.get("text"))
                                                .and_then(|s| s.as_str())
                                                .map(|s| s.to_string());

                                            let latency = start.elapsed().as_secs_f64() * 1000.0;
                                            return Ok(TranslationResult {
                                                original_text: clean,
                                                translated_text: text.to_string(),
                                                source_lang: req.source_lang.clone(),
                                                target_lang: req.target_lang.clone(),
                                                provider: "microsoft".into(),
                                                latency_ms: (latency * 10.0).round() / 10.0,
                                                from_cache: false,
                                                phonetic,
                                                detected_lang: detected,
                                            });
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }

        Err("必应翻译重试后仍未获取到有效译文（可能受到临时限流或网络阻断）".into())
    }

    async fn translate_azure_official(&self, req: &TranslationRequest) -> Result<TranslationResult, Box<dyn std::error::Error + Send + Sync>> {
        let clean = req.clean_text();
        let src_lang = Self::map_lang(&req.source_lang, true);
        let tgt_lang = Self::map_lang(&req.target_lang, false);

        let endpoint = if self.config.base_url.starts_with("http") && !self.config.base_url.contains("bing.com") {
            self.config.base_url.clone()
        } else {
            "https://api.cognitive.microsofttranslator.com/translate".to_string()
        };

        let mut query = vec![("api-version", "3.0"), ("to", tgt_lang)];
        if src_lang != "auto-detect" {
            query.push(("from", src_lang));
        }

        let mut req_builder = self.client.post(&endpoint)
            .query(&query)
            .header("Ocp-Apim-Subscription-Key", self.config.api_key.trim())
            .header("Content-Type", "application/json");

        if self.config.base_url.contains("region=") {
            if let Some(region) = self.config.base_url.split("region=").nth(1).and_then(|s| s.split('&').next()) {
                req_builder = req_builder.header("Ocp-Apim-Subscription-Region", region);
            }
        }

        let body = serde_json::json!([{"Text": clean}]);
        let start = Instant::now();

        let resp = req_builder.json(&body).send().await?;
        let latency = (start.elapsed().as_secs_f64() * 1000.0 * 10.0).round() / 10.0;

        if !resp.status().is_success() {
            let err_txt = resp.text().await.unwrap_or_default();
            return Ok(TranslationResult::error(&req.text, format!("Azure 官方接口返回错误: {err_txt}"), "microsoft"));
        }

        let json: Value = resp.json().await?;
        if let Some(arr) = json.as_array() {
            if let Some(first) = arr.first() {
                if let Some(translations) = first.get("translations").and_then(|t| t.as_array()) {
                    if let Some(trans_first) = translations.first() {
                        if let Some(text) = trans_first.get("text").and_then(|t| t.as_str()) {
                            return Ok(TranslationResult {
                                original_text: clean,
                                translated_text: text.to_string(),
                                source_lang: req.source_lang.clone(),
                                target_lang: req.target_lang.clone(),
                                provider: "microsoft".into(),
                                latency_ms: latency,
                                from_cache: false,
                                phonetic: None,
                                detected_lang: None,
                            });
                        }
                    }
                }
            }
        }

        Ok(TranslationResult::error(&req.text, "Azure 响应结构无法解析", "microsoft"))
    }

    pub async fn translate(&self, req: &TranslationRequest) -> Result<TranslationResult, Box<dyn std::error::Error + Send + Sync>> {
        if !self.config.api_key.trim().is_empty() {
            self.translate_azure_official(req).await
        } else {
            self.translate_free_bing(req).await
        }
    }

    pub async fn test_connection(&self) -> (bool, String) {
        let req = TranslationRequest::with_langs("Hello", "en", "zh-CN");
        match self.translate(&req).await {
            Ok(res) if res.is_success() => {
                let mode = if !self.config.api_key.trim().is_empty() {
                    "Azure 官方专线"
                } else {
                    "必应免 Key 免费通道"
                };
                (
                    true,
                    format!("微软翻译连通成功 [{}]，测试译文: {} ({:.1}ms)", mode, res.translated_text, res.latency_ms),
                )
            }
            Ok(res) => (false, res.translated_text),
            Err(e) => (false, format!("连接错误: {e}")),
        }
    }
}
