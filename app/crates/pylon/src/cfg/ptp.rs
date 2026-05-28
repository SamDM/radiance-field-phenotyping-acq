use std::ffi::CStr;
use std::net::Ipv4Addr;

use crate::pylon::Camera;

pub type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

#[derive(Debug)]
struct PtpError(String);

impl std::fmt::Display for PtpError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for PtpError {}

// ── PTP status types ──────────────────────────────────────────────────────────

#[derive(Debug, PartialEq, Eq)]
pub enum PtpStatus {
    Master,
    PreMaster,
    Slave,
    Faulty,
    Listening,
    Uncalibrated,
    Disabled,
}

impl PtpStatus {
    fn from_str(s: &str) -> Result<Self> {
        match s {
            "Master" => Ok(Self::Master),
            "PreMaster" => Ok(Self::PreMaster),
            "Slave" => Ok(Self::Slave),
            "Faulty" => Ok(Self::Faulty),
            "Listening" => Ok(Self::Listening),
            "Uncalibrated" => Ok(Self::Uncalibrated),
            "Disabled" => Ok(Self::Disabled),
            other => Err(PtpError(format!("unknown PtpStatus: {other}")).into()),
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum PtpServoStatus {
    Unlocked,
    Locked,
}

impl PtpServoStatus {
    fn from_str(s: &str) -> Result<Self> {
        match s {
            "Locked" => Ok(Self::Locked),
            // "Unknown" and anything else is considered unlocked
            _ => Ok(Self::Unlocked),
        }
    }
}

// ── Host interface discovery ──────────────────────────────────────────────────

struct GetifaddrsGuard(*mut libc::ifaddrs);

impl Drop for GetifaddrsGuard {
    fn drop(&mut self) {
        unsafe { libc::freeifaddrs(self.0) };
    }
}

/// Returns `(iface_name, ipv4_addr, prefix_len)` for all non-loopback IPv4 interfaces.
fn get_host_interfaces() -> Result<Vec<(String, Ipv4Addr, u8)>> {
    let mut ifap: *mut libc::ifaddrs = std::ptr::null_mut();
    if unsafe { libc::getifaddrs(&mut ifap) } != 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    let _guard = GetifaddrsGuard(ifap);

    let mut result = Vec::new();
    let mut cur = ifap;
    while !cur.is_null() {
        let ifa = unsafe { &*cur };
        cur = ifa.ifa_next;

        if ifa.ifa_addr.is_null() {
            continue;
        }
        let family = unsafe { (*ifa.ifa_addr).sa_family as i32 };
        if family != libc::AF_INET {
            continue;
        }
        if ifa.ifa_flags & (libc::IFF_LOOPBACK as u32) != 0 {
            continue;
        }
        if ifa.ifa_netmask.is_null() {
            continue;
        }

        let sin = unsafe { &*(ifa.ifa_addr as *const libc::sockaddr_in) };
        let addr = Ipv4Addr::from(u32::from_be(sin.sin_addr.s_addr));

        let nm = unsafe { &*(ifa.ifa_netmask as *const libc::sockaddr_in) };
        let mask_u32 = u32::from_be(nm.sin_addr.s_addr);
        let prefix_len = mask_u32.count_ones() as u8;

        let name = unsafe { CStr::from_ptr(ifa.ifa_name) }
            .to_string_lossy()
            .into_owned();

        result.push((name, addr, prefix_len));
    }
    Ok(result)
}

/// Returns the host network interface name that the camera is connected to.
pub fn get_camera_iface(cam: &Camera<'_>) -> Result<String> {
    let di = cam.device_info();
    let cam_ip: Ipv4Addr = di.ip_address()?.parse()?;
    let cam_mask: Ipv4Addr = di.subnet_mask()?.parse()?;
    let cam_mask_u32 = u32::from(cam_mask);

    for (name, host_addr, prefix_len) in get_host_interfaces()? {
        if prefix_len == 0 {
            continue;
        }
        let host_mask = !0u32 << (32 - prefix_len as u32);
        // Use the more restrictive of the two masks for the comparison
        let mask = host_mask & cam_mask_u32;
        if (u32::from(host_addr) & mask) == (u32::from(cam_ip) & mask) {
            return Ok(name);
        }
    }
    Err(PtpError(format!("no host interface found for camera {cam_ip}")).into())
}

// ── Camera PTP configuration ──────────────────────────────────────────────────

/// Configure PTP on a camera. Call this after `cameras.open()`.
/// `priority`: lower value = more likely to become PTP master (1 = highest priority).
pub fn enable_ptp(cam: &mut Camera<'_>, priority: u8) -> Result<()> {
    // Disable first; ignore errors (feature may already be false or temporarily read-only)
    cam.set_feature("PtpEnable", "false").ok();

    cam.set_feature("BslPtpPriority1", &priority.to_string())?;
    cam.set_feature("BslPtpProfile", "DelayRequestResponseDefaultProfile")?;
    cam.set_feature("BslPtpNetworkMode", "Multicast")?;
    cam.set_feature("BslPtpTwoStep", "false")?;
    cam.set_feature("BslPtpManagementEnable", "false")?;
    cam.set_feature("PtpEnable", "true")?;
    Ok(())
}

pub fn get_ptp_status(cam: &mut Camera<'_>) -> Result<PtpStatus> {
    cam.execute_command("PtpDataSetLatch")?;
    let s = cam.get_feature("PtpStatus")?;
    PtpStatus::from_str(&s)
}

pub fn get_servo_status(cam: &mut Camera<'_>) -> Result<PtpServoStatus> {
    cam.execute_command("PtpDataSetLatch")?;
    let s = cam.get_feature("PtpServoStatus")?;
    PtpServoStatus::from_str(&s)
}

/// Returns the deduplicated, sorted list of host interface names for a slice of cameras.
pub fn get_camera_interfaces(cameras: &[Camera<'_>]) -> Result<Vec<String>> {
    let mut ifaces: Vec<String> = cameras
        .iter()
        .map(|cam| get_camera_iface(cam))
        .collect::<Result<Vec<_>>>()?;
    ifaces.sort();
    ifaces.dedup();
    Ok(ifaces)
}

