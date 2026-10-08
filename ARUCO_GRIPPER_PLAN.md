# Aruco auto-record on the watering machine: findings and plan

Temporary working note (2026-10-07, updated 2026-10-08). Delete once the plan below is implemented or
dropped. Steps 0, A, B and C are implemented; D is not.

## The setting

Every day the machine picks each pot in turn, rotates it in front of the stereo pair
while watering it, and puts it back. Each pot should become one recording.

- Every pot carries the **same** ring of 8 ArUco markers, `DICT_6X6_50` ids 1–8, on
  the pot's underside. In the gripper the ring faces the cameras, so rotating the pot
  turns the ring within the image plane.
- Each pot also has a QR code that identifies it.
- The markers are there for calibration and 3D reconstruction. The plant deforms
  (wobble, watering), which is why the rig is stereo: each time-matched pair stands on
  its own.

## Problems

1. **Pots that aren't being handled trigger recordings.** Pots in the rack are in view
   at the top of the frame and carry the same marker ids. Only the pot in the gripper
   should start or sustain a recording.
2. **Duplicate frames.** If the machine stalls or runs slowly, a recording fills with
   frames that add no information.

## Findings

Measured with scratch tools: the production downscale and detector run over recorded
raw Bayer PNGs.

- **The far pot is detected reliably.** In `~/phenominator/2026-10-07/16-16-13`, with
  production settings (k=3, threshold window 13, error correction 0.2), it is detected
  on both cameras in every frame:

  | cam | id | centre (rel x, y) | side (full-res px) |
  |-----|----|-------------------|--------------------|
  | 0   | 1  | 0.54, 0.08        | ~125               |
  | 0   | 8  | 0.64, 0.09        | ~140               |
  | 1   | 1  | 0.74, 0.06        | ~112               |
  | 1   | 8  | 0.81, 0.07        | ~128               |

  So with the right family, the far pot would start a recording and keep it alive
  forever, because any marker that stays in view keeps a recording running.
- **No false positives.** `DICT_6X6_50` on all 792 frames of the 2026-09-22 recordings
  (QR codes and a test card in view): 0 detections. The only false positives ever seen
  came from `DICT_4X4` with loose detector settings, on the perforated panel.
- **Detection cost** on real frames is 15–20 ms at k=3, against ~5 ms on synthetic
  scenes. Still fine at the 200 ms interval.
- **Measuring frame change.** Thumbnail: green subsample at k=3, then a 4×4 box average
  (about 147×147). Metric: the share of pixels where |Δ − median Δ| > 16. Subtracting
  the median Δ cancels global brightness shifts.
  - Static scene, gain 10: mean |Δ| ≈ 2.3, p99 = 8, share above 16 = **0%**.
  - Moving (2026-09-22): adjacent frames 0.07–0.4% above 32; 5–10 frames apart, 9–30%.

## Plan

Order of work: 0 (arucotools update), then A, B and C. D is small and independent.
Reading the pot's QR code is out of scope for this branch.

### 0. Update arucotools to main first — done (2026-10-08)

Pinned to 4a57f47. `aruco/markers.rs` no longer merges codewords into a synthetic
family. It passes each configured family whole to one `detect_markers` call and
matches detections on the codeword, so overlapping dictionary names still resolve to
the configured name.

Checked:
- The tests pass, with and without the feature.
- `aruco_bench` is unchanged: about 5 ms per detection at k=3, same size thresholds.
- A replay of the recordings through the real `AutoRecordWatcher` (ids 1–8):
  - The 792 frames from 2026-09-22 never start a recording.
  - 17-06-39 starts at frame 3 and stops at frame 56 (markers in frames 2–53).
  - 17-08-12 starts at frame 2 and stops at frame 18 (markers in frames 1–15).
  - 16-16-13 starts on the rack pot, as expected until A lands.

### A. Marker gate: count a marker only if it is in the right place — done (2026-10-08)

Implemented as `MarkerGate` in `aruco/cfg.rs`, applied in `worker::detect`, with
`--aruco-roi` and `--aruco-min-side-px`. Each configured marker is logged at debug level
("aruco marker": camera, position, side, confidence, admitted). `launch-server.py`
enables that logging.

Checked by replaying recordings through the real watcher at the rig's timing (1 s per
pair, cam1 about 8 ms after cam0), with ROI `0.4,0.15,1.0,0.9`:
- The rack pot (16-16-13, looped for 30 s) never starts a recording. Without the ROI,
  it starts one and holds it open.
- 17-06-39 starts at frame 5 and stops at 60; 17-08-12 starts at 4 and stops at 21.
- 2026-09-22 never starts a recording.

The ROI went into `launch-server.py` wider in x than first proposed below. y is what
separates the rack from the gripper, and a loose x keeps the planned camera adjustment
from pushing the pot out. Original notes:

