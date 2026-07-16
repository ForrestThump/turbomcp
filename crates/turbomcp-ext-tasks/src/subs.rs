//! Task-status notification subscriptions (SEP-2663 §Task Status Notifications).
//!
//! A client opens a `subscriptions/listen` stream with a `taskIds` filter; the
//! server records `(taskId → subscriber)` here and pushes `notifications/tasks`
//! (the full [`DetailedTask`](crate::wire::DetailedTask), minus `resultType`)
//! on each status change over the connection's ordered writer
//! ([`turbomcp_service::outbound`]). Every pushed notification carries the
//! originating subscription's id verbatim in
//! `_meta["io.modelcontextprotocol/subscriptionId"]` (a spec MUST for
//! notifications delivered via a `subscriptions/listen` stream). A connection
//! whose writer is gone is pruned on the spot.
//!
//! Task-status notifications are spec-**optional** — clients MUST be able to
//! poll `tasks/get` regardless — so a missing subscription simply means no push.

use std::collections::HashMap;
use std::sync::Mutex;

use serde_json::{Value, json};
use turbomcp_core::{JsonRpcNotification, RequestId};
use turbomcp_service::outbound;

use crate::store::DraftTaskStore;
use crate::wire::DetailedTask;

/// `notifications/tasks` — pushed to subscribers on a task's status change.
pub const NOTIFICATIONS_TASKS: &str = "notifications/tasks";

/// The reserved `_meta` key correlating a stream notification with its
/// subscription.
const SUBSCRIPTION_ID_KEY: &str = "io.modelcontextprotocol/subscriptionId";

/// One listening endpoint: the connection to deliver on and the listen
/// request's id (stamped into each notification's `_meta`).
#[derive(Clone, PartialEq, Eq, Hash)]
struct Subscriber {
    connection: String,
    subscription_id: RequestId,
}

/// Maps each subscribed task id to the subscribers listening for its status.
#[derive(Default)]
pub(crate) struct TaskSubscriptions {
    inner: Mutex<HashMap<String, Vec<Subscriber>>>,
}

impl TaskSubscriptions {
    /// Record that `connection_id`'s listen request `subscription_id` wants
    /// status notifications for `task_ids`.
    pub(crate) fn subscribe(
        &self,
        connection_id: &str,
        subscription_id: &RequestId,
        task_ids: &[String],
    ) {
        let subscriber = Subscriber {
            connection: connection_id.to_owned(),
            subscription_id: subscription_id.clone(),
        };
        let mut map = self.lock();
        for task_id in task_ids {
            let subs = map.entry(task_id.clone()).or_default();
            if !subs.contains(&subscriber) {
                subs.push(subscriber.clone());
            }
        }
    }

    /// The subscribers listening to `task_id`.
    fn subscribers(&self, task_id: &str) -> Vec<Subscriber> {
        self.lock().get(task_id).cloned().unwrap_or_default()
    }

    /// Drop `connection_id` from every task's subscriber set (its writer is
    /// gone). Empties are reclaimed.
    fn drop_connection(&self, connection_id: &str) {
        let mut map = self.lock();
        map.retain(|_, subs| {
            subs.retain(|s| s.connection != connection_id);
            !subs.is_empty()
        });
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<String, Vec<Subscriber>>> {
        self.inner.lock().expect("task subscriptions poisoned")
    }
}

/// Push `notifications/tasks` for `task_id` to every subscriber, pruning
/// connections whose writer has closed. No-op if nobody is subscribed or the
/// task is gone.
pub(crate) async fn push_status(subs: &TaskSubscriptions, store: &DraftTaskStore, task_id: &str) {
    let subscribers = subs.subscribers(task_id);
    if subscribers.is_empty() {
        return;
    }
    let Some(detailed) = store.get(task_id) else {
        return;
    };
    let base = notification_params(&detailed);
    for subscriber in subscribers {
        match outbound::writer(&subscriber.connection) {
            Some(writer) => {
                let params = stamp_subscription_id(base.clone(), &subscriber.subscription_id);
                let note = JsonRpcNotification::new(NOTIFICATIONS_TASKS, Some(params));
                if writer.send(note.into()).await.is_err() {
                    subs.drop_connection(&subscriber.connection);
                }
            }
            None => subs.drop_connection(&subscriber.connection),
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

/// Stamp the subscription's id — verbatim, string or number — into the
/// notification's `_meta` (subscriptions spec: the server MUST include it on
/// every notification delivered via a listen stream).
fn stamp_subscription_id(mut params: Value, id: &RequestId) -> Value {
    let id_value = serde_json::to_value(id).unwrap_or(Value::Null);
    if let Some(obj) = params.as_object_mut() {
        let meta = obj
            .entry("_meta")
            .or_insert_with(|| Value::Object(serde_json::Map::new()));
        if let Some(meta_obj) = meta.as_object_mut() {
            meta_obj.insert(SUBSCRIPTION_ID_KEY.to_owned(), id_value);
        }
    } else {
        params = json!({ "_meta": { SUBSCRIPTION_ID_KEY: id_value } });
    }
    params
}
