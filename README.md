# Running the acquisition server on the NUC

```bash
cargo run --manifest-path app/Cargo.toml -p acquisition-server
# in case the pylon SDK is installed in a non-standard location,
# set LD_LIBRARY_PATH explicitly
LD_LIBRARY_PATH=/opt/pylon/lib cargo run --manifest-path app/Cargo.toml -p acquisition-server
```

## Logging

Logging is controlled via the `RUST_LOG` environment variable:

```bash
# info level and above (default if RUST_LOG is unset: no output)
RUST_LOG=info cargo run --manifest-path app/Cargo.toml -p acquisition-server

# debug adds per-poll PTP status for each camera; span close events show
# elapsed_milliseconds for Ptp4lGuard::new, wait_for_slave, wait_for_locked
RUST_LOG=debug cargo run --manifest-path app/Cargo.toml -p acquisition-server

# trace also includes ptp4l stdout and config feature calls
RUST_LOG=trace cargo run --manifest-path app/Cargo.toml -p acquisition-server

# enable everything except PTP
RUST_LOG=trace,pylon::ptp=off cargo run --manifest-path app/Cargo.toml -p acquisition-server
```

## Build dependencies (server)

The server requires `cmake` and `nasm` to build the bundled libjpeg-turbo (used for JPEG-compressed preview streaming):

```bash
sudo apt install cmake nasm
```

## On-camera debayering (not viable)

The a2A3536-9gcBAS has a 1 GigE interface (~125 MB/s) and a **square** sensor:
`SensorWidth` 3548 × `SensorHeight` 3552, with the default ROI at 3536×3536, so
a frame is 12.5 MP rather than the 8.7 MP a 4:3 reading of the model number
would suggest. At that size an RGB8 frame is ~37.5 MB, so 9 fps would require
~338 MB/s — roughly 2.7× the link capacity. Frames arrive incomplete and are
dropped by Pylon's stream grabber. **Debayering must be done on the host.**

BayerRG8 is 1 byte/pixel, so 12.5 MB/frame: fine at the 2-3 fps this rig runs
(~37.5 MB/s at 3 fps, confirmed by `BslDeviceLinkCurrentThroughput` reading
~37.9 MB/s), but ~113 MB/s at 9 fps, which is about 90% of the link and leaves
very little headroom.

Lowering the framerate to 2 fps via `AcquisitionFrameRate` does not help either —
frames are still incompletely grabbed. The bottleneck appears to be burst behaviour
in the camera or the GigE stream grabber rather than sustained link throughput.

The `--debayer` CLI flag and the associated `PixelFormat`/`DemosaicingMethod`/
`AcquisitionFrameRate` camera settings exist in the code but cannot be used reliably.

## Auto-tuning camera whitebalance/gain/exposure

Auto-tuning of camera whitebalance, gain and exposure runs automatically at
startup; there is no flag to request it. <br>
Here, PTP is disabled such that the cameras start up faster,
but auto-tuning is compatible with PTP syncing.

```bash
cargo run --manifest-path app/Cargo.toml -p acquisition-server -- --frames 5 --no-ptp

# with debugging
RUST_LOG=debug cargo run --manifest-path app/Cargo.toml -p acquisition-server -- --frames 5 --no-ptp
```

## Synchronized triggering

By default the cameras do not free-run: they expose on a shared trigger grid derived
from their PTP-disciplined clocks (Basler's Periodic Signal feature, no I/O cabling
needed). Measured cross-camera agreement on this rig is within ~1 µs of the frame
timestamps, against ~3 ms when free-running.

Two independent axes control this, each with its own opt-out:

|                          | PTP locked          | PTP enabled, not locked | `--no-ptp`         |
| ------------------------ | ------------------- | ----------------------- | ------------------ |
| default                  | period, **synced**  | period, unsynced        | period, unsynced   |
| `--no-sync-trigger`      | limiter, unsynced   | limiter, unsynced       | limiter, unsynced  |

- **`--no-sync-trigger`** free-runs the cameras on the `AcquisitionFrameRate` limiter
  (the pre-existing behaviour). The server falls back to this on its own if a camera
  does not support the periodic signal; the flag forces it, which is the only way to
  exercise that path on hardware that does support it.
- **`--no-ptp`** skips `ptp4l` and switches `PtpEnable` off on every camera. The
  trigger still paces both cameras at the requested rate, just off each camera's own
  free-running clock.

Which feature owns the frame rate depends only on the trigger axis, never on PTP:
losing PTP lock costs synchronization, never rate control.

