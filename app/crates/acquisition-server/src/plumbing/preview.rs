use std::time::Duration;

use acquisition_common::protocol::PreviewFrame;
use futures_util::StreamExt;
use tokio::net::TcpStream;
use tokio::sync::broadcast;
use tokio_tungstenite::tungstenite::Message;
use tracing::warn;

use super::ws::{send_or_dead, Heartbeat};

/// starts an infinite loop to communicate with a client.
/// This function will receive camera frames from the server (preview_rx) and forward them to the client (stream)
/// I.e. it dumps frames received internally (preview_rx) onto a websocket (stream)
pub async fn handle(
    stream: TcpStream,
    mut preview_rx: broadcast::Receiver<Vec<PreviewFrame>>,
    heartbeat_interval: Duration,
    send_timeout: Duration,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let ws = tokio_tungstenite::accept_async(stream).await?;
    let (mut sink, mut source) = ws.split();

    let mut heartbeat = Heartbeat::new(heartbeat_interval);

    loop {
        tokio::select! {
            _ = heartbeat.tick() => {
                if heartbeat.on_tick() {
                    return Ok(()); // previous ping never answered → client gone
                }
                if !send_or_dead(&mut sink, Message::Ping(Vec::new().into()), send_timeout).await? {
                    return Ok(()); // ping send wedged → client gone
                }
            }
            msg = source.next() => {
                match msg {
                    None | Some(Ok(Message::Close(_))) | Some(Err(_)) => return Ok(()),
                    Some(Ok(Message::Pong(_))) => heartbeat.on_pong(),
                    Some(Ok(other)) => warn!(?other, "unexpected preview frame"),
                }
            }
            recv = preview_rx.recv() => {
                match recv {
                    Ok(frames) => {
                        let bytes = postcard::to_allocvec(&frames)?;
                        if !send_or_dead(&mut sink, Message::Binary(bytes.into()), send_timeout).await? {
                            return Ok(()); // frame send wedged → client gone
                        }
                    }
                    Err(broadcast::error::RecvError::Lagged(n)) => {
                        warn!(missed = n, "preview subscriber lagged");
                    }
                    Err(broadcast::error::RecvError::Closed) => return Ok(()),
                }
            }
        }
    }
}
