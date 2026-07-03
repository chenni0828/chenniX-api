//! Model name normalizer: maps any incoming model name (canonical or alias)
//! to its `(model_id, canonical_name)` pair.
//!
//! The mapping is process-local and write-once-per-reload: it is fully
//! replaced whenever the cache layer calls `reload`. Lookups are O(1).

use std::collections::HashMap;
use std::sync::Arc;
use chennix_common::ProxyResult;
use tokio::sync::RwLock;

/// Maps an incoming model name (canonical or alias) to `(model_id, canonical_name)`.
///
/// Both keys and the canonical_name field are stored lowercased so that
/// resolution is case-insensitive — clients send "GPT-4", "gpt-4", and "Gpt-4"
/// interchangeably.
pub struct Normalizer {
    cache: Arc<RwLock<HashMap<String, (i64, String)>>>,
}

impl Normalizer {
    pub fn new() -> Self {
        Self {
            cache: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    /// Resolve a name to `(model_id, canonical_name)`.
    ///
    /// Looks up the name (case-insensitive) in the cached alias map. Returns
    /// `Ok(None)` if the name is unknown — callers should treat that as
    /// "no canonical model matches, fall back to the raw name".
    pub async fn resolve(&self, name: &str) -> ProxyResult<Option<(i64, String)>> {
        let key = name.trim().to_lowercase();
        if key.is_empty() {
            return Ok(None);
        }
        let map = self.cache.read().await;
        Ok(map.get(&key).cloned())
    }

    /// Replace the entire alias → (model_id, canonical_name) mapping.
    ///
    /// Keys should already be the names clients will send (canonical_name
    /// and every alias). Both keys and canonical_name values are lowercased
    /// internally so callers do not need to normalize themselves.
    pub async fn reload(&self, mapping: HashMap<String, (i64, String)>) {
        let mut normalized: HashMap<String, (i64, String)> = HashMap::with_capacity(mapping.len());
        for (k, (id, canonical)) in mapping {
            let key = k.trim().to_lowercase();
            if key.is_empty() {
                continue;
            }
            normalized.insert(key, (id, canonical));
        }
        let mut w = self.cache.write().await;
        *w = normalized;
    }
}

impl Default for Normalizer {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mapping() -> HashMap<String, (i64, String)> {
        let mut m = HashMap::new();
        // canonical names only — alias system removed
        m.insert("deepseek-v3".into(), (1, "deepseek-v3".into()));
        m.insert("gpt-4".into(), (2, "gpt-4".into()));
        m
    }

    #[tokio::test]
    async fn test_resolve_canonical_name() {
        let n = Normalizer::new();
        n.reload(mapping()).await;

        let r = n.resolve("deepseek-v3").await.unwrap();
        assert_eq!(r, Some((1, "deepseek-v3".to_string())));

        // case-insensitive
        let r = n.resolve("DEEPSEEK-V3").await.unwrap();
        assert_eq!(r, Some((1, "deepseek-v3".to_string())));
    }

    #[tokio::test]
    async fn test_resolve_canonical_only() {
        // 别名系统已移除：只有 canonical_name 能解析
        let n = Normalizer::new();
        n.reload(mapping()).await;

        // canonical names resolve
        assert!(n.resolve("deepseek-v3").await.unwrap().is_some());
        assert!(n.resolve("gpt-4").await.unwrap().is_some());

        // 大小写不敏感
        let r = n.resolve("GPT-4").await.unwrap();
        assert_eq!(r, Some((2, "gpt-4".to_string())));
    }

    #[tokio::test]
    async fn test_resolve_unknown_returns_none() {
        let n = Normalizer::new();
        n.reload(mapping()).await;

        assert!(n.resolve("claude-sonnet-4").await.unwrap().is_none());
        // empty / whitespace
        assert!(n.resolve("").await.unwrap().is_none());
        assert!(n.resolve("   ").await.unwrap().is_none());
    }

    #[tokio::test]
    async fn test_resolve_before_reload_returns_none() {
        // A fresh normalizer has no mapping; any name is unknown.
        let n = Normalizer::new();
        assert!(n.resolve("gpt-4").await.unwrap().is_none());
    }

    #[tokio::test]
    async fn test_reload_replaces_entire_mapping() {
        let n = Normalizer::new();
        n.reload(mapping()).await;
        assert!(n.resolve("gpt-4").await.unwrap().is_some());

        // reload with a totally different (smaller) map — old entries must vanish
        let mut m2 = HashMap::new();
        m2.insert("claude".into(), (10, "claude".into()));
        n.reload(m2).await;

        assert!(n.resolve("gpt-4").await.unwrap().is_none());
        assert_eq!(
            n.resolve("claude").await.unwrap(),
            Some((10, "claude".to_string()))
        );
    }
}