Note the periodic signal quantizes the period to whole microseconds and caps it at
1 s, so **the minimum rate is 1 fps** (below that the request is clamped) and 3 fps
is realized as 3.000003 fps. The server reports the realized rate back to the client.
Under `--no-sync-trigger` the limiter's full 0.2 fps range is available again.

### Reading the PTP status

The client status bar shows, per camera, the PTP role, a servo lock icon, a `⇄`
sync indicator and the clock offset from the grand master. Captures are only
genuinely simultaneous when the trigger runs off the PTP clock **and** the servo is
locked — the signal source flips to `PtpClock` the moment PTP is enabled, tens of
seconds before the clock actually settles.

The offset matters because it, not the frame timestamps, bounds true simultaneity:
timestamps are measured on the very clocks that are off, so they look better aligned
than the captures really are. Note also that `PtpServoStatus == Locked` only promises
agreement within `BslPtpServoLockedThreshold`, which defaults to 50 µs.

# NUC to Laptop Direct Ethernet Connection (Point-to-Point)

This guide documents the steps to establish a direct, Point-to-Point (P2P)
Ethernet connection between a laptop and a NUC-type mini PC (hostname:
`phenominator`) for field debugging without an external network switch or
router.

## 1. What is a P2P Connection?

A Point-to-Point (P2P) Ethernet connection directly links two computing devices
using a standard Ethernet cable. Modern network interfaces support Auto-MDIX,
meaning standard straight-through patch cables can be used without requiring a
specialized crossover cable.

In this setup, DHCP is bypassed. Static IP addresses in the same subnet
(`192.168.99.x/24`) are assigned to both machines to enable direct
communication.

**Hardware Configuration:**
* **NUC Interface:** `enp2s0` (IP: 192.168.99.1)
* **Laptop Interface:** (example) `enp0s31f6` (IP: 192.168.99.2)

---

## 2. NUC Network Configuration

### Temporary Configuration (Lost on reboot)
To temporarily assign the IP and bring the interface up, use the `ip` utility:

```bash
sudo ip addr add 192.168.99.1/24 dev enp2s0
sudo ip link set dev enp2s0 up
```

### Permanent Configuration (Survives reboot)

To make the configuration persistent using NetworkManager, create a connection
profile via nmcli.

```Bash
# Create the static connection profile
sudo nmcli con add con-name enp2s0-direct type ethernet ifname enp2s0 ipv4.method manual ipv4.addresses 192.168.99.1/24

# Activate the connection
sudo nmcli con up enp2s0-direct
```

Expected Output Verification:

```Plaintext
samey@phenominator:~$ sudo nmcli con up enp2s0-direct
Connection successfully activated (D-Bus active path: /org/freedesktop/NetworkManager/ActiveConnection/15)

samey@phenominator:~$ ip a
...
3: enp2s0: <BROADCAST,MULTICAST,UP,LOWER_UP> mtu 1500 qdisc mq state UP group default qlen 1000
    link/ether a8:b8:e0:0a:ed:0d brd ff:ff:ff:ff:ff:ff
    inet 192.168.99.1/24 brd 192.168.99.255 scope global noprefixroute enp2s0
       valid_lft forever preferred_lft forever
...
```

## 3. Laptop Network Configuration

On the laptop running Linux, identify the physical Ethernet port (e.g. enp0s31f6).
Configure a static IP in the `192.168.99.x` subnet before connecting the physical
cable.

### Temporary (may not work for long if you have networkmanager installed)

```Bash
sudo ip addr add 192.168.99.2/24 dev enp0s31f6
sudo ip link set dev enp0s31f6 up
```

Expected Output Verification:

```Plaintext
samey @ ledono25 :2 > ip a
...
2: enp0s31f6: <NO-CARRIER,BROADCAST,MULTICAST,UP> mtu 9000 qdisc fq_codel state DOWN group default qlen 1000
    link/ether c0:47:0e:56:ef:55 brd ff:ff:ff:ff:ff:ff
...
```
(Note: NO-CARRIER and state DOWN are expected until the physical connection is made).

### If networkmanager (ubuntu) keeps resetting your network interface:

```bash
# Create a profile
sudo nmcli con add type ethernet con-name my-static-link ifname enp0s31f6 ip4 192.168.99.2/24
# Activate the profile
sudo nmcli con up my-static-link
```

## 4. Connection and Testing

1. Physical Connection: Plug the Ethernet cable directly into the NUC's enp2s0 port
and the laptop's enp0s31f6 port.

2. State Change: The interface states in ip a will transition from DOWN to UP.

3. Test with SSH: Open a terminal on the laptop and connect to the NUC.

```Bash
ssh samey@192.168.99.1
```