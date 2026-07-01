use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use acquisition_common::protocol::{ClientMessage, PreviewFrame, ServerMessage};
use acquisition_common::{Color, Frame};
use clap::Parser;
use eframe::egui;
use futures_util::{SinkExt, StreamExt};
use image::ImageFormat;
use tokio::sync::mpsc as tokio_mpsc;
use tokio::task::JoinHandle;
use tokio_tungstenite::tungstenite::Message;
use tracing::warn;

mod backoff;
mod gui;
mod numeric_input_widget;

use backoff::Backoff;
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
        viewport: egui::ViewportBuilder::default()
            .with_title("Phenominator")
            .with_app_id("phenominator-client")
            .with_icon(load_icon()),
        ..Default::default()
    };

    type WsHandles = (JoinHandle<()>, JoinHandle<()>);
    let ws_handles: Arc<Mutex<Option<WsHandles>>> = Arc::new(Mutex::new(None));
    let ws_handles_for_setup = ws_handles.clone();

    let result = eframe::run_native(
        "Phenominator",
        options,
        Box::new(move |cc| {
            // setup forwarding between internal channels and server,
            let event_tx = EventSender::new(event_tx, cc.egui_ctx.clone());
            let handles = rt_handle.block_on(start_ws_bridges(
                args.control_url,
                args.preview_url,
                event_tx,  // forward server events (WS) -> internal evens (event_tx)
                cmd_rx,    // forward internal commands (cmd_rx) to server (WS)
            ));
            *ws_handles_for_setup.lock().unwrap() = Some(handles);
            // `start_ws_bridges` above returns near instantly,
            // but at this point the app can start with
            // channel forwarding to the server enabled.
            Ok(Box::new(App::new(event_rx, cmd_tx)))  // TODO, make client ID unique (a string? hardware ID?)
        }),
    );

    // The connection tasks retry forever; they must be cancelled explicitly
    // rather than left to `shutdown_background`, which just drops the
    // runtime without waiting for tasks to unwind.
    if let Some((control_handle, preview_handle)) = ws_handles.lock().unwrap().take() {
        control_handle.abort();
        preview_handle.abort();
    }
    rt.shutdown_background();
    result
}

fn load_icon() -> egui::IconData {
    let image = image::load_from_memory(include_bytes!("../assets/icon.png"))
        .expect("embedded icon is a valid image")
        .into_rgba8();
    let (width, height) = image.dimensions();
    egui::IconData {
        rgba: image.into_raw(),
        width,
        height,
    }
}

async fn start_ws_bridges(
    control_url: String,
    preview_url: String,
    event_tx: EventSender,
    cmd_rx: tokio_mpsc::UnboundedReceiver<ClientMessage>,
) -> (JoinHandle<()>, JoinHandle<()>) {
    let control_handle = tokio::spawn(run_control_connection(control_url, event_tx.clone(), cmd_rx));
    let preview_handle = tokio::spawn(run_preview_connection(preview_url, event_tx));
    (control_handle, preview_handle)
}

type Ws = tokio_tungstenite::WebSocketStream<
    tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
>;

/// A TCP connection can be accepted by something that never completes the
/// WebSocket handshake (e.g. a stale port-forward with nothing real behind
/// it) — `connect_async` alone would then hang forever with no error and no
/// retry, freezing the reconnect countdown. Bound the whole attempt so a
/// hung handshake is treated the same as a refused connection.
const CONNECT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

async fn connect(url: &str) -> Result<Ws, String> {
    match tokio::time::timeout(CONNECT_TIMEOUT, tokio_tungstenite::connect_async(url)).await {
        Ok(Ok((ws, _))) => Ok(ws),
        Ok(Err(e)) => Err(e.to_string()),
        Err(_) => Err(format!("connect timed out after {CONNECT_TIMEOUT:?}")),
    }
}

