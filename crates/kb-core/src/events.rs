//! Change notifications on channel `kb_events` (docs/architecture.md section 4).

use std::time::Duration;

use serde::{Deserialize, Serialize};
use sqlx::postgres::PgListener;
use tokio::sync::mpsc;

use crate::error::{Error, Result};

/// Notification channel name.
pub const CHANNEL: &str = "kb_events";

/// Payload of one notification: a board changed and now has revision `rev`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct BoardEvent {
    pub workspace: i64,
    pub board: i64,
    pub rev: i64,
}

impl BoardEvent {
    /// Parses a `kb_events` payload; `None` if it is not a board event.
    pub fn parse(payload: &str) -> Option<BoardEvent> {
        serde_json::from_str(payload).ok()
    }
}

/// What [`EventListener::next`] observed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ListenEvent {
    /// A board changed.
    Board(BoardEvent),
    /// The connection was lost. Notifications may have been missed; call `next` again
    /// (after a delay) to reconnect.
    Disconnected,
    /// A lost connection was re-established. Reload everything that is shown.
    Reconnected,
}

/// A `LISTEN kb_events` connection with explicit reconnection.
pub struct EventListener {
    url: String,
    inner: Option<PgListener>,
}

impl EventListener {
    /// Connects with `url` (normally the reader URL) and listens on `kb_events`.
    pub async fn connect(url: &str) -> Result<EventListener> {
        let inner = listen(url).await?;
        Ok(EventListener {
            url: url.to_string(),
            inner: Some(inner),
        })
    }

    /// Waits for the next event.
    ///
    /// - Connected: returns the next [`ListenEvent::Board`] (unparseable payloads are
    ///   skipped), or [`ListenEvent::Disconnected`] when the connection drops.
    /// - Disconnected: tries to reconnect once; returns [`ListenEvent::Reconnected`] on
    ///   success or `Err(Error::Unreachable)` on failure. The caller chooses the retry delay.
    pub async fn next(&mut self) -> Result<ListenEvent> {
        let Some(listener) = self.inner.as_mut() else {
            self.inner = Some(listen(&self.url).await?);
            return Ok(ListenEvent::Reconnected);
        };
        loop {
            match listener.try_recv().await {
                Ok(Some(n)) => {
                    if let Some(ev) = BoardEvent::parse(n.payload()) {
                        return Ok(ListenEvent::Board(ev));
                    }
                }
                Ok(None) | Err(_) => {
                    self.inner = None;
                    return Ok(ListenEvent::Disconnected);
                }
            }
        }
    }
}

async fn listen(url: &str) -> Result<PgListener> {
    let connect = async {
        let mut listener = PgListener::connect(url).await?;
        // Reconnection is handled by EventListener, not inside PgListener.
        listener.eager_reconnect(false);
        listener.listen(CHANNEL).await?;
        Ok::<_, sqlx::Error>(listener)
    };
    match tokio::time::timeout(Duration::from_secs(5), connect).await {
        Ok(Ok(l)) => Ok(l),
        Ok(Err(e)) => Err(Error::Unreachable(e.to_string())),
        Err(_) => Err(Error::Unreachable(
            "timed out connecting for LISTEN".to_string(),
        )),
    }
}

/// Retry delays after a lost connection: 1 s, 2 s, 5 s, then every 5 s (docs/tui.md 8.1).
pub const RECONNECT_DELAYS: [Duration; 3] = [
    Duration::from_secs(1),
    Duration::from_secs(2),
    Duration::from_secs(5),
];

/// Runs an [`EventListener`] on a background tokio task and forwards its events,
/// reconnecting with [`RECONNECT_DELAYS`]. The first connection must succeed (otherwise
/// this returns the error). The task ends when the receiver is dropped.
///
/// While disconnected, the channel receives one `Disconnected`, then `Reconnected` once
/// the connection is back.
pub async fn spawn_listener(url: &str) -> Result<mpsc::UnboundedReceiver<ListenEvent>> {
    let mut listener = EventListener::connect(url).await?;
    let (tx, rx) = mpsc::unbounded_channel();
    tokio::spawn(async move {
        let mut failures = 0usize;
        loop {
            match listener.next().await {
                Ok(ev) => {
                    if !matches!(ev, ListenEvent::Disconnected) {
                        failures = 0;
                    }
                    if tx.send(ev).is_err() {
                        return;
                    }
                    if matches!(ev, ListenEvent::Disconnected) {
                        tokio::time::sleep(RECONNECT_DELAYS[0]).await;
                        failures = 1;
                    }
                }
                Err(_) => {
                    let delay = RECONNECT_DELAYS[failures.min(RECONNECT_DELAYS.len() - 1)];
                    failures += 1;
                    if tx.is_closed() {
                        return;
                    }
                    tokio::time::sleep(delay).await;
                }
            }
        }
    });
    Ok(rx)
}
