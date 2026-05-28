use std::collections::HashSet;
use std::io::{BufRead, BufReader, Read};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};

use tracing::{debug, error, info, trace};

pub type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

#[derive(Debug)]
struct Ptp4lError(String);

impl std::fmt::Display for Ptp4lError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for Ptp4lError {}

/// Internal `ptp4l` state, derived by parsing its stdout/stderr.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Ptp4lStatus {
    /// Process spawned (or already running) but grand-master role not yet
    /// confirmed on every required interface.
    Starting,
    /// `ptp4l` is master on all required interfaces.
    Master,
    /// Spawn or runtime error; PTP is not functional.
    Failed(String),
}

/// RAII guard for a non-blocking `ptp4l` process. Spawns immediately and
/// returns; callers poll [`Ptp4lGuard::status`] for progress. The process is
/// killed on drop.
///
/// If `ptp4l` is already running for all required interfaces the guard is a
/// no-op (no child process) and reports `Master` straight away.
///
/// `ptp4l` requires Linux capabilities to bind to privileged network ports.
/// Set these once with:
///   `sudo setcap cap_net_bind_service,cap_sys_time=ep /usr/sbin/ptp4l`
/// After this, `ptp4l` can run without sudo.
///
/// To revert (remove all file capabilities and restore pre-setcap state):
///   `sudo setcap -r /usr/sbin/ptp4l`
/// Verify the capabilities at any time with:
///   `getcap /usr/sbin/ptp4l`
pub struct Ptp4lGuard {
    child: Option<Child>,
    readers: Vec<JoinHandle<()>>,
    status: Arc<Mutex<Ptp4lStatus>>,
}

const CAPABILITY_HINT: &str =
    "ptp4l requires Linux capabilities. Run: \n\
     sudo setcap cap_net_bind_service,cap_sys_time=ep /usr/sbin/ptp4l \n\
     See Ptp4lGuard documentation for details.";

impl Ptp4lGuard {
    /// Spawn (or attach to an already-running) `ptp4l` for `ifaces`. Returns
    /// as soon as the child process is launched and the reader threads are
    /// running. The grand-master role is reported asynchronously via
    /// [`Ptp4lGuard::status`].
    pub fn spawn(ifaces: &[String]) -> Result<Self> {
        let mut ifaces = ifaces.to_vec();
        ifaces.sort();
        ifaces.dedup();

        if ifaces.is_empty() {
            return Err(Ptp4lError("no interfaces provided".to_string()).into());
        }

        let status = Arc::new(Mutex::new(Ptp4lStatus::Starting));

        // If ptp4l is already running for all required interfaces, return a
        // no-op guard reporting Master immediately.
        if ptp4l_covers(&ifaces) {
            info!("ptp4l already running, guard is no-op");
            *status.lock().unwrap() = Ptp4lStatus::Master;  // TODO: not always true
            return Ok(Self {
                child: None,
                readers: Vec::new(),
                status,
            });
        }

        let mut cmd = Command::new("ptp4l");
        cmd.args(["-m", "-l", "7", "-S", "--priority1=1",
                   "--logSyncInterval=-1", "--logAnnounceInterval=1",
                   "--uds_address=/tmp/ptp4l", "--uds_ro_address=/tmp/ptp4lro"]);
        for iface in &ifaces {
            cmd.args(["-i", iface.as_str()]);
        }
        cmd.stdout(Stdio::piped()).stderr(Stdio::piped());

        let cmd_str = std::iter::once("ptp4l")
            .chain(cmd.get_args().map(|a| a.to_str().unwrap_or("?")))
            .collect::<Vec<_>>()
            .join(" ");
        debug!("spawning: {cmd_str}");

        let mut child = cmd.spawn().map_err(|e| {
            if e.kind() == std::io::ErrorKind::PermissionDenied {
                Ptp4lError(format!("permission denied spawning ptp4l.\n{CAPABILITY_HINT}"))
            } else {
                Ptp4lError(format!("failed to spawn ptp4l: {e}"))
            }
        })?;
        info!("ptp4l spawned");

        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| Ptp4lError("ptp4l stdout not captured".to_string()))?;
        let stderr = child
            .stderr
            .take()
            .ok_or_else(|| Ptp4lError("ptp4l stderr not captured".to_string()))?;