- Add `--aruco-roi x0,y0,x1,y1`: the rectangle the marker's centre must fall in, as
  fractions of the frame. **This is the primary test.** Held markers sit at y ≥ 0.20
  and rack pots at y ≤ 0.09 (see the measurements).
- Add `--aruco-min-side-px N`, optional and off by default: the minimum mean side
  length, in full-resolution pixels. The size margin is only 4 px (held markers from
  149 px, rack up to 145 px), so it can only back up the ROI.
- Filter in the detector thread, before the trigger, so a gated-out marker neither
  starts nor sustains a recording.
- Log every marker's geometry and whether it passed, at debug level, for tuning.
- Starting values for the current camera placement: ROI `0.6,0.15,1.0,0.8`, which
  covers the measured spread with margin and excludes the rack. Re-measure after the
  cameras are adjusted.
  - The ring turns within the image plane, so held markers keep their size as the
    pot turns, and their centres trace a circle around the gripper axis (cam0 ≈ 0.85,
    0.48). The ROI must cover that whole circle, not just the partial turn recorded so
    far.
  - The first and last frame of a pot (in transit, y 0.06–0.27) are partly cut off by
    the ROI. That's fine: the pot is moving, not being imaged.
- A draft existed but was not applied, and is gone with the session scratchpad. It
  added `MarkerGate { roi, min_side_px }` to `AutoRecordCfg`, and passed it plus the
  scale `2*downscale` to `worker::run`/`detect`.
- Don't use the size-only stopgap `--aruco-min-perimeter-rate ≈ 0.25` (cut-off about
  220 px at k=3): it would also reject held markers, which start at 149 px.

### B. Skip frame groups that haven't changed since the last saved group — done (2026-10-08)

Implemented in `src/change.rs` (`ChangeGate`), applied in `process_grouped_frames_result`,
enabled with `--aruco-min-change-pct 0.5` in `launch-server.py`.
- Departure from the notes below: it applies **only to auto-started recordings**, so the
  flag can stay on in `launch-server.py` without thinning out manual sync tests or
  calibration runs.
- Skipped groups take no frame number, so the files stay contiguous and verification is
  unchanged.
- A finished auto-recording logs `groups_saved` and `groups_skipped`; each group's score
  is logged at debug level ("frame group change").
- Thumbnail cost measured at 1.0 ms per camera.

On the recordings, at 0.5%:
- 17-06-39 keeps 12 of 52 held frames: 2, 3, 29 (slow drift reached 0.59%), and the
  turn 45–53.
- 17-08-12 keeps 12 of 15: that pot turned during 6–11 (5–7% per frame), and the still
  frames 4, 5 and 12 were skipped.

Original notes:

- Compare each camera's thumbnail against that camera's last **saved** frame, not the
  previous frame. That way slow drift accumulates and is still recorded, and a stalled
  machine saves nothing.
- Save the whole group if any camera exceeds the threshold. The zipper already pairs
  frames by time; this just keeps them together.
- Threshold: 0.5% of pixels above 16 levels. Measured: a static hold gives 0%, turning
  1.4–7%. In 17-06-39 this would have kept about 10 of 52 held frames.
- The frame limit counts only saved frames, so a stall doesn't use it up.
- Costs under 1 ms per camera on the acquisition thread.
- Server flag, off by default, enabled in `launch-server.py`. Manual recordings of
  static scenes (calibration, sync tests) are unaffected.
- Wobble and watering while the machine is stalled still cause saves. Accept that: the
  geometry really did change. Check against a run with watering. If flowing water
  alone keeps every frame, restrict the comparison to the ROI or raise the threshold.
- Later upgrade: the ring's in-plane angle is easy to read from the marker centres, so
  B could save one frame per N degrees of rotation, which gives even coverage for
  reconstruction.

### C. Tune the trigger for this cycle — done (2026-10-08), pending a rig test

In `launch-server.py`: markers `DICT_6X6_50:1..8`, ROI `0.4,0.15,1.0,0.9`,
`--aruco-stop-after 3`, `--frames 300`.

**Correction to the pass length.** The cameras are synchronised: their frames arrive
6–13 ms apart, faster than one detection (17–20 ms). So the second camera's frame is
always dropped while the first is examined, and each camera is checked every other
frame. At 1 fps a pass takes 2 s, not 0.4 s. `--aruco-stop-after 3` therefore stops
about 6 s after the pot leaves; the replay confirms it (last marker frame + 6 or 7).
Start latency is 2–4 s: two sightings on one camera, 2 s apart.

Original notes:

- `--aruco-markers DICT_6X6_50:1,2,3,4,5,6,7,8`. The `launch-server.py` line
  currently says `DICT_5X5_250:1..9` and is commented out.
