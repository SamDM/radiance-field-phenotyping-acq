use acquisition_common::protocol::{self as wire, PtpStatusReport};
use std::time::{Duration, Instant};

use pylon::cfg::RateControl;
use pylon::CameraArray;
use tokio::sync::broadcast;
use tracing::{debug, info, warn};

use crate::utils::ptp4l::{Ptp4lGuard, Ptp4lStatus};

enum Ptp4lIterState {
    Unitialized,
    Initialized(Option<Ptp4lGuard>),
    /// PTP switched off via `--no-ptp`. Distinct from `Initialized(None)`, which means
    /// PTP was wanted but failed.
    OptedOut,
}

impl Ptp4lIterState {
    fn get_ptp4l(&self) -> &Option<Ptp4lGuard> {
        match self {
            Ptp4lIterState::Unitialized | Ptp4lIterState::OptedOut => &None,
            Ptp4lIterState::Initialized(maybe_ptp4l) => &maybe_ptp4l,
        }
    }
}

pub(super) struct PtpIterState {
    poll_interval: Duration,
    no_ptp: bool,
    rate_control: RateControl,
    ptp4l: Ptp4lIterState,
    last_ptp_result: (Instant, wire::PtpStatusReport),
}

impl PtpIterState {
    pub fn new(
        poll_intervall_secs: u64,
        no_ptp: bool,
        rate_control: RateControl,
    ) -> PtpIterState {
        PtpIterState {
            poll_interval: Duration::from_secs(poll_intervall_secs),
            no_ptp,
            rate_control,
            ptp4l: Ptp4lIterState::Unitialized,
            last_ptp_result: (Instant::now(), wire::PtpStatusReport {
                ptp4l: wire::Ptp4lStatus::Starting,
                cameras: vec![],
            }),
        }
    }

    pub(super) fn ptp_report(&self) -> &wire::PtpStatusReport {
        &self.last_ptp_result.1
    }
}

pub(super) fn iteration(
    camera_array: &CameraArray,
    state: &mut PtpIterState,
    evt_tx: &broadcast::Sender<wire::ServerMessage>,
) {
    match &state.ptp4l {
        // Opted out with --no-ptp: switch PTP off on every camera once, then idle.
        // PtpEnable persists across process restarts, so it must be written off
        // explicitly rather than merely left alone.
        Ptp4lIterState::Unitialized if state.no_ptp => {
            for i in 0..camera_array.len() {
                let mut cam = camera_array.get_camera(i);
                if let Err(e) = pylon::cfg::disable_ptp(&mut cam) {
                    warn!(camera = i, error = %e, "failed to disable PTP");
                }
            }
            info!("PTP disabled (--no-ptp): camera clocks free-run");
            state.ptp4l = Ptp4lIterState::OptedOut;
            state.last_ptp_result = (
                Instant::now(),
                poll_ptp_report(None, camera_array, state.rate_control, true),
            );
        }
        // Start PTP4l if not done so already
        Ptp4lIterState::Unitialized => {
            let result = (|| {
                for i in 0..camera_array.len() {
                    let mut cam = camera_array.get_camera(i);
                    pylon::cfg::enable_ptp(&mut cam, 128)?;
                }
                spawn_ptp4l_guard(camera_array)
            })();
            match result {
                Ok(ptp4l) => {
                    state.ptp4l = Ptp4lIterState::Initialized(Some(ptp4l));
                }
                Err(e) => {
                    warn_ptp_failed(&e.to_string(), evt_tx);
                    state.ptp4l = Ptp4lIterState::Initialized(None);
                }
            }
            state.last_ptp_result = (
                Instant::now(),
                poll_ptp_report(
                    state.ptp4l.get_ptp4l().as_ref(),
                    camera_array,
                    state.rate_control,
                    false,
                ),
            );
        }
        // PTP was never wanted; nothing to poll.
        Ptp4lIterState::OptedOut => (),
        // If PTP4l crashed in the past, do nothing
        Ptp4lIterState::Initialized(None) => (),
        // If PTP4l is running, poll report (with rate limiting)
        Ptp4lIterState::Initialized(Some(ptp4l_guard)) => {
            state.last_ptp_result = poll_ptp_report_rate_limited(
                state.last_ptp_result.clone(),
                state.poll_interval,
                camera_array,
                state.rate_control,
                ptp4l_guard,
            );
            if let wire::Ptp4lStatus::Failed(e) = &state.last_ptp_result.1.ptp4l {
                warn_ptp_failed(e, evt_tx);
                state.ptp4l = Ptp4lIterState::Initialized(None)
                // ^ drop the guard, process is defunct anyway
                // fail msg already captured in ptp_state.last_ptp_result above
            }
        }
    }
}

fn warn_ptp_failed(e: &str, evt_tx: &broadcast::Sender<wire::ServerMessage>) {
    let msg = "PTP initialization failed, continueing without PTP";
    warn!(e, msg);
    let _ = evt_tx.send(wire::ServerMessage::notice(wire::NoticeSeverity::Warn, format!("{msg}: {e}")));
}

fn spawn_ptp4l_guard(camera_array: &CameraArray) -> Result<Ptp4lGuard, Box<dyn std::error::Error>> {
    let n = camera_array.len();
    let cams: Vec<_> = (0..n).map(|i| camera_array.get_camera(i)).collect();
    let ifaces = pylon::cfg::get_camera_interfaces(&cams)?;
    Ptp4lGuard::spawn(&ifaces)
}

