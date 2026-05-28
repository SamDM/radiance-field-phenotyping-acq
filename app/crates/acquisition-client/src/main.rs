use std::sync::mpsc;

use acquisition_common::protocol::{ClientMessage, PreviewFrame, ServerMessage};
use clap::Parser;
use eframe::egui;
use futures_util::{SinkExt, StreamExt};
use tokio::sync::mpsc as tokio_mpsc;
use tokio_tungstenite::tungstenite::Message;
use tracing::warn;

mod gui;
mod numeric_input_widget;

use gui::{App, AppEvent, EventSender};

#[derive(Parser)]
struct Args {
    #[arg(long, default_value = "ws://127.0.0.1:8081")]
    control_url: String,
    #[arg(long, default_value = "ws://127.0.0.1:8082")]
    preview_url: String,
}

fn main() -> eframe::Result {
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();

    let args = Args::parse();
    let rt = tokio::runtime::Runtime::new().expect("tokio runtime");
    let rt_handle = rt.handle().clone();
    let (event_tx, event_rx) = mpsc::sync_channel::<AppEvent>(256);
    let (cmd_tx, cmd_rx) = tokio_mpsc::unbounded_channel::<ClientMessage>();

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default().with_title("Acquisition Client"),
        ..Default::default()
    };

    let result = eframe::run_native(
        "Acquisition Client",
        options,
        Box::new(move |cc| {
            // setup forwarding between internal channels and server,
            let event_tx = EventSender::new(event_tx, cc.egui_ctx.clone());
            rt_handle.block_on(start_ws_bridges(
                args.control_url,
                args.preview_url,
                event_tx,  // forward server events (WS) -> internal evens (event_tx)
                cmd_rx,    // forward internal commands (cmd_rx) to server (WS)
            ));
            // `start_ws_bridges` above returns near instantly,
            // but at this point the app can start with
            // channel forwarding to the server enabled.
            Ok(Box::new(App::new(event_rx, cmd_tx)))  // TODO, make client ID unique (a string? hardware ID?)
        }),
    );

    // keep tokio rt (and therefore websocket forwarding) alive until app closes
    drop(rt);
    result
}

async fn start_ws_bridges(
    control_url: String,
    preview_url: String,
    event_tx: EventSender,
    cmd_rx: tokio_mpsc::UnboundedReceiver<ClientMessage>,
) {
    let ctrl_tx = event_tx.clone();
    tokio::spawn(run_connection(
        control_url,
        "control",
        ctrl_tx.clone(),
        AppEvent::ControlConnected,
        AppEvent::ControlDisconnected,
        move |ws| control_connection(ws, ctrl_tx, cmd_rx),
    ));

    tokio::spawn(run_connection(
        preview_url,
        "preview",
        event_tx.clone(),
        AppEvent::PreviewConnected,
        AppEvent::PreviewDisconnected,
        move |ws| preview_connection(ws, event_tx),
    ));
}

type Ws = tokio_tungstenite::WebSocketStream<
    tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
>;

/// Connect to `url`, emit `connected` on success, run `body` until it returns,
/// then emit `disconnected` exactly once. On connect failure, nothing is emitted.
async fn run_connection<F, Fut>(
    url: String,
    label: &'static str,
    event_tx: EventSender,
    connected: AppEvent,
    disconnected: AppEvent,
    body: F,
) where
    F: FnOnce(Ws) -> Fut,
    Fut: std::future::Future<Output = ()>,
{
    let ws = match tokio_tungstenite::connect_async(&url).await {
        Ok((ws, _)) => ws,
        Err(e) => {
            warn!(error = %e, label, "connection failed");
            return;
        }
    };
    event_tx.send(connected);
    body(ws).await;
    event_tx.send(disconnected);
}

async fn control_connection(
    ws: Ws,
    event_tx: EventSender,
    mut cmd_rx: tokio_mpsc::UnboundedReceiver<ClientMessage>,
) {
    let (mut sink, mut stream) = ws.split();
    tokio::select! {
        _ = async {
            while let Some(msg) = stream.next().await {
                match msg {
                    Ok(Message::Text(text)) => {
                        if let Ok(evt) = serde_json::from_str::<ServerMessage>(&text) {
                            event_tx.send(AppEvent::ServerMessage(evt));
                        }
                    }
                    Ok(Message::Close(_)) | Err(_) => break,
                    Ok(_) => {}
                }
            }
        } => {}
        _ = async {
            while let Some(cmd) = cmd_rx.recv().await {
                if let Ok(text) = serde_json::to_string(&cmd) {
                    if sink.send(Message::Text(text)).await.is_err() {
                        break;
                    }
                }
            }
        } => {}
    }
}

async fn preview_connection(ws: Ws, event_tx: EventSender) {
    let (_sink, mut stream) = ws.split();
    while let Some(msg) = stream.next().await {
        match msg {
            Ok(Message::Binary(bytes)) => {
                if let Ok(frames) = postcard::from_bytes::<Vec<PreviewFrame>>(&bytes) {
                    event_tx.send(AppEvent::PreviewFrameGroup(
                        frames
                            .into_iter()
                            .map(|pf| (pf.frame.into_color(), pf.zoom))
                            .collect(),
                    ));
                }
            }
            Ok(Message::Close(_)) | Err(_) => break,
            Ok(_) => {}
        }
    }
}