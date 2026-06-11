//! Subscription registry + [`ServerNotifier`] (draft `subscriptions/listen`).
//!
//! A subscription is `(connection, listen-request id)` plus the filter subset
//! the server agreed to honor (subscriptions spec: the server **MUST NOT**
//! send notification types the client didn't opt in to). Delivery resolves the
//! connection's ordered writer lazily via [`turbomcp4_service::outbound`] —
//! a missing writer means the connection closed, and the subscription is
//! pruned on the spot (on stdio the server holds no subscription state across
//! reconnections, per spec).
//!
//! `*_list_changed` publishes are coalesced: bursts inside
//! [`COALESCE_WINDOW_MS`] collapse into one notification per kind.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{Value, json};
use turbomcp4_core::{JsonRpcMessage, JsonRpcNotification, RequestId, meta};
use turbomcp4_protocol::methods;
use turbomcp4_protocol::v2026_draft::types as draft;
use turbomcp4_service::outbound;

/// How long a `*_list_changed` burst is allowed to accumulate before the one
/// coalesced notification goes out.
pub(crate) const COALESCE_WINDOW_MS: u64 = 50;

/// The list-changed notification kinds (also the coalescing slots).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum ListChangedKind {
    Tools,
    Resources,
    Prompts,
}

impl ListChangedKind {
    fn method(self) -> &'static str {
        match self {
            Self::Tools => methods::notification::TOOLS_LIST_CHANGED,
            Self::Resources => methods::notification::RESOURCES_LIST_CHANGED,
            Self::Prompts => methods::notification::PROMPTS_LIST_CHANGED,
        }
    }

    fn slot(self) -> usize {
        match self {
            Self::Tools => 0,
            Self::Resources => 1,
            Self::Prompts => 2,
        }
    }

    fn wants(self, filter: &draft::SubscriptionFilter) -> bool {
        match self {
            Self::Tools => filter.tools_list_changed == Some(true),
            Self::Resources => filter.resources_list_changed == Some(true),
            Self::Prompts => filter.prompts_list_changed == Some(true),
        }
    }
}

/// Shared map of live subscriptions; dispatcher clones share it via `Arc`.
#[derive(Default)]
pub(crate) struct SubscriptionRegistry {
    inner: Mutex<HashMap<(String, RequestId), draft::SubscriptionFilter>>,
    /// One pending-flush flag per [`ListChangedKind`] slot.
    pending: [AtomicBool; 3],
}

impl SubscriptionRegistry {
    pub(crate) fn insert(
        &self,
        connection: &str,
        id: &RequestId,
        filter: draft::SubscriptionFilter,
    ) {
        self.lock()
            .insert((connection.to_owned(), id.clone()), filter);
    }

    /// Drop the subscription opened by `(connection, id)`, if any. Wired to
    /// `notifications/cancelled` referencing the listen request id.
    pub(crate) fn remove(&self, connection: &str, id: &RequestId) -> bool {
        self.lock()
            .remove(&(connection.to_owned(), id.clone()))
            .is_some()
    }

    /// Coalesced `*_list_changed`: the first call in a window schedules one
    /// flush; further calls inside the window are absorbed.
    pub(crate) fn schedule_list_changed(self: &Arc<Self>, kind: ListChangedKind) {
        if self.pending[kind.slot()].swap(true, Ordering::AcqRel) {
            return; // a flush is already scheduled
        }
        let registry = Arc::clone(self);
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(COALESCE_WINDOW_MS)).await;
            registry.pending[kind.slot()].store(false, Ordering::Release);
            registry
                .publish(kind.method(), None, |f| kind.wants(f))
                .await;
        });
    }

    /// Immediate `notifications/resources/updated` to every subscription that
    /// listed `uri` in its `resourceSubscriptions`.
    pub(crate) async fn publish_resource_updated(&self, uri: &str) {
        self.publish(
            methods::notification::RESOURCES_UPDATED,
            Some(("uri", json!(uri))),
            |f| f.resource_subscriptions.iter().any(|u| u == uri),
        )
        .await;
    }

    /// Deliver `method` to every subscription whose filter passes `wants`,
    /// stamping each copy with its subscription id. Subscriptions whose
    /// connection is gone are pruned.
    async fn publish(
        &self,
        method: &str,
        extra: Option<(&str, Value)>,
        wants: impl Fn(&draft::SubscriptionFilter) -> bool,
    ) {
        let targets: Vec<(String, RequestId)> = self
            .lock()
            .iter()
            .filter(|(_, filter)| wants(filter))
            .map(|(key, _)| key.clone())
            .collect();

        for (connection, id) in targets {
            let Some(writer) = outbound::writer(&connection) else {
                self.remove(&connection, &id);
                continue;
            };
            let note = subscription_notification(method, &id, extra.clone());
            if writer.send(note).await.is_err() {
                self.remove(&connection, &id);
            }
        }
    }

    fn lock(
        &self,
    ) -> std::sync::MutexGuard<'_, HashMap<(String, RequestId), draft::SubscriptionFilter>> {
        self.inner.lock().expect("subscription registry poisoned")
    }
}

/// The `_meta.subscriptionId` value for a listen request id. The spec's
/// examples stringify numeric ids (`id: 1` → `"1"`).
pub(crate) fn subscription_id_value(id: &RequestId) -> String {
    match id {
        RequestId::Number(n) => n.to_string(),
        RequestId::String(s) => s.clone(),
    }
}

