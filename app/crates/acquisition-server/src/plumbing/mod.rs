use std::future::Future;
use std::net::SocketAddr;
use std::time::Duration;

use acquisition_common::protocol::{ClientMessage, PreviewFrame, ServerMessage};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{broadcast, mpsc};
use tokio::task::JoinHandle;
use tracing::{info, warn};

use crate::Args;

mod control;
mod preview;
mod ws;

/// This function has a 3 phases:
/// Phase 1: define channels for internal communication between threads,
///          which are passed to the acquisition loop
/// Phase 2: define forwarding loops to forward internal messages
///          from/to websockets (client communicatoin)
/// Phase 3: teardown the server (after receiving CTRL-C / SIGINT).
pub async fn start_server(args: Args) -> Result<(), Box<dyn std::error::Error>> {
    let control_addr = args.control_addr;  // copy SocketAddr (which are `Copy`)
    let preview_addr = args.preview_addr;  // for later use
    let event_broadcast_capacity = args.event_broadcast_capacity;
    let preview_broadcast_capacity = args.preview_broadcast_capacity;
    let heartbeat_interval = Duration::from_secs(args.heartbeat_interval_secs);
    let send_timeout = Duration::from_secs(args.send_timeout_secs);

    // -------
    // Phase 1

    // cmd_tx: cloned into each control::handle task; each connected control client
    //         sends ClientMessage commands (start, stop, …) through this sender.
    //         I.e. message coming from client WS connections are forwarded onto
    //         this end of the channel.
    // cmd_rx: consumed by acquisition::run; drives the acquisition state machine.
    //         I.e. message dumped from a client WS connection onto cmd_tx end
    //         are read at the cmd_rx end by the acquistion loop and executed.
    let (cmd_tx, cmd_rx) = mpsc::unbounded_channel::<ClientMessage>();

    // evt_bcast: cloned into acquisition::run, which publishes ServerMessage status
    //            events (recording started/stopped, errors, …) as they occur.
    // _:         each control::handle task holds its own subscription (created via
    //            evt_bcast.subscribe() — broadcast::Receiver is not Clone) and
    //            forwards events to its connected client over the WebSocket.
    let (evt_bcast, _) = broadcast::channel::<ServerMessage>(event_broadcast_capacity);

    // preview_bcast: cloned into acquisition::run, which publishes an AnyFrame
    //                for every captured frame so all preview clients stay in sync.
    // _:             each preview::handle task holds its own subscription (created via
    //                preview_bcast.subscribe() — broadcast::Receiver is not Clone) and
    //                streams frames to its connected client over the WebSocket.
    let (preview_bcast, _) = broadcast::channel::<Vec<PreviewFrame>>(preview_broadcast_capacity);

    // start the acquisition loop.
    // The loop has access to:
    // `cmd_rx`: to receive commands from clients (which were forwarded from a WS to `cmd_tx`).
    // `evt_tx`: to send state events to clients (which will be forwarded from `evt_rx` to a WS)
    // `preview_tx`: to send camera frames to clients (which will be forwarded from `peview_rx` to a WS)
    let acq_handle = {
        let evt_tx = evt_bcast.clone();
        let preview_tx = preview_bcast.clone();
        std::thread::spawn(move || crate::grabbing_loop::run(args, cmd_rx, evt_tx, preview_tx).map_err(|e| e.to_string()))
    };

    // -------
    // Phase 2

    // set up a TCP accept loops and per-connection handlers that acts as a bridge:
    // - control (bidirectional):
    //   forwards incoming WebSocket messages from the client onto cmd_tx,
    //   and forwards outgoing evt_rx messages back to the client over WebSocket.
    // - preview (unidirectional):
    //   forwards preview_rx frames out to the client over WebSocket.
    //
    // The two loops together are the async ↔ WebSocket boundary for all three channels
    let (control_accept, preview_accept) = start_ws_bridges(
        control_addr,
        preview_addr,
        cmd_tx.clone(),
        evt_bcast.clone(),      // note: `evt_rx` is obtained from `evt_bcast.subscribe()`
        preview_bcast.clone(),  // note: `preview_rx` is obtained from `preview_bcast.subscribe()`
        heartbeat_interval,
        send_timeout,
    )
    .await?;

    // -------
    // Phase 3

    // Wait for server teardown, all code below (in this function) is server teardown.
    tokio::signal::ctrl_c().await?;
    info!("ctrl-c received, shutting down");

    // Drop the command sender so the acquisition loop sees Disconnected and exits.
    drop(cmd_tx);
    // Stop listening for clients, and stop existing client connections
    // TODO: is abort too brutal here?
    control_accept.abort();
    preview_accept.abort();

    // Wait for the acquistion loop to end on its own.
    // It is expected to stop due to the disconnected control channel.
    let join_result = tokio::task::spawn_blocking(move || acq_handle.join()).await?;
    match join_result {
        Ok(Ok(())) => Ok(()),
        Ok(Err(msg)) => Err(msg.into()),
        Err(_) => Err("acquisition thread panicked".into()),
    }
}

// Binds the control and preview TCP listeners and starts their accept loops.
// Each accepted connection gets a fresh channel subscription so it receives
// only messages that arrive after it connects.
// Returns (control_accept, preview_accept) join handles.
async fn start_ws_bridges(
    control_addr: SocketAddr,
    preview_addr: SocketAddr,
    cmd_tx: mpsc::UnboundedSender<ClientMessage>,
    evt_bcast: broadcast::Sender<ServerMessage>,
    preview_bcast: broadcast::Sender<Vec<PreviewFrame>>,
    heartbeat_interval: Duration,
    send_timeout: Duration,
) -> Result<(JoinHandle<()>, JoinHandle<()>), Box<dyn std::error::Error>> {
    let control_listener = TcpListener::bind(control_addr).await?;
    info!(%control_addr, "control socket listening");
    let preview_listener = TcpListener::bind(preview_addr).await?;
    info!(%preview_addr, "preview socket listening");

    let control_accept = spawn_accept_loop("control", control_listener, move |stream| {
        let cmd_tx = cmd_tx.clone();
        let evt_rx = evt_bcast.subscribe();
        control::handle(stream, cmd_tx, evt_rx, heartbeat_interval, send_timeout)
    });

    let preview_accept = spawn_accept_loop("preview", preview_listener, move |stream| {
        let preview_rx = preview_bcast.subscribe();
        preview::handle(stream, preview_rx, heartbeat_interval, send_timeout)
    });

    Ok((control_accept, preview_accept))
}

// Start an infinity loop, waiting for client connections.
// The `make_handler` is a `Clone`able function that accepts a TCP stream, and does whatever it wants with it.
// This `handler` function is executed once per new client connection.
fn spawn_accept_loop<F, Fut>(name: &'static str, listener: TcpListener, make_handler: F) -> JoinHandle<()>
where
    F: Fn(TcpStream) -> Fut + Send + 'static + Clone,
    Fut: Future<Output = Result<(), Box<dyn std::error::Error + Send + Sync>>> + Send + 'static,
{
    tokio::spawn(async move {
        loop {
            match listener.accept().await {
                Ok((stream, peer)) => {
                    info!(%peer, "{name} client connected");
                    let handler = make_handler.clone();
                    tokio::spawn(async move {
                        if let Err(e) = handler(stream).await {
                            warn!(%peer, error = %e, "{name} client error");
                        } else {
                            info!(%peer, "{name} client disconnected");
                        }
                    });
                }
                Err(e) => warn!(error = %e, "{name} accept failed"),
            }
        }
    })
}
