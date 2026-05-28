use acquisition_common::protocol::{ClientMessage, ServerMessage};
use futures_util::{SinkExt, StreamExt};
use tokio::net::TcpStream;
use tokio::sync::{broadcast, mpsc};
use tokio_tungstenite::tungstenite::Message;
use tracing::{info, warn};

/// starts an infinite loop to communicate with a client
/// This function will:
/// - forward control commands from the client (stream) to the server (cmd_tx)
/// - receive state events from the server (evt_rx) and forward them to the client (stream)
pub async fn handle(
    stream: TcpStream,
    cmd_tx: mpsc::UnboundedSender<ClientMessage>,
    mut evt_rx: broadcast::Receiver<ServerMessage>,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let ws = tokio_tungstenite::accept_async(stream).await?;
    let (mut sink, mut stream) = ws.split();

    let reader = async move {
        while let Some(msg) = stream.next().await {
            match msg? {
                Message::Text(text) => match serde_json::from_str::<ClientMessage>(&text) {
                    Ok(cmd) => {
                        info!(?cmd, "received command");
                        if cmd_tx.send(cmd).is_err() {
                            return Ok::<_, Box<dyn std::error::Error + Send + Sync>>(());
                        }
                    }
                    Err(e) => warn!(error = %e, raw = %text, "failed to parse control message"),
                },
                Message::Close(_) => return Ok(()),
                Message::Ping(_) | Message::Pong(_) => {}
                other => warn!(?other, "unexpected control frame"),
            }
        }
        Ok(())
    };

    let writer = async move {
        loop {
            match evt_rx.recv().await {
                Ok(msg) => {
                    let text = serde_json::to_string(&msg)?;
                    sink.send(Message::Text(text.into())).await?;
                }
                Err(broadcast::error::RecvError::Lagged(n)) => {
                    warn!(missed = n, "control event subscriber lagged");
                }
                Err(broadcast::error::RecvError::Closed) => {
                    return Ok::<_, Box<dyn std::error::Error + Send + Sync>>(());
                }
            }
        }
    };

    tokio::select! {
        r = reader => r,
        r = writer => r,
    }
}
