//! The framework-owned task registry behind core `tasks/*` (2025-11-25).
//!
//! Tasks are CORE in `2025-11-25` (AUDIT F9): a client may augment `tools/call`
//! with a `task` field, get a `CreateTaskResult` immediately, and poll
//! `tasks/get` / block on `tasks/result` for the outcome. This module is the
//! *store* — deliberately wire-agnostic so the draft Tasks *extension*
//! (Phase 8) can front the same registry ("one TaskStore, two front-ends").
//!
//! Spec behavior encoded here (tasks.mdx §Behavior Requirements):
//! - tasks begin `working`; terminal states (`completed`/`failed`/`cancelled`)
//!   never transition again;
//! - `createdAt`/`lastUpdatedAt` are RFC 3339; the *actual* `ttl` is reported
//!   (requests are clamped to [`TaskStore::MAX_TTL_MS`], defaulted to
//!   [`TaskStore::DEFAULT_TTL_MS`]);
//! - expired tasks may be deleted regardless of status (purged lazily here;
//!   in-flight work is cancelled via the task's token);
//! - unknown ids and terminal-state cancels are `-32602` at the dispatch
//!   layer ([`TaskError`] carries the distinction).
//!
//! Tasks are scoped to the session that created them: `get`/`list`/`cancel`/
//! `wait_result` only see tasks minted under the same session id, so one
//! HTTP session cannot observe (or cancel) another's work.
//!
//! `notifications/tasks/status` is spec-optional and needs the Phase 6
//! server→client push seam; polling is the contract until then.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use serde_json::Value;
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;
use tokio::sync::watch;
use turbomcp4_core::{CancellationToken, JsonRpcError};

/// Internal task status (rendered to the wire by the dispatcher).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum TaskStatus {
    Working,
    Completed,
    Failed,
    Cancelled,
}

impl TaskStatus {
    pub(crate) fn is_terminal(self) -> bool {
        matches!(self, Self::Completed | Self::Failed | Self::Cancelled)
    }
}

/// A point-in-time copy of one task's externally visible state.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct TaskSnapshot {
    pub task_id: String,
    pub status: TaskStatus,
    pub status_message: Option<String>,
    /// RFC 3339.
    pub created_at: String,
    /// RFC 3339.
    pub last_updated_at: String,
    /// Actual retention from creation, in milliseconds.
    pub ttl_ms: i64,
}

/// Why a task operation failed (mapped to JSON-RPC codes by the dispatcher).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum TaskError {
    /// No live task with that id under this session (`-32602`).
    NotFound,
    /// `tasks/cancel` on a task already in a terminal status (`-32602`).
    AlreadyTerminal,
    /// The registry is full; the task-augmented request is rejected (`-32603`).
    CapacityExhausted,
}

struct TaskEntry {
    session_id: String,
    status: TaskStatus,
    status_message: Option<String>,
    created_wall: OffsetDateTime,
    updated_wall: OffsetDateTime,
    created: Instant,
    ttl_ms: i64,
    /// The underlying request's outcome, present once terminal: the serialized
    /// wire result, or the JSON-RPC error it would have answered with.
    outcome: Option<Result<Value, JsonRpcError>>,
    /// Fires the spawned handler's cancellation.
    cancel: CancellationToken,
    /// Notifies `wait_result` blockers on every status transition.
    notify: watch::Sender<()>,
}

impl TaskEntry {
    fn expired(&self, now: Instant) -> bool {
        let ttl = u64::try_from(self.ttl_ms).unwrap_or(0);
        now.duration_since(self.created) > Duration::from_millis(ttl)
    }

    fn snapshot(&self, id: &str) -> TaskSnapshot {
        TaskSnapshot {
            task_id: id.to_owned(),
            status: self.status,
            status_message: self.status_message.clone(),
            created_at: rfc3339(self.created_wall),
            last_updated_at: rfc3339(self.updated_wall),
            ttl_ms: self.ttl_ms,
        }
    }
}

fn rfc3339(t: OffsetDateTime) -> String {
    t.format(&Rfc3339)
        .unwrap_or_else(|_| String::from("1970-01-01T00:00:00Z"))
}

/// Bounded, session-scoped, in-memory task registry.
pub(crate) struct TaskStore {
    inner: Mutex<HashMap<String, TaskEntry>>,
    capacity: usize,
}

impl TaskStore {
    /// Retention when the client doesn't request a `ttl`, in milliseconds.
    pub(crate) const DEFAULT_TTL_MS: i64 = 300_000; // 5 minutes
    /// Hard upper bound on retention (the spec lets receivers override).
    pub(crate) const MAX_TTL_MS: i64 = 3_600_000; // 1 hour
    /// Suggested client polling interval, in milliseconds.
    pub(crate) const POLL_INTERVAL_MS: i64 = 500;

    const DEFAULT_CAPACITY: usize = 1024;

