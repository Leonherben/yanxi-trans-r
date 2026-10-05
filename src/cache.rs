use rusqlite::{params, Connection};
use sha2::{Digest, Sha256};
use std::fs;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use crate::models::{TranslationRequest, TranslationResult};

#[derive(Clone)]
pub struct SQLiteCache {
    conn: Arc<Mutex<Connection>>,
}

impl SQLiteCache {
    pub fn new() -> Result<Self, Box<dyn std::error::Error + Send + Sync>> {
        let dir = dirs::data_dir()
            .or_else(dirs::config_dir)
            .unwrap_or_else(|| PathBuf::from("."))
            .join("yanxi");
        let _ = fs::create_dir_all(&dir);
        let db_path = dir.join("cache.db");

        let conn = Connection::open(&db_path)?;
        conn.execute_batch(
            "
            CREATE TABLE IF NOT EXISTS translation_cache (
                hash_key TEXT PRIMARY KEY,
                original_text TEXT NOT NULL,
                translated_text TEXT NOT NULL,
                source_lang TEXT NOT NULL,
                target_lang TEXT NOT NULL,
                provider TEXT NOT NULL,
                latency_ms REAL NOT NULL,
                created_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP,
                last_accessed_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP,
                access_count INTEGER DEFAULT 1
            );
            CREATE INDEX IF NOT EXISTS idx_hash_key ON translation_cache(hash_key);
            CREATE INDEX IF NOT EXISTS idx_lookup ON translation_cache(original_text, target_lang, provider);
            ",
        )?;

        Ok(Self {
            conn: Arc::new(Mutex::new(conn)),
        })
    }

    /// 与言蹊 Python 规范 100% 保持一致的 SHA256 指纹算法
    /// raw = f"{source_lang}|{target_lang}|{provider}|{normalized}"
    pub fn compute_hash(
        text: &str,
        source_lang: &str,
        target_lang: &str,
        provider: &str,
    ) -> String {
        let normalized = text.trim();
        let raw = format!("{source_lang}|{target_lang}|{provider}|{normalized}");
        let mut hasher = Sha256::new();
        hasher.update(raw.as_bytes());
        format!("{:x}", hasher.finalize())
    }

    pub fn get(
        &self,
        req: &TranslationRequest,
        provider: &str,
    ) -> Option<TranslationResult> {
        let clean = req.clean_text();
        let hash_key = Self::compute_hash(&clean, &req.source_lang, &req.target_lang, provider);

        let conn = self.conn.lock().ok()?;

        // 1. 精确哈希键匹配
        let mut stmt = conn
            .prepare(
                "SELECT original_text, translated_text, source_lang, target_lang, provider
                 FROM translation_cache WHERE hash_key = ?1",
            )
            .ok()?;

        let mut rows = stmt
            .query_map(params![hash_key], |row| {
                Ok(TranslationResult {
                    original_text: row.get(0)?,
                    translated_text: row.get(1)?,
                    source_lang: row.get(2)?,
                    target_lang: row.get(3)?,
                    provider: row.get(4)?,
                    latency_ms: 0.0,
                    from_cache: true,
                    phonetic: None,
                    detected_lang: None,
                })
            })
            .ok()?;

        if let Some(Ok(res)) = rows.next() {
            let _ = conn.execute(
                "UPDATE translation_cache SET access_count = access_count + 1, last_accessed_at = CURRENT_TIMESTAMP WHERE hash_key = ?1",
                params![hash_key],
            );
            return Some(res);
        }

        // 2. 智能模糊匹配：若查询源语言为 auto，自动命中已缓存的目标语言相同条目
        if req.source_lang == "auto" {
            let mut stmt2 = conn
                .prepare(
                    "SELECT hash_key, original_text, translated_text, source_lang, target_lang, provider
                     FROM translation_cache
                     WHERE original_text = ?1 AND target_lang = ?2 AND provider = ?3
                     ORDER BY last_accessed_at DESC LIMIT 1",
                )
                .ok()?;

            let mut rows2 = stmt2
                .query_map(params![clean, req.target_lang, provider], |row| {
                    let hk: String = row.get(0)?;
                    Ok((
                        hk,
                        TranslationResult {
                            original_text: row.get(1)?,
                            translated_text: row.get(2)?,
                            source_lang: row.get(3)?,
                            target_lang: row.get(4)?,
                            provider: row.get(5)?,
                            latency_ms: 0.0,
                            from_cache: true,
                            phonetic: None,
                            detected_lang: None,
                        },
                    ))
                })
                .ok()?;

            if let Some(Ok((hk, res))) = rows2.next() {
                let _ = conn.execute(
                    "UPDATE translation_cache SET access_count = access_count + 1, last_accessed_at = CURRENT_TIMESTAMP WHERE hash_key = ?1",
                    params![hk],
                );
                return Some(res);
            }
        }

        None
    }

    pub fn put(&self, res: &TranslationResult) {
        if !res.is_success() {
            return;
        }

        let hash_key = Self::compute_hash(
            &res.original_text,
            &res.source_lang,
            &res.target_lang,
            &res.provider,
        );

        if let Ok(conn) = self.conn.lock() {
            let _ = conn.execute(
                "INSERT INTO translation_cache (
                    hash_key, original_text, translated_text, source_lang, target_lang, provider, latency_ms, last_accessed_at
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, CURRENT_TIMESTAMP)
                 ON CONFLICT(hash_key) DO UPDATE SET
                    translated_text = excluded.translated_text,
                    latency_ms = excluded.latency_ms,
                    last_accessed_at = CURRENT_TIMESTAMP,
                    access_count = access_count + 1",
                params![
                    hash_key,
                    res.original_text,
                    res.translated_text,
                    res.source_lang,
                    res.target_lang,
                    res.provider,
                    res.latency_ms
                ],
            );
        }
    }

    pub fn clear(&self) -> Result<(), rusqlite::Error> {
        let conn = self.conn.lock().unwrap();
        conn.execute("DELETE FROM translation_cache", [])?;
        Ok(())
    }
}
