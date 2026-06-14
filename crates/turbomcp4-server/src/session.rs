//! Legacy (`2025-11-25`) session state — the stateful half of the dual-stack
//! server (PLAN §11, D9: sessions are a service-layer concern, not transport).
//!
//! The store itself is transport-agnostic: it maps an opaque session id to the
//! state negotiated at `initialize`. *Who mints the id* is the transport's
//! business (the HTTP runner derives it for the `Mcp-Session-Id` header; the
//! stdio [`LegacySessionAdapter`] mints one per connection) — the id reaches
//! the dispatcher via the internal `_meta` side-channel
//! ([`turbomcp4_core::meta::internal::SESSION_ID`]).
//!
//! In-memory only for now; pluggable backends (Redis, …) are post-GA (PLAN §11).
//!
//! [`LegacySessionAdapter`]: crate::LegacySessionAdapter

use std::collections::HashMap;
use std::sync::RwLock;
use std::time::{Duration, Instant};

use serde_json::Value;
use turbomcp4_core::{Implementation, ProtocolVersion};

/// What `initialize` negotiated for one session.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct SessionState {
    /// The protocol version the server answered with.
    pub version: ProtocolVersion,
    /// The client's `clientInfo`.
    pub client_info: Implementation,
    /// The client's declared capabilities (kept as raw JSON; the dispatcher
    /// injects it into [`turbomcp4_core::RequestContext::client_capabilities`]).
    pub client_capabilities: Value,
    /// The minimum severity the client opted into via `logging/setLevel`.
    /// `None` ⇒ no opt-in yet ⇒ this server sends no `notifications/message`
    /// (the spec leaves un-opted behavior to the server; we choose opt-in).
    pub log_level: Option<turbomcp4_core::LogLevel>,
}

struct Entry {
    state: SessionState,
    last_seen: Instant,
}

/// A bounded in-memory session table with least-recently-used eviction and an
/// optional idle timeout.
///
/// All methods take `&self`; the store is shared as an `Arc` between the
/// dispatcher (which writes at `initialize` and reads on every legacy request)
/// and whoever else needs existence checks. When an `idle_timeout` is set, a
/// session not seen within that window is treated as gone: [`get`](Self::get)
/// drops it and answers `None` (so the client re-`initialize`s), and
/// [`sweep_expired`](Self::sweep_expired) reclaims it in bulk (the dispatcher
/// pairs that with tearing down the session's subscription routes).
pub struct SessionStore {
    inner: RwLock<HashMap<String, Entry>>,
    capacity: usize,
    idle_timeout: Option<Duration>,
}

impl SessionStore {
    /// Default maximum number of live sessions.
    pub const DEFAULT_CAPACITY: usize = 4096;

    /// A store bounded to `capacity` sessions (least-recently-used wins), with
    /// no idle timeout (LRU + cap only).
    #[must_use]
    pub fn with_capacity(capacity: usize) -> Self {
        Self {
            inner: RwLock::new(HashMap::new()),
            capacity: capacity.max(1),
            idle_timeout: None,
        }
    }

    /// Set an idle timeout: a session not accessed within `timeout` is evicted.
    /// `None` disables it (the default). Chainable from the constructors.
    #[must_use]
    pub fn with_idle_timeout(mut self, timeout: Option<Duration>) -> Self {
        self.idle_timeout = timeout;
        self
    }

    /// Whether `entry` has been idle past the configured timeout as of `now`.
    fn is_expired(&self, entry: &Entry, now: Instant) -> bool {
        self.idle_timeout
            .is_some_and(|t| now.duration_since(entry.last_seen) >= t)
    }

    /// Store (or replace) `state` under `id`. If the table is full, the
    /// least-recently-seen session is evicted to make room (idle sessions, being
    /// the oldest, go first; bulk idle reclamation is [`sweep_expired`](Self::sweep_expired)).
    pub fn insert(&self, id: impl Into<String>, state: SessionState) {
        let id = id.into();
        let mut map = self.inner.write().expect("session store lock poisoned");
        let now = Instant::now();
        if !map.contains_key(&id) && map.len() >= self.capacity {
            // O(n) scan; the capacity bounds n and inserts happen once per
            // session (at `initialize`), so this is not on the hot path.
            if let Some(oldest) = map
                .iter()
                .min_by_key(|(_, e)| e.last_seen)
                .map(|(k, _)| k.clone())
            {
                map.remove(&oldest);
            }
        }
        map.insert(
            id,
            Entry {
                state,
                last_seen: now,
            },
        );
    }

