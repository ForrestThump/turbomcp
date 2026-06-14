//! The session-termination seam.
//!
//! The `2025-11-25` Streamable HTTP transport lets a client end a session with
//! an HTTP `DELETE` (spec §Session Management). The session table lives in the
//! server layer (`turbomcp4-server`), which the HTTP transport doesn't depend
//! on — so, like [auth](crate::HttpAuthenticator), termination crosses the
//! boundary through a small `service`-level trait the server implements and the
//! transport holds behind `Arc<dyn …>`.

/// Terminates a server session by id (backs HTTP `DELETE`). Implemented by the
/// dispatcher (it drops the session state *and* its subscription routes);
/// obtained from `VersionDispatcher::session_terminator`.
pub trait SessionTerminator: Send + Sync {
    /// Terminate the session `session_id`. Returns whether it existed (the
    /// transport answers `204` vs `404` accordingly).
    fn terminate(&self, session_id: &str) -> bool;
}
