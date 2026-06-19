//! Task-status notification subscriptions (SEP-2663 §Task Status Notifications).
//!
//! A client opens a `subscriptions/listen` stream with a `taskIds` filter; the
//! server records `(taskId → connection)` here and pushes `notifications/tasks`
//! (the full [`DetailedTask`](crate::wire::DetailedTask), minus `resultType`)
//! on each status change over the connection's ordered writer
//! ([`turbomcp4_service::outbound`]). A connection whose writer is gone is
//! pruned on the spot.
//!
//! Task-status notifications are spec-**optional** — clients MUST be able to
//! poll `tasks/get` regardless — so a missing subscription simply means no push.

use std::collections::{HashMap, HashSet};
use std::sync::Mutex;

use serde_json::Value;
use turbomcp4_core::JsonRpcNotification;
use turbomcp4_service::outbound;

use crate::store::DraftTaskStore;
use crate::wire::DetailedTask;

/// `notifications/tasks` — pushed to subscribers on a task's status change.
pub const NOTIFICATIONS_TASKS: &str = "notifications/tasks";

/// Maps each subscribed task id to the connections listening for its status.
#[derive(Default)]
pub(crate) struct TaskSubscriptions {
    inner: Mutex<HashMap<String, HashSet<String>>>,
}

impl TaskSubscriptions {
    /// Record that `connection_id` wants status notifications for `task_ids`.
    pub(crate) fn subscribe(&self, connection_id: &str, task_ids: &[String]) {
        let mut map = self.lock();
        for task_id in task_ids {
            map.entry(task_id.clone())
                .or_default()
                .insert(connection_id.to_owned());
        }
    }

    /// The connections subscribed to `task_id`.
    fn subscribers(&self, task_id: &str) -> Vec<String> {
        self.lock()
            .get(task_id)
            .map(|set| set.iter().cloned().collect())
            .unwrap_or_default()
    }

    /// Drop `connection_id` from every task's subscriber set (its writer is
    /// gone). Empties are reclaimed.
    fn drop_connection(&self, connection_id: &str) {
        let mut map = self.lock();
        map.retain(|_, conns| {
            conns.remove(connection_id);
            !conns.is_empty()
        });
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<String, HashSet<String>>> {
        self.inner.lock().expect("task subscriptions poisoned")
    }
}

/// Push `notifications/tasks` for `task_id` to every subscriber, pruning
/// connections whose writer has closed. No-op if nobody is subscribed or the
/// task is gone.
pub(crate) async fn push_status(subs: &TaskSubscriptions, store: &DraftTaskStore, task_id: &str) {
    let connections = subs.subscribers(task_id);
    if connections.is_empty() {
        return;
    }
    let Some(detailed) = store.get(task_id) else {
        return;
    };
    let params = notification_params(&detailed);
    for connection in connections {
        match outbound::writer(&connection) {
            Some(writer) => {
                let note = JsonRpcNotification::new(NOTIFICATIONS_TASKS, Some(params.clone()));
                if writer.send(note.into()).await.is_err() {
                    subs.drop_connection(&connection);
                }
            }
            None => subs.drop_connection(&connection),
        }
    }
}

/// The `notifications/tasks` params: the full `DetailedTask` carries a
/// `resultType` (it doubles as the `tasks/get` result), but a notification
/// isn't a result — strip it (SEP-2663 §Task Status Notifications example).
fn notification_params(detailed: &DetailedTask) -> Value {
    let mut value = serde_json::to_value(detailed).unwrap_or(Value::Null);
    if let Some(obj) = value.as_object_mut() {
        obj.remove("resultType");
    }
    value
}
