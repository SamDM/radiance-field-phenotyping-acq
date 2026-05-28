use acquisition_common::protocol::PreviewFrame;
use futures_util::StreamExt;
use tokio::net::TcpStream;
use tokio::sync::broadcast;
use tokio_tungstenite::tungstenite::Message;
use tracing::warn;

/// starts an infinite loop to communicate with a client.
/// This function will receive camera frames from the server (preview_rx) and forward them to the client (stream)
/// I.e. it dumps frames received internally (preview_rx) onto a websocket (stream)
pub async fn handle(
    stream: TcpStream,
    mut preview_rx: broadcast::Receiver<Vec<PreviewFrame>>,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let ws = tokio_tungstenite::accept_async(stream).await?;
    let (mut sink, mut source) = ws.split();

    use futures_util::SinkExt;
    loop {
        tokio::select! {
            msg = source.next() => {
                match msg {
                    None | Some(Ok(Message::Close(_))) | Some(Err(_)) => return Ok(()),
                    _ => {}
                }
            }
            recv = preview_rx.recv() => {
                match recv {
                    Ok(frames) => {
                        let bytes = postcard::to_allocvec(&frames)?;
                        sink.send(Message::Binary(bytes.into())).await?;
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