        // Shared progress state between the two reader threads: the set of
        // interfaces that have assumed the grand master role. When every
        // expected iface is in this set, status flips to Master.
        let confirmed: Arc<Mutex<HashSet<String>>> =
            Arc::new(Mutex::new(HashSet::new()));
        let expected: HashSet<String> = ifaces.iter().cloned().collect();

        let reader_out = {
            let status = Arc::clone(&status);
            let confirmed = Arc::clone(&confirmed);
            let expected = expected.clone();
            thread::spawn(move || parse_lines(stdout, false, status, confirmed, expected))
        };
        let reader_err = {
            let status = Arc::clone(&status);
            let confirmed = Arc::clone(&confirmed);
            let expected = expected.clone();
            thread::spawn(move || parse_lines(stderr, true, status, confirmed, expected))
        };

        Ok(Self {
            child: Some(child),
            readers: vec![reader_out, reader_err],
            status,
        })
    }

    /// Snapshot the current `ptp4l` state.
    pub fn status(&self) -> Ptp4lStatus {
        self.status.lock().unwrap().clone()
    }
}

impl Drop for Ptp4lGuard {
    fn drop(&mut self) {
        if let Some(mut child) = self.child.take() {
            let _ = child.kill(); // closing stdio is the single stop signal
            let _ = child.wait();
        }
        for t in self.readers.drain(..) {
            let _ = t.join();
        }
    }
}

fn parse_lines<R: Read>(
    src: R,
    is_stderr: bool,
    status: Arc<Mutex<Ptp4lStatus>>,
    confirmed: Arc<Mutex<HashSet<String>>>,
    expected: HashSet<String>,
) {
    let reader = BufReader::new(src);
    for line in reader.lines().map_while(|r| r.ok()) {
        if is_stderr {
            error!("[ptp4l stderr] {line}");
        } else {
            trace!("[ptp4l stdout] {line}");
        }

        if let Some(iface) = extract_iface_from_gm_line(&line) {
            if expected.contains(iface) {
                let mut c = confirmed.lock().unwrap();
                c.insert(iface.to_string());
                if c.len() == expected.len() {  // TODO: full set comparison?
                    let mut s = status.lock().unwrap();
                    if !matches!(*s, Ptp4lStatus::Failed(_)) {
                        *s = Ptp4lStatus::Master;
                        info!("ptp4l assumed grand master role on all interfaces");
                    }
                }
            }
        } else if is_fatal_line(&line) {
            let mut s = status.lock().unwrap();
            if !matches!(*s, Ptp4lStatus::Failed(_)) {
                *s = Ptp4lStatus::Failed(format!("ptp4l error: {line}"));
            }
        }
    }
}

fn is_fatal_line(line: &str) -> bool {
    [
        "Permission denied",
        "bind failed",
        "failed to create",
        "failed to open",
        "failed to set",
    ]
    .iter()
    .any(|needle| line.contains(needle))
}

/// Returns true if a running ptp4l process covers all of the given interfaces.
fn ptp4l_covers(ifaces: &[String]) -> bool {
    let Ok(output) = Command::new("pgrep").args(["-a", "ptp4l"]).output() else {
        return false;
    };
    let text = String::from_utf8_lossy(&output.stdout);
    ifaces.iter().all(|iface| {
        text.lines()
            .any(|line| line.contains(&format!("-i {iface}")) || line.contains(&format!(" {iface} ")))
    })
}

fn extract_iface_from_gm_line(line: &str) -> Option<&str> {
    // Pattern: "ptp4l[...] port N (iface_name): assuming the grand master role"
    if !line.contains("assuming the grand master role") {
        return None;
    }
    let start = line.find('(')? + 1;
    let end = start + line[start..].find(')')?;
    Some(&line[start..end])
}