fn poll_ptp_report(
    guard: Option<&Ptp4lGuard>,
    cameras: &CameraArray,
    rate_control: RateControl,
    opted_out: bool,
) -> wire::PtpStatusReport {
    let ptp4l = match (guard, opted_out) {
        (Some(g), _) => wire_ptp4l(g.status()),
        (None, true) => wire::Ptp4lStatus::Disabled,
        (None, false) => wire::Ptp4lStatus::Failed("ptp4l not started".to_string()),
    };

    let n = cameras.len();
    let mut cams = Vec::with_capacity(n);
    for i in 0..n {
        let mut cam = cameras.get_camera(i);
        // Only meaningful when the periodic signal is what paces acquisition; under
        // the free-run fallback the signal exists but drives nothing.
        let signal_source = match rate_control {
            RateControl::PeriodicSignal => match pylon::cfg::signal_source(&cam) {
                Ok(s) => Some(wire_signal_source(s)),
                Err(e) => {
                    warn!(camera = i, error = %e, "failed to read BslPeriodicSignalSource");
                    None
                }
            },
            RateControl::AcquisitionLimiter => None,
        };
        // With PTP off there is no dataset to latch: PtpDataSetLatch and the status
        // nodes are unavailable, and asking for them anyway throws.
        if opted_out {
            cams.push(wire::CameraPtpInfo {
                ptp: wire::CameraPtpStatus::Disabled,
                servo: wire::CameraServoStatus::Unlocked,
                signal_source,
                offset_from_master_ns: None,
            });
            continue;
        }
        let (ptp, servo, offset_from_master_ns) = match pylon::cfg::latch_ptp_data_set(&mut cam) {
            Ok(latch) => {
                let ptp = match latch.ptp_status() {
                    Ok(s) => wire_ptp(s),
                    Err(e) => {
                        warn!(camera = i, error = %e, "failed to read PtpStatus");
                        wire::CameraPtpStatus::Disabled
                    }
                };
                let servo = match latch.servo_status() {
                    Ok(s) => wire_servo(s),
                    Err(e) => {
                        warn!(camera = i, error = %e, "failed to read PtpServoStatus");
                        wire::CameraServoStatus::Unlocked
                    }
                };
                // Unreadable while PTP is off, which is expected rather than an error.
                let offset = latch.offset_from_master_ns().ok();
                (ptp, servo, offset)
            }
            Err(e) => {
                warn!(camera = i, error = %e, "failed to latch PtpDataSet");
                (wire::CameraPtpStatus::Disabled, wire::CameraServoStatus::Unlocked, None)
            }
        };
        cams.push(wire::CameraPtpInfo {
            ptp,
            servo,
            signal_source,
            offset_from_master_ns,
        });
    }

    wire::PtpStatusReport {
        ptp4l,
        cameras: cams,
    }
}

fn poll_ptp_report_rate_limited(
    last_ptp_result: (Instant, PtpStatusReport),
    ptp_poll_interval: Duration,
    camera_array: &CameraArray,
    rate_control: RateControl,
    ptp_guard: &Ptp4lGuard,
) -> (Instant, PtpStatusReport) {
    let (last_ptp_poll, last_ptp_report) = last_ptp_result;
    if last_ptp_poll.elapsed() < ptp_poll_interval {
        return (last_ptp_poll, last_ptp_report);
    }
    let ptp_report = poll_ptp_report(Some(ptp_guard), camera_array, rate_control, false);
    if ptp_report != last_ptp_report {
        debug!(ptp4l = ?ptp_report.ptp4l, cameras = ?ptp_report.cameras, "PTP status changed");
    }
    (Instant::now(), ptp_report)
}

fn wire_ptp4l(s: Ptp4lStatus) -> wire::Ptp4lStatus {
    match s {
        Ptp4lStatus::Starting => wire::Ptp4lStatus::Starting,
        Ptp4lStatus::Master => wire::Ptp4lStatus::Master,
        Ptp4lStatus::Failed(msg) => wire::Ptp4lStatus::Failed(msg),
    }
}

fn wire_ptp(s: pylon::cfg::PtpStatus) -> wire::CameraPtpStatus {
    match s {
        pylon::cfg::PtpStatus::Master => wire::CameraPtpStatus::Master,
        pylon::cfg::PtpStatus::PreMaster => wire::CameraPtpStatus::PreMaster,
        pylon::cfg::PtpStatus::Slave => wire::CameraPtpStatus::Slave,
        pylon::cfg::PtpStatus::Faulty => wire::CameraPtpStatus::Faulty,
        pylon::cfg::PtpStatus::Listening => wire::CameraPtpStatus::Listening,
        pylon::cfg::PtpStatus::Uncalibrated => wire::CameraPtpStatus::Uncalibrated,
        pylon::cfg::PtpStatus::Disabled => wire::CameraPtpStatus::Disabled,
    }
}

fn wire_signal_source(s: pylon::cfg::PeriodicSignalSource) -> wire::PeriodicSignalSource {
    match s {
        pylon::cfg::PeriodicSignalSource::PtpClock => wire::PeriodicSignalSource::PtpClock,
        pylon::cfg::PeriodicSignalSource::DeviceClock => wire::PeriodicSignalSource::DeviceClock,
    }
}

fn wire_servo(s: pylon::cfg::PtpServoStatus) -> wire::CameraServoStatus {
    match s {
        pylon::cfg::PtpServoStatus::Locked => wire::CameraServoStatus::Locked,
        pylon::cfg::PtpServoStatus::Unlocked => wire::CameraServoStatus::Unlocked,
    }
}
