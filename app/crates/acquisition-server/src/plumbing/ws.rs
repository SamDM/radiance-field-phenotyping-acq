use std::time::Duration;

use futures_util::SinkExt;
use tokio::time::{Interval, MissedTickBehavior};
use tokio_tungstenite::tungstenite::Message;

/// Sends `msg` on `sink`, bounding the wait with `timeout`.
///
/// Returns `Ok(true)` when the send completed, `Ok(false)` when it timed out.
/// A timeout means the socket is wedged (peer stopped draining / froze), so the
/// caller should treat it as a disconnect. Real send errors are propagated.
///
/// Every send — preview frame, control event, and heartbeat ping — goes through
/// this so a wedged send can never block a handler loop indefinitely (which
/// would otherwise defeat the heartbeat, since a stuck send never returns to the
/// `select!` point).
pub async fn send_or_dead<S>(sink: &mut S, msg: Message, timeout: Duration) -> Result<bool, S::Error>
where
    S: SinkExt<Message> + Unpin,
{
    match tokio::time::timeout(timeout, sink.send(msg)).await {
        Ok(res) => res.map(|()| true),
        Err(_) => Ok(false),
    }
}

/// Tracks WebSocket ping/pong liveness for a single client connection.
///
/// Each tick, if the previous ping was never answered the peer is declared dead;
/// otherwise a fresh ping is marked in-flight (the caller sends it). This is the
/// only disconnect detector that survives an SSH/port-forward tunnel, which does
/// not propagate the client's TCP FIN to the server-side socket.
pub struct Heartbeat {
    interval: Interval,
    awaiting_pong: bool,
}

impl Heartbeat {
    pub fn new(period: Duration) -> Self {
        let mut interval = tokio::time::interval(period);
        // If a tick is delayed (e.g. a slow send held the loop), don't fire a
        // burst of catch-up ticks afterwards.
        interval.set_missed_tick_behavior(MissedTickBehavior::Delay);
        Self { interval, awaiting_pong: false }
    }

    /// Resolves on the next heartbeat tick.
    pub async fn tick(&mut self) {
        self.interval.tick().await;
    }

    /// Call on each tick. Returns `true` if the previous ping is still
    /// unanswered (=> client is dead). Otherwise marks a ping as in-flight and
    /// returns `false`; the caller should then send a `Message::Ping`.
    pub fn on_tick(&mut self) -> bool {
        if self.awaiting_pong {
            return true;
        }
        self.awaiting_pong = true;
        false
    }

    /// Call when a `Pong` is received from the client.
    pub fn on_pong(&mut self) {
        self.awaiting_pong = false;
    }
}