    /// Look up a session, refreshing its recency. `None` means expired
    /// (dropped on the spot), evicted, or never created — the caller answers
    /// "unknown session".
    #[must_use]
    pub fn get(&self, id: &str) -> Option<SessionState> {
        let mut map = self.inner.write().expect("session store lock poisoned");
        let now = Instant::now();
        let entry = map.get_mut(id)?;
        if self.is_expired(entry, now) {
            map.remove(id);
            return None;
        }
        entry.last_seen = now;
        Some(entry.state.clone())
    }

    /// Remove and return every session past its idle timeout. The dispatcher
    /// calls this opportunistically (and tears down each id's subscription
    /// routes). A store with no idle timeout always returns empty.
    #[must_use]
    pub fn sweep_expired(&self) -> Vec<String> {
        if self.idle_timeout.is_none() {
            return Vec::new();
        }
        let now = Instant::now();
        let mut map = self.inner.write().expect("session store lock poisoned");
        let expired: Vec<String> = map
            .iter()
            .filter(|(_, e)| self.is_expired(e, now))
            .map(|(k, _)| k.clone())
            .collect();
        for id in &expired {
            map.remove(id);
        }
        expired
    }

    /// Whether `id` is a live session (does not refresh recency).
    #[must_use]
    pub fn contains(&self, id: &str) -> bool {
        self.inner
            .read()
            .expect("session store lock poisoned")
            .contains_key(id)
    }

    /// Record the session's `logging/setLevel` choice. Returns whether the
    /// session exists.
    pub fn set_log_level(&self, id: &str, level: turbomcp4_core::LogLevel) -> bool {
        let mut map = self.inner.write().expect("session store lock poisoned");
        match map.get_mut(id) {
            Some(entry) => {
                entry.state.log_level = Some(level);
                entry.last_seen = Instant::now();
                true
            }
            None => false,
        }
    }

    /// Terminate a session. Returns whether it existed.
    pub fn remove(&self, id: &str) -> bool {
        self.inner
            .write()
            .expect("session store lock poisoned")
            .remove(id)
            .is_some()
    }

    /// Number of live sessions.
    #[must_use]
    pub fn len(&self) -> usize {
        self.inner
            .read()
            .expect("session store lock poisoned")
            .len()
    }

    /// Whether no sessions are live.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

impl Default for SessionStore {
    fn default() -> Self {
        Self::with_capacity(Self::DEFAULT_CAPACITY)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state() -> SessionState {
        SessionState {
            version: ProtocolVersion::V2025_11_25,
            client_info: Implementation::new("test-client", "1.0"),
            client_capabilities: serde_json::json!({}),
            log_level: None,
        }
    }

    #[test]
    fn insert_get_remove_round_trip() {
        let store = SessionStore::default();
        store.insert("a", state());
        assert!(store.contains("a"));
        assert_eq!(store.get("a").unwrap().client_info.name, "test-client");
        assert!(store.remove("a"));
        assert!(store.get("a").is_none());
        assert!(!store.remove("a"));
    }

    #[test]
    fn capacity_evicts_least_recently_seen() {
        let store = SessionStore::with_capacity(2);
        store.insert("a", state());
        store.insert("b", state());
        let _ = store.get("a"); // refresh "a"; "b" is now oldest
        store.insert("c", state());
        assert!(store.contains("a"));
        assert!(!store.contains("b"));
        assert!(store.contains("c"));
    }

    #[test]
    fn idle_timeout_expires_on_get() {
        let store = SessionStore::with_capacity(8).with_idle_timeout(Some(Duration::from_millis(10)));
        store.insert("a", state());
        assert!(store.get("a").is_some()); // refreshes recency
        std::thread::sleep(Duration::from_millis(25));
        assert!(store.get("a").is_none(), "idle past the timeout → gone");
        assert!(!store.contains("a"), "the expired entry was dropped");
    }

    #[test]
    fn sweep_expired_reclaims_idle_sessions() {
        let store = SessionStore::with_capacity(8).with_idle_timeout(Some(Duration::from_millis(10)));
        store.insert("a", state());
        store.insert("b", state());
        std::thread::sleep(Duration::from_millis(25));
        store.insert("c", state()); // fresh
        let mut swept = store.sweep_expired();
        swept.sort();
        assert_eq!(swept, vec!["a".to_owned(), "b".to_owned()]);
        assert!(store.contains("c"));
        assert!(store.sweep_expired().is_empty(), "second sweep finds nothing");
    }

    #[test]
    fn no_idle_timeout_never_sweeps() {
        let store = SessionStore::with_capacity(8); // default: no idle timeout
        store.insert("a", state());
        std::thread::sleep(Duration::from_millis(5));
        assert!(store.sweep_expired().is_empty());
        assert!(store.contains("a"));
    }
}
