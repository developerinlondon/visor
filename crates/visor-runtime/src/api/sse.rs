//! Server-Sent Events broadcaster for VM lifecycle events.
//!
//! The [`EventBroadcaster`] wraps a `tokio::sync::broadcast` channel to fan out
//! [`VmEvent`]s to all connected SSE clients. Events are emitted by the API
//! route handlers after each VM lifecycle operation.

use std::convert::Infallible;

use axum::extract::State;
use axum::response::sse::{Event, KeepAlive, Sse};
use serde::{Deserialize, Serialize};
use tokio::sync::broadcast;
use tokio_stream::StreamExt as _;
use tokio_stream::wrappers::BroadcastStream;
use utoipa::ToSchema;

use crate::api::router::AppState;

/// A VM lifecycle event broadcast to SSE clients.
///
/// Events are emitted after each VM operation (create, stop, destroy, etc.)
/// and streamed to all connected `/v1/events` clients.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[non_exhaustive]
pub struct VmEvent {
    /// Event type identifier (e.g. `"vm.created"`, `"vm.stopped"`).
    pub event_type: String,
    /// ID of the VM this event relates to.
    pub vm_id: String,
    /// ISO 8601 timestamp of when the event occurred.
    pub timestamp: String,
    /// Arbitrary JSON payload with event-specific data.
    pub data: serde_json::Value,
}

impl VmEvent {
    /// Creates a new event with the given type and VM ID.
    ///
    /// The timestamp is set to `"1970-01-01T00:00:00Z"` as a stub.
    ///
    /// TODO(P1): Use real ISO 8601 timestamps.
    #[must_use]
    pub fn new(event_type: impl Into<String>, vm_id: impl Into<String>) -> Self {
        Self {
            event_type: event_type.into(),
            vm_id: vm_id.into(),
            timestamp: "1970-01-01T00:00:00Z".to_owned(),
            data: serde_json::Value::Null,
        }
    }

    /// Sets the event data payload.
    #[must_use]
    pub fn with_data(mut self, data: serde_json::Value) -> Self {
        self.data = data;
        self
    }
}

/// Broadcasts VM lifecycle events to connected SSE clients.
///
/// Wraps a `tokio::sync::broadcast` channel. Dropped receivers (slow clients)
/// are handled gracefully — lagged events are simply skipped.
#[derive(Debug)]
pub struct EventBroadcaster {
    sender: broadcast::Sender<VmEvent>,
    capacity: usize,
}

impl EventBroadcaster {
    /// Creates a new broadcaster with the given channel capacity.
    ///
    /// # Arguments
    ///
    /// * `capacity` — Maximum number of buffered events before slow receivers
    ///   start losing messages.
    #[must_use]
    pub fn new(capacity: usize) -> Self {
        let (sender, _rx) = broadcast::channel(capacity);
        Self { sender, capacity }
    }

    /// Returns the configured channel capacity.
    #[must_use]
    pub fn capacity(&self) -> usize {
        self.capacity
    }

    /// Broadcasts an event to all connected SSE clients.
    ///
    /// If no clients are connected, the event is silently dropped.
    pub fn send(&self, event: VmEvent) {
        // Ignore SendError — it just means no receivers are listening.
        let _result = self.sender.send(event);
    }

    /// Creates a new receiver for this broadcaster.
    #[must_use]
    pub fn subscribe(&self) -> broadcast::Receiver<VmEvent> {
        self.sender.subscribe()
    }
}

/// SSE endpoint that streams VM lifecycle events to clients.
///
/// Clients receive a continuous stream of server-sent events for VM lifecycle
/// changes. The connection stays open with periodic keep-alive pings.
#[utoipa::path(
    get,
    path = "/v1/events",
    tag = "system",
    responses(
        (status = 200, description = "SSE event stream", content_type = "text/event-stream")
    )
)]
pub async fn event_stream(
    State(state): State<AppState>,
) -> Sse<impl tokio_stream::Stream<Item = Result<Event, Infallible>>> {
    let rx = state.events.subscribe();
    let stream = BroadcastStream::new(rx)
        .filter_map(|result: Result<VmEvent, _>| result.ok())
        .map(|vm_event: VmEvent| {
            let data = serde_json::to_string(&vm_event).unwrap_or_default();
            Ok::<_, Infallible>(Event::default().data(data).event(vm_event.event_type))
        });

    Sse::new(stream).keep_alive(KeepAlive::default())
}

#[cfg(test)]
#[path = "sse_test.rs"]
mod tests;