    /// Create a task in `working` status, owned by `session_id`, driven by
    /// `cancel`. Returns the snapshot to render as `CreateTaskResult`.
    pub(crate) fn create(
        &self,
        session_id: String,
        requested_ttl_ms: Option<i64>,
        cancel: CancellationToken,
    ) -> Result<TaskSnapshot, TaskError> {
        let mut map = self.inner.lock().expect("task store lock poisoned");
        Self::purge_expired(&mut map);
        if map.len() >= self.capacity {
            return Err(TaskError::CapacityExhausted);
        }
        let id = uuid::Uuid::new_v4().to_string();
        let now_wall = OffsetDateTime::now_utc();
        let (notify, _) = watch::channel(());
        let entry = TaskEntry {
            session_id,
            status: TaskStatus::Working,
            status_message: None,
            created_wall: now_wall,
            updated_wall: now_wall,
            created: Instant::now(),
            ttl_ms: requested_ttl_ms
                .unwrap_or(Self::DEFAULT_TTL_MS)
                .clamp(0, Self::MAX_TTL_MS),
            outcome: None,
            cancel,
            notify,
        };
        let snap = entry.snapshot(&id);
        map.insert(id, entry);
        Ok(snap)
    }

    /// Record the underlying request's outcome: `Ok` ⇒ `completed`,
    /// `Err` ⇒ `failed`. No-op if the task is already terminal (a cancel won
    /// the race) or was purged.
    pub(crate) fn complete(&self, id: &str, outcome: Result<Value, JsonRpcError>) {
        let mut map = self.inner.lock().expect("task store lock poisoned");
        let Some(entry) = map.get_mut(id) else {
            return;
        };
        if entry.status.is_terminal() {
            return;
        }
        entry.status = match &outcome {
            Ok(_) => TaskStatus::Completed,
            Err(_) => TaskStatus::Failed,
        };
        if let Err(e) = &outcome {
            entry.status_message = Some(e.message.clone());
        }
        entry.outcome = Some(outcome);
        entry.updated_wall = OffsetDateTime::now_utc();
        let _ = entry.notify.send(());
    }

    /// `tasks/cancel`: fire the task's token and transition to `cancelled`.
    pub(crate) fn cancel(&self, session_id: &str, id: &str) -> Result<TaskSnapshot, TaskError> {
        let mut map = self.inner.lock().expect("task store lock poisoned");
        Self::purge_expired(&mut map);
        let entry = match map.get_mut(id) {
            Some(e) if e.session_id == session_id => e,
            _ => return Err(TaskError::NotFound),
        };
        if entry.status.is_terminal() {
            return Err(TaskError::AlreadyTerminal);
        }
        entry.cancel.cancel();
        entry.status = TaskStatus::Cancelled;
        entry.status_message = Some("the task was cancelled by request".to_owned());
        // The underlying request never finished; its "result" is the
        // cancellation error (implementation-defined code, LSP convention).
        entry.outcome = Some(Err(JsonRpcError {
            code: -32800,
            message: "task cancelled".to_owned(),
            data: None,
        }));
        entry.updated_wall = OffsetDateTime::now_utc();
        let _ = entry.notify.send(());
        Ok(entry.snapshot(id))
    }

    /// `tasks/get`: the task's current state.
    pub(crate) fn get(&self, session_id: &str, id: &str) -> Result<TaskSnapshot, TaskError> {
        let mut map = self.inner.lock().expect("task store lock poisoned");
        Self::purge_expired(&mut map);
        match map.get(id) {
            Some(e) if e.session_id == session_id => Ok(e.snapshot(id)),
            _ => Err(TaskError::NotFound),
        }
    }

    /// `tasks/list`: this session's tasks, oldest first, paginated. The cursor
    /// is the stringified offset of the next page.
    pub(crate) fn list(
        &self,
        session_id: &str,
        cursor: Option<&str>,
        page_size: usize,
    ) -> Result<(Vec<TaskSnapshot>, Option<String>), TaskError> {
        let offset = match cursor {
            None => 0,
            Some(c) => c.parse::<usize>().map_err(|_| TaskError::NotFound)?,
        };
        let mut map = self.inner.lock().expect("task store lock poisoned");
        Self::purge_expired(&mut map);
        let mut all: Vec<(&String, &TaskEntry)> = map
            .iter()
            .filter(|(_, e)| e.session_id == session_id)
            .collect();
        all.sort_by_key(|(_, e)| e.created);
        let page: Vec<TaskSnapshot> = all
            .iter()
            .skip(offset)
            .take(page_size)
            .map(|(id, e)| e.snapshot(id))
            .collect();
        let next = (offset + page.len() < all.len()).then(|| (offset + page.len()).to_string());
        Ok((page, next))
    }

    /// `tasks/result`: block until the task is terminal, then return the
    /// underlying request's outcome verbatim (success value or JSON-RPC
    /// error), per spec §Result Retrieval.
    pub(crate) async fn wait_result(
        &self,
        session_id: &str,
        id: &str,
    ) -> Result<Result<Value, JsonRpcError>, TaskError> {
        loop {
            let mut rx = {
                let mut map = self.inner.lock().expect("task store lock poisoned");
                Self::purge_expired(&mut map);
                let entry = match map.get(id) {
                    Some(e) if e.session_id == session_id => e,
                    _ => return Err(TaskError::NotFound),
                };
                if entry.status.is_terminal() {
                    return Ok(entry.outcome.clone().unwrap_or_else(|| {
                        Err(JsonRpcError {
                            code: -32603,
                            message: "task finished without an outcome".to_owned(),
                            data: None,
                        })
                    }));
                }
                entry.notify.subscribe()
            }; // lock released before awaiting
            if rx.changed().await.is_err() {
                // Sender dropped ⇒ task purged while we waited.
                return Err(TaskError::NotFound);
            }
        }
    }