/// Build one stream notification, stamped with its subscription id.
fn subscription_notification(
    method: &str,
    id: &RequestId,
    extra: Option<(&str, Value)>,
) -> JsonRpcMessage {
    let mut params = serde_json::Map::new();
    params.insert(
        "_meta".to_owned(),
        json!({ meta::keys::SUBSCRIPTION_ID: subscription_id_value(id) }),
    );
    if let Some((key, value)) = extra {
        params.insert(key.to_owned(), value);
    }
    JsonRpcNotification::new(method, Some(Value::Object(params))).into()
}

/// Publishes server-side change events to every live subscription. Cheap to
/// clone; obtained from
/// [`VersionDispatcher::notifier`](crate::VersionDispatcher::notifier).
///
/// `*_list_changed` events are coalesced (bursts collapse into one
/// notification); `resource_updated` is delivered immediately, with the
/// writer's backpressure applied.
#[derive(Clone)]
pub struct ServerNotifier {
    subs: Arc<SubscriptionRegistry>,
}

impl ServerNotifier {
    pub(crate) fn new(subs: Arc<SubscriptionRegistry>) -> Self {
        Self { subs }
    }

    /// The tool list changed (`notifications/tools/list_changed`).
    pub fn tools_list_changed(&self) {
        self.subs.schedule_list_changed(ListChangedKind::Tools);
    }

    /// The resource list changed (`notifications/resources/list_changed`).
    pub fn resources_list_changed(&self) {
        self.subs.schedule_list_changed(ListChangedKind::Resources);
    }

    /// The prompt list changed (`notifications/prompts/list_changed`).
    pub fn prompts_list_changed(&self) {
        self.subs.schedule_list_changed(ListChangedKind::Prompts);
    }

    /// `uri`'s content changed (`notifications/resources/updated`), delivered
    /// to every subscription that listed it.
    pub async fn resource_updated(&self, uri: &str) {
        self.subs.publish_resource_updated(uri).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn filter(tools: bool, uris: &[&str]) -> draft::SubscriptionFilter {
        draft::SubscriptionFilter {
            tools_list_changed: tools.then_some(true),
            resources_list_changed: None,
            prompts_list_changed: None,
            resource_subscriptions: uris.iter().map(|s| (*s).to_owned()).collect(),
        }
    }

    #[tokio::test]
    async fn publish_respects_filters_and_stamps_subscription_id() {
        let (tx, mut rx) = tokio::sync::mpsc::channel(8);
        let _guard = outbound::register("sub-test-conn", tx);
        let reg = Arc::new(SubscriptionRegistry::default());
        reg.insert(
            "sub-test-conn",
            &RequestId::from(1i64),
            filter(true, &["file://a"]),
        );
        reg.insert("sub-test-conn", &RequestId::from(2i64), filter(false, &[]));

        reg.publish_resource_updated("file://a").await;
        reg.publish(methods::notification::TOOLS_LIST_CHANGED, None, |f| {
            f.tools_list_changed == Some(true)
        })
        .await;

        let mut methods_seen = Vec::new();
        while let Ok(msg) = rx.try_recv() {
            let JsonRpcMessage::Notification(n) = msg else {
                panic!("expected notification");
            };
            let meta = &n.params.as_ref().unwrap()["_meta"];
            assert_eq!(
                meta[meta::keys::SUBSCRIPTION_ID],
                "1",
                "only subscription 1 opted in to anything"
            );
            methods_seen.push(n.method);
        }
        assert_eq!(
            methods_seen,
            vec![
                methods::notification::RESOURCES_UPDATED.to_owned(),
                methods::notification::TOOLS_LIST_CHANGED.to_owned(),
            ]
        );
    }

    #[tokio::test]
    async fn dead_connections_are_pruned_on_publish() {
        let reg = Arc::new(SubscriptionRegistry::default());
        reg.insert(
            "never-registered",
            &RequestId::from(1i64),
            filter(true, &[]),
        );
        reg.publish(methods::notification::TOOLS_LIST_CHANGED, None, |_| true)
            .await;
        assert!(
            !reg.remove("never-registered", &RequestId::from(1i64)),
            "publish should have pruned the dead subscription"
        );
    }

    #[tokio::test]
    async fn list_changed_bursts_coalesce_into_one_notification() {
        let (tx, mut rx) = tokio::sync::mpsc::channel(8);
        let _guard = outbound::register("coalesce-conn", tx);
        let reg = Arc::new(SubscriptionRegistry::default());
        reg.insert("coalesce-conn", &RequestId::from(1i64), filter(true, &[]));

        let notifier = ServerNotifier::new(Arc::clone(&reg));
        for _ in 0..5 {
            notifier.tools_list_changed();
        }
        tokio::time::sleep(Duration::from_millis(COALESCE_WINDOW_MS * 3)).await;

        let first = rx.try_recv().expect("one coalesced notification");
        assert!(matches!(
            first,
            JsonRpcMessage::Notification(n) if n.method == methods::notification::TOOLS_LIST_CHANGED
        ));
        assert!(rx.try_recv().is_err(), "the burst coalesced into one");
    }
}
