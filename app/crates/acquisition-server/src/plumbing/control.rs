use std::time::Duration;

use acquisition_common::protocol::{ClientMessage, ServerMessage};
use futures_util::StreamExt;
use tokio::net::TcpStream;
use tokio::sync::{broadcast, mpsc};
use tokio_tungstenite::tungstenite::Message;
use tracing::{info, warn};

use super::ws::{send_or_dead, Heartbeat};

/// starts an infinite loop to communicate with a client
/// This function will:
/// - forward control commands from the client (stream) to the server (cmd_tx)
/// - receive state events from the server (evt_rx) and forward them to the client (stream)
pub async fn handle(
    stream: TcpStream,
    cmd_tx: mpsc::UnboundedSender<ClientMessage>,
    mut evt_rx: broadcast::Receiver<ServerMessage>,
    heartbeat_interval: Duration,
    send_timeout: Duration,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let ws = tokio_tungstenite::accept_async(stream).await?;
    let (mut sink, mut source) = ws.split();

    let mut heartbeat = Heartbeat::new(heartbeat_interval);

    // Single loop owning both halves of the socket: the heartbeat must both send
    // a ping (sink) and observe the pong (source), so reader/writer can't live in
    // separate tasks.
    loop {
        tokio::select! {
            msg = source.next() => {
                match msg {
                    None | Some(Ok(Message::Close(_))) | Some(Err(_)) => return Ok(()),
                    Some(Ok(Message::Text(text))) => match serde_json::from_str::<ClientMessage>(&text) {
                        Ok(cmd) => {
                            info!(?cmd, "received command");
                            if cmd_tx.send(cmd).is_err() {
                                return Ok(());
                            }
                        }
                        Err(e) => warn!(error = %e, raw = %text, "failed to parse control message"),
                    },
                    Some(Ok(Message::Pong(_))) => heartbeat.on_pong(),
                    Some(Ok(Message::Ping(_))) => {} // tungstenite auto-replies with a Pong
                    Some(Ok(other)) => warn!(?other, "unexpected control frame"),
                }
            }
            recv = evt_rx.recv() => {
                match recv {
                    Ok(server_msg) => {
                        let text = serde_json::to_string(&server_msg)?;
                        if !send_or_dead(&mut sink, Message::Text(text.into()), send_timeout).await? {
                            return Ok(()); // event send wedged → client gone
                        }
                    }
                    Err(broadcast::error::RecvError::Lagged(n)) => {
                        warn!(missed = n, "control event subscriber lagged");
                    }
                    Err(broadcast::error::RecvError::Closed) => return Ok(()),
                }
            }
            _ = heartbeat.tick() => {
                if heartbeat.on_tick() {
                    return Ok(()); // previous ping never answered → client gone
                }
                if !send_or_dead(&mut sink, Message::Ping(Vec::new().into()), send_timeout).await? {
                    return Ok(()); // ping send wedged → client gone
                }
            }
        }
    }
}
