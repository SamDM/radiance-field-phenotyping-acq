## sync-analysis: build the detrended-residual bound into `scan-recording`

Each camera's measured phase ramps steadily across a recording (a few hundred
us/frame) because the 3.000003 fps periodic-signal trigger sits almost exactly
on a 20:1 resonance with the phone's ~60 Hz panel — a tiny error in the
phone's *reported* Hz (60.00 vs 60.02 vs 60.03 between recordings) gets
amplified ~20x per trigger. That ramp swamps the raw cam0-vs-cam1 spread
`scan-recording` currently reports (267-622 us sd across three test
recordings) even though it isn't sync error at all: it's evidence the slope
differs in both sign and magnitude between recordings, which a real fixed
hardware clock-rate difference between the two cameras would not do.

Fitting and removing each camera's own linear phase trend before taking the
cross-camera difference isolates a much tighter, more defensible number —
64-70 us sd, consistent across all three recordings tested — that also
converges with the independent PTP-servo-based bound from earlier in this
project (`BslPtpServoLockedThreshold` / `PtpOffsetFromMaster`). Currently this
only exists as a one-off script outside the repo that regex-parses
`scan-recording -v`'s text output and recomputes the wrapped diff itself
rather than reusing `phase::wrapped_diff`.

**Fix:**
- Add the detrending as a proper stage in `main.rs`, alongside (not replacing)
  the existing raw spread — the raw number is still the honest "no assumptions"
  answer, the detrended one is the "assuming the beat-frequency ramp is a
  nuisance, here's the residual" answer, and a report should show both.
- Fit against each frame's actual `camera_timestamp_ns` (already loaded for
  the pairing-sanity check) rather than frame index. Frame index assumes a
  constant capture rate; the timestamp is the real elapsed time and removes
  that assumption.
- Phase unwrapping (`phase[i]` chosen as whichever representative is closest
  to the running unwrapped value) assumes true phase never moves more than
  half a cycle between two *successfully measured* frames. True here (small
  gaps, ~1-2% of a cycle drift per frame) but unverified in general — a long
  run of missed measurements could break it silently. Should either check the
  gap size before trusting the unwrap, or fall back to reporting only the raw
  (non-detrended) bound when it can't be verified.

## pylon crate: `CameraArray::get_camera` does not enforce mutability

`get_camera(&self, i) -> Camera<'_>` currently allows callers to mutate camera
hardware state (via `set_feature`, `execute_command`) even when holding a shared
`&CameraArray`. The mutation goes through a raw pointer inside `Camera<'_>`,
bypassing the borrow checker.

**Fix (Option A — collapses read/write distinction):**  
Change `get_camera` to take `&mut self`. One-line pylon change; all call sites
that currently pass `&CameraArray` and mutate must switch to `&mut CameraArray`.
Read-only sites (e.g. `get_camera_interfaces`) would also require `&mut`, which
is slightly noisy.

**Fix (Option B — full semantics):**  
Split into two methods and two types:
- `get_camera(&self, i) -> CameraRef<'_>` — exposes `get_feature`, `device_info` only  
- `get_camera_mut(&mut self, i) -> Camera<'_>` — exposes `set_feature`, `execute_command`, etc.

Update call sites: read-only uses → `get_camera`, mutable uses → `get_camera_mut`.
More churn but enforces the intended semantics at the type level.