    fn purge_expired(map: &mut HashMap<String, TaskEntry>) {
        let now = Instant::now();
        map.retain(|_, e| {
            let keep = !e.expired(now);
            if !keep && !e.status.is_terminal() {
                e.cancel.cancel();
            }
            keep
        });
    }
}

impl Default for TaskStore {
    fn default() -> Self {
        Self {
            inner: Mutex::new(HashMap::new()),
            capacity: Self::DEFAULT_CAPACITY,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn store() -> TaskStore {
        TaskStore::default()
    }

    #[tokio::test]
    async fn lifecycle_working_to_completed() {
        let s = store();
        let snap = s
            .create("sess".into(), None, CancellationToken::new())
            .unwrap();
        assert_eq!(snap.status, TaskStatus::Working);
        assert_eq!(snap.ttl_ms, TaskStore::DEFAULT_TTL_MS);

        s.complete(&snap.task_id, Ok(json!({"done": true})));
        let got = s.get("sess", &snap.task_id).unwrap();
        assert_eq!(got.status, TaskStatus::Completed);

        let outcome = s.wait_result("sess", &snap.task_id).await.unwrap();
        assert_eq!(outcome.unwrap()["done"], true);
    }

    #[tokio::test]
    async fn wait_result_blocks_until_terminal() {
        let s = std::sync::Arc::new(store());
        let snap = s
            .create("sess".into(), None, CancellationToken::new())
            .unwrap();
        let waiter = {
            let s = std::sync::Arc::clone(&s);
            let id = snap.task_id.clone();
            tokio::spawn(async move { s.wait_result("sess", &id).await })
        };
        tokio::task::yield_now().await;
        s.complete(&snap.task_id, Ok(json!("late")));
        let outcome = waiter.await.unwrap().unwrap();
        assert_eq!(outcome.unwrap(), json!("late"));
    }

    #[tokio::test]
    async fn cancel_fires_token_and_rejects_terminal() {
        let s = store();
        let token = CancellationToken::new();
        let snap = s.create("sess".into(), Some(1000), token.clone()).unwrap();

        let cancelled = s.cancel("sess", &snap.task_id).unwrap();
        assert_eq!(cancelled.status, TaskStatus::Cancelled);
        assert!(token.is_cancelled());

        // Terminal cancel → AlreadyTerminal; completion after cancel is a no-op.
        assert_eq!(
            s.cancel("sess", &snap.task_id),
            Err(TaskError::AlreadyTerminal)
        );
        s.complete(&snap.task_id, Ok(json!("too late")));
        assert_eq!(
            s.get("sess", &snap.task_id).unwrap().status,
            TaskStatus::Cancelled
        );

        let outcome = s.wait_result("sess", &snap.task_id).await.unwrap();
        assert_eq!(outcome.unwrap_err().code, -32800);
    }

    #[tokio::test]
    async fn session_scoping_hides_foreign_tasks() {
        let s = store();
        let snap = s
            .create("alice".into(), None, CancellationToken::new())
            .unwrap();
        assert_eq!(s.get("mallory", &snap.task_id), Err(TaskError::NotFound));
        assert_eq!(s.cancel("mallory", &snap.task_id), Err(TaskError::NotFound));
        let (page, _) = s.list("mallory", None, 10).unwrap();
        assert!(page.is_empty());
        let (page, _) = s.list("alice", None, 10).unwrap();
        assert_eq!(page.len(), 1);
    }

    #[tokio::test]
    async fn list_paginates_with_offset_cursor() {
        let s = store();
        for _ in 0..5 {
            let _ = s
                .create("sess".into(), None, CancellationToken::new())
                .unwrap();
        }
        let (first, next) = s.list("sess", None, 2).unwrap();
        assert_eq!(first.len(), 2);
        let (second, next2) = s.list("sess", next.as_deref(), 2).unwrap();
        assert_eq!(second.len(), 2);
        let (third, end) = s.list("sess", next2.as_deref(), 2).unwrap();
        assert_eq!(third.len(), 1);
        assert!(end.is_none());
        assert_eq!(s.list("sess", Some("bogus"), 2), Err(TaskError::NotFound));
    }

    #[tokio::test]
    async fn ttl_is_clamped_and_reported() {
        let s = store();
        let snap = s
            .create("sess".into(), Some(i64::MAX), CancellationToken::new())
            .unwrap();
        assert_eq!(snap.ttl_ms, TaskStore::MAX_TTL_MS);
        // RFC 3339 timestamps render.
        assert!(snap.created_at.contains('T'));
    }
}