/// Wait out a backoff delay, logging (unless the backoff has already
/// plateaued) and emitting a countdown event so the UI can show it.
async fn wait_and_backoff(
    label: &'static str,
    event_tx: &EventSender,
    backoff: &mut Backoff,
    failed: Option<&str>,
    reconnecting: impl Fn(u32, Instant) -> AppEvent,
) {
    if let Some(e) = failed {
        // Stop logging once the backoff has plateaued, so a long outage
        // doesn't spam the log forever.
        if !backoff.is_at_cap() {
            warn!(error = %e, label, "connection failed");
        }
    }

    let delay = backoff.current_delay();
    let next_at = Instant::now() + delay;
    event_tx.send(reconnecting(backoff.attempt_number(), next_at));
    backoff.record_failure();
    tokio::time::sleep(delay).await;
}

/// Repeatedly connect to `url` for the control channel, retrying with
/// exponential backoff forever on connect failure or disconnect. Never
/// returns; the caller must abort the task to stop it.
async fn run_control_connection(
    url: String,
    event_tx: EventSender,
    mut cmd_rx: tokio_mpsc::UnboundedReceiver<ClientMessage>,
) {
    let mut backoff = Backoff::new();
    loop {
        event_tx.send(AppEvent::ControlConnecting);
        match connect(&url).await {
            Ok(ws) => {
                backoff.reset();
                event_tx.send(AppEvent::ControlConnected);
                control_connection(ws, &event_tx, &mut cmd_rx).await;
                event_tx.send(AppEvent::ControlDisconnected);
                wait_and_backoff("control", &event_tx, &mut backoff, None, |attempt, next_at| {
                    AppEvent::ControlReconnecting { attempt, next_at }
                })
                .await;
            }
            Err(e) => {
                wait_and_backoff("control", &event_tx, &mut backoff, Some(&e), |attempt, next_at| {
                    AppEvent::ControlReconnecting { attempt, next_at }
                })
                .await;
            }
        }
    }
}

/// Repeatedly connect to `url` for the preview channel, retrying with
/// exponential backoff forever on connect failure or disconnect. Never
/// returns; the caller must abort the task to stop it.
async fn run_preview_connection(url: String, event_tx: EventSender) {
    let mut backoff = Backoff::new();
    loop {
        event_tx.send(AppEvent::PreviewConnecting);
        match connect(&url).await {
            Ok(ws) => {
                backoff.reset();
                event_tx.send(AppEvent::PreviewConnected);
                preview_connection(ws, &event_tx).await;
                event_tx.send(AppEvent::PreviewDisconnected);
                wait_and_backoff("preview", &event_tx, &mut backoff, None, |attempt, next_at| {
                    AppEvent::PreviewReconnecting { attempt, next_at }
                })
                .await;
            }
            Err(e) => {
                wait_and_backoff("preview", &event_tx, &mut backoff, Some(&e), |attempt, next_at| {
                    AppEvent::PreviewReconnecting { attempt, next_at }
                })
                .await;
            }
        }
    }
}

async fn control_connection(
    ws: Ws,
    event_tx: &EventSender,
    cmd_rx: &mut tokio_mpsc::UnboundedReceiver<ClientMessage>,
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

async fn preview_connection(ws: Ws, event_tx: &EventSender) {
    let (_sink, mut stream) = ws.split();
    while let Some(msg) = stream.next().await {
        match msg {
            Ok(Message::Binary(bytes)) => {
                if let Ok(frames) = postcard::from_bytes::<Vec<PreviewFrame>>(&bytes) {
                    let decoded = decode_jpeg_previews(frames);
                    if !decoded.is_empty() {
                        event_tx.send(AppEvent::PreviewFrameGroup(decoded));
                    }
                }
            }
            Ok(Message::Close(_)) | Err(_) => break,
            Ok(_) => {}
        }
    }
}

fn decode_jpeg_previews(
    frames: Vec<PreviewFrame>,
) -> Vec<(Frame<Color>, Option<acquisition_common::protocol::Zoom>)> {
    frames
        .into_iter()
        .filter_map(|pf| {
            let rgb = image::load_from_memory_with_format(&pf.jpeg, ImageFormat::Jpeg)
                .ok()?
                .into_rgb8();
            let frame = Frame::<Color>::new(
                rgb.into_raw(),
                pf.width,
                pf.height,
                pf.camera_timestamp_ns,
                pf.host_timestamp,
                pf.camera_name,
            );
            Some((frame, pf.zoom))
        })
        .collect()
}