//! Server-Sent Events helpers for streaming responses.

use std::convert::Infallible;
use std::time::Duration;

use axum::response::sse::{Event, KeepAlive, Sse};
use futures_util::stream::Stream;
use tokio_stream::wrappers::BroadcastStream;
use tokio_stream::StreamExt;

/// Create an SSE response from a broadcast receiver.
///
/// Events are serialized as JSON. A keep-alive ping is sent every 15 seconds
/// to prevent proxies from closing the connection.
pub fn from_broadcast(
    rx: tokio::sync::broadcast::Receiver<SseEvent>,
) -> Sse<impl Stream<Item = Result<Event, Infallible>>> {
    let stream = BroadcastStream::new(rx).filter_map(|result| match result {
        Ok(event) => {
            let sse = Event::default()
                .event(&event.event_type)
                .json_data(&event.data)
                .ok()?;
            Some(Ok(sse))
        }
        Err(_) => None, // lagged — skip
    });

    Sse::new(stream).keep_alive(
        KeepAlive::new()
            .interval(Duration::from_secs(15))
            .text("ping"),
    )
}

/// An event to broadcast over SSE.
#[derive(Debug, Clone)]
pub struct SseEvent {
    pub event_type: String,
    pub data: serde_json::Value,
}
