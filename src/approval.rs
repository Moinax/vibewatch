//! `ApprovalRegistry` — holds open hook connections keyed by `request_id` so
//! the daemon can write a decision back when the user clicks Accept/Deny in
//! the widget.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::net::unix::OwnedWriteHalf;
use tokio::sync::Mutex;

/// One held hook connection awaiting the user's Accept/Deny click.
pub struct ApprovalEntry {
    pub write_half: OwnedWriteHalf,
    pub session_id: String,
    pub created_at: Instant,
}

/// Thread-safe map of `request_id` → pending approval. Cheaply cloneable.
#[derive(Clone)]
pub struct ApprovalRegistry {
    inner: Arc<Mutex<HashMap<String, ApprovalEntry>>>,
}

impl ApprovalRegistry {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    pub async fn insert(&self, request_id: String, entry: ApprovalEntry) {
        let mut map = self.inner.lock().await;
        map.insert(request_id, entry);
    }

    pub async fn take(&self, request_id: &str) -> Option<ApprovalEntry> {
        let mut map = self.inner.lock().await;
        map.remove(request_id)
    }

    /// Remove and return every entry for `session_id`. Used when a later
    /// session event (PostToolUse, Stop, UserPromptSubmit, a new
    /// PermissionRequest) proves the prior prompt was already resolved — the
    /// caller drops the returned write_halves, giving any still-blocked hook
    /// an EOF so it can exit.
    pub async fn take_by_session(&self, session_id: &str) -> Vec<ApprovalEntry> {
        let mut map = self.inner.lock().await;
        let matching: Vec<String> = map
            .iter()
            .filter(|(_, e)| e.session_id == session_id)
            .map(|(k, _)| k.clone())
            .collect();
        matching.into_iter().filter_map(|k| map.remove(&k)).collect()
    }

    /// Remove and return any entries older than `max_age`.
    pub async fn reap_stale(&self, max_age: Duration) -> Vec<ApprovalEntry> {
        let mut map = self.inner.lock().await;
        let now = Instant::now();
        let stale: Vec<String> = map
            .iter()
            .filter(|(_, e)| now.duration_since(e.created_at) > max_age)
            .map(|(k, _)| k.clone())
            .collect();
        stale.into_iter().filter_map(|k| map.remove(&k)).collect()
    }
}

impl Default for ApprovalRegistry {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;
    use tokio::net::{UnixListener, UnixStream};

    /// Spawn a throwaway UnixStream pair and return the "server side" which
    /// we'll split into halves — the write half is what gets stashed.
    async fn make_pair() -> UnixStream {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("s.sock");
        let listener = UnixListener::bind(&path).unwrap();
        let client = UnixStream::connect(&path).await.unwrap();
        let (server, _) = listener.accept().await.unwrap();
        drop(client);
        drop(listener);
        drop(tmp);
        server
    }

    #[tokio::test]
    async fn insert_and_take_roundtrip() {
        let reg = ApprovalRegistry::new();
        let stream = make_pair().await;
        let (_rh, wh) = stream.into_split();
        let entry = ApprovalEntry {
            write_half: wh,
            session_id: "s1".into(),
            created_at: std::time::Instant::now(),
        };
        reg.insert("req-1".into(), entry).await;

        let taken = reg.take("req-1").await.expect("should find entry");
        assert_eq!(taken.session_id, "s1");
        assert!(reg.take("req-1").await.is_none(), "second take returns None");
    }

    #[tokio::test]
    async fn take_unknown_returns_none() {
        let reg = ApprovalRegistry::new();
        assert!(reg.take("does-not-exist").await.is_none());
    }

    #[tokio::test]
    async fn reap_stale_removes_old_entries() {
        let reg = ApprovalRegistry::new();
        let stream = make_pair().await;
        let (_rh, wh) = stream.into_split();
        let old_entry = ApprovalEntry {
            write_half: wh,
            session_id: "old".into(),
            created_at: std::time::Instant::now() - Duration::from_secs(700),
        };
        reg.insert("old-req".into(), old_entry).await;

        let stream2 = make_pair().await;
        let (_rh2, wh2) = stream2.into_split();
        let fresh_entry = ApprovalEntry {
            write_half: wh2,
            session_id: "fresh".into(),
            created_at: std::time::Instant::now(),
        };
        reg.insert("fresh-req".into(), fresh_entry).await;

        let reaped = reg.reap_stale(Duration::from_secs(580)).await;
        assert_eq!(reaped.len(), 1);
        assert_eq!(reaped[0].session_id, "old");
        assert!(reg.take("old-req").await.is_none());
        assert!(reg.take("fresh-req").await.is_some());
    }

    #[tokio::test]
    async fn take_by_session_returns_only_matching_entries() {
        let reg = ApprovalRegistry::new();

        let s1a = make_pair().await;
        let (_rh, wh) = s1a.into_split();
        reg.insert(
            "req-s1-a".into(),
            ApprovalEntry {
                write_half: wh,
                session_id: "s1".into(),
                created_at: std::time::Instant::now(),
            },
        )
        .await;

        let s1b = make_pair().await;
        let (_rh, wh) = s1b.into_split();
        reg.insert(
            "req-s1-b".into(),
            ApprovalEntry {
                write_half: wh,
                session_id: "s1".into(),
                created_at: std::time::Instant::now(),
            },
        )
        .await;

        let s2 = make_pair().await;
        let (_rh, wh) = s2.into_split();
        reg.insert(
            "req-s2".into(),
            ApprovalEntry {
                write_half: wh,
                session_id: "s2".into(),
                created_at: std::time::Instant::now(),
            },
        )
        .await;

        let taken = reg.take_by_session("s1").await;
        assert_eq!(taken.len(), 2, "both s1 entries returned");
        assert!(taken.iter().all(|e| e.session_id == "s1"));

        assert!(reg.take("req-s1-a").await.is_none());
        assert!(reg.take("req-s1-b").await.is_none());
        assert!(reg.take("req-s2").await.is_some(), "s2 untouched");
    }

    #[tokio::test]
    async fn take_by_session_with_no_matches_returns_empty() {
        let reg = ApprovalRegistry::new();
        let taken = reg.take_by_session("nope").await;
        assert!(taken.is_empty());
    }

    #[tokio::test]
    async fn clone_shares_storage() {
        let reg = ApprovalRegistry::new();
        let reg2 = reg.clone();
        let stream = make_pair().await;
        let (_rh, wh) = stream.into_split();
        reg.insert(
            "r1".into(),
            ApprovalEntry {
                write_half: wh,
                session_id: "s1".into(),
                created_at: std::time::Instant::now(),
            },
        )
        .await;
        assert!(reg2.take("r1").await.is_some(), "clone sees the entry");
    }
}