- Raise `--aruco-stop-after` to cover about 3–5 s: with 2 cameras at the 200 ms
  interval, that's 8–12 passes. Held pots showed no marker dropouts, and pots are about
  40 s apart, so anything in that range separates pots cleanly while tolerating an
  occlusion by the watering arm. Today the window is 3 passes ≈ 1.2 s.
- Size `--frames` well above one rotation's worth of *changed* frames, once B is in
  place. Hitting the limit mid-rotation stops the recording, and the absence latch
  then blocks a restart while that pot is still in view, so the rest of the turn is
  lost. The full rotation duration is still unmeasured.

### D. Stall handling: warn, don't stop

Send the client a notice like "no new frames for N s" instead of auto-stopping. With B
in place a stalled recording costs nothing, and stopping would lose the rest of the
turn if the machine resumes on the same pot.

## arucotools features left unused

The update itself is step 0. These newer features were tried on the 2026-10-07 frames
and not adopted:
- **`rejected` candidates**, with a reason per dictionary. Not used yet, but the first
  thing to log if the rig misses markers, e.g. under growth-light reflections.
- **`confidence`**: far-pot markers read 0.85–0.92, held ones 0.86–1.0, so it neither
  separates them nor gates size. No false positives have needed it as a guard.
- **Aruco3 mode** is faster (7–14 ms against 17 ms), but its size cutoff is coarse:
  about 1.2–1.4× above its `(32 + r·M)·2k` px formula. The explicit gate (A) is
  predictable, and detection isn't a bottleneck at one detection per second.
- **AprilTag corner refinement**: ~100 ms at k=3, with tighter corners. That's for
  the offline calibration, not for triggering.

## Measurements on gripped pots (2026-10-07 17-06-39 and 17-08-12)

Two consecutive pots, recorded separately. The machine was otherwise empty. The pots
had already been watered, so each turns only about one marker step and back. Markers
near the right edge are clipped by the sensor, and the growth lights cause
reflections. Both are normal operating conditions. All figures below are at
production settings: k=3, full `DICT_6X6_50`.

- **Ids:** 1–8 all seen. 7 only once, and only at k=2; it is probably the clipped one.
- **Where held markers land (cam0):** x 0.70–0.96, y 0.32–0.63 (ring centre ≈ 0.85,
  0.48). On cam1: x 0.80–0.96, y 0.20–0.60. The gripped pot sits on the right edge,
  not the centre; the cameras will be adjusted, so re-measure afterwards.
- **In transit** (arriving or leaving, one frame each): y 0.06–0.27.
- **Pots in the rack** sit at the top of the frame: y ≤ 0.09, 110–145 px. Returned
  pots end up there.
- **Size:** held markers are 179–228 px on cam0 and 149–200 px on cam1. The rack is
  110–145 px. **Size alone is marginal** (149 against 145), so **position is the
  primary test**; any y cut-off between about 0.15 and 0.3 separates them. Keep the
  minimum size optional.
- **Detection under reflections:** every held frame has ≥ 3 detections on cam0 and
  ≥ 1 on cam1 at 1 fps. k=2 adds 2% more markers at 2× the cost, so keep k=3.
  Confidence (0.86–1.0) doesn't separate held from rack.
- **Timeline, recording 17-06-39:** markers frames 2–53. Static frames 3–44 (marker
  positions identical to 0.001, about 40 s). Turning frames 45–52. Leaving 53. No
  markers after that, while the machine moves on.
- **Timeline, recording 17-08-12:** markers frames 1–15, turning 6–10 and 13–14.
- **No mid-hold dropouts**, so a stop-after of a few seconds is safe. The gap between
  pots is about 40 s with no markers, well above any sensible stop window. Each pot
  becomes its own recording naturally.
- **Change metric** (share of pixels above 16 levels, adjacent frames):
  - static hold: **0.0000**
  - turning: 1.4–7%
  - arrival and departure: 5–27%
  - One pair in 17-06-39 (frames 50→51) changes by 0.51% with the markers unmoved,
    right at the 0.5% threshold. That's harmless either way: comparing against the
    last *saved* frame makes small changes add up.
  - About 80% of held frames in 17-06-39 (≈ 42 of 52) were duplicates.

## Still needed

Rig test with `launch-server.py` as committed (debug logging on):
- After adjusting the cameras, check the "aruco marker" lines. Every held marker should be
  `admitted=true` through a full turn, and any rack pot `admitted=false`. The ROI's top
  edge (0.15) is the tight side: the lowest held marker so far was at y 0.20.
- Whether the watering arm hides the markers for longer than ~6 s (`--aruco-stop-after`).
- A full turn with watering: its duration (is `--frames 300` enough?), and whether
  flowing water alone keeps the change score above 0.5% (the `groups_skipped` count
  at the end of each recording).
- The longest normal pause during a pot, to size D's notice.
