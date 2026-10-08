# Aruco auto-record on the watering machine: findings and plan

Temporary working note (2026-10-07, updated 2026-10-08). Delete once the plan below is implemented or
dropped. No code has been changed for it yet.

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

- **`launch-server.py` names the wrong family.** It says `DICT_5X5_250:1..9`, which
  can never match the pots. Use `DICT_6X6_50`.
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

Order of work: 0 (arucotools update), then A, B and C. D and E are small and
independent.

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

### A. Marker gate: count a marker only if it is in the right place

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

### B. Skip frame groups that haven't changed since the last saved group

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

### C. Tune the trigger for this cycle

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

### E. Pot identity from the QR code

Read it offline, from the saved frames, in downstream processing. Not in the server.

## arucotools main (4a57f47) vs our pin (fdf2ad9)

41 commits ahead. Tried with a scratch crate on the 2026-10-07 frames and all 792
frames from 2026-09-22.

**Breaking API changes** that the bump has to absorb:
- One `MarkerFamily` type replaces `OwnedMarkerFamily`, `StaticMarkerFamily` and
  `HasMarkerFamily`. Fields are now private behind accessors. `nbits_side_pattern` is
  now `nbits_pattern_side`, and `nbits_hamming` is now `nbits_max_correction`.
- `detect_markers(&img, &[&MarkerFamily], &params)` takes several dictionaries and
  returns `Detections { markers: Vec<DetectedMarker { marker, dictionary, confidence }>,
  rejected: Vec<RejectedCandidate { corners, reasons }> }`.
- Defaults moved to OpenCV 5.0.0: `min_marker_distance_rate` 0.125, a border-error
  budget, `valid_bit_id_threshold` 0.49. On our frames, detections and positions are
  unchanged.

**What it means for the server:**
- **Most of `aruco/markers.rs` can go.** The library now finds candidates once and
  identifies them against every dictionary passed in, resolving overlapping
  dictionaries to one marker by confidence and then list order. So pass the
  configured predefined families whole, and keep the hits whose id is configured.
  - This is also more robust than a shrunken code list: an unconfigured id is
    recognised as itself and dropped, instead of being error-corrected onto a
    configured one.
  - The extra cost is negligible: a handful of candidates against 50 codes.
  - Zero false positives over the 792 frames with the full `DICT_6X6_50`.
- **`confidence`** is available as an extra false-positive guard. Far-pot markers read
  0.85–0.92, large markers 0.94–1.0. It's not a size gate.
- **`rejected`** candidates help diagnose missed detections on the rig.
- **Aruco3 mode is fast, but its size cutoff is coarse.**
  - Its minimum marker size is roughly `(32 + r·M)·2k` full-res px. In practice the
    cutoff lands about 1.2–1.4× above that. At k=3 with r=0.02, a 260–290 px marker is
    missed and a ≥ 390 px one detected; with r=0.05, a 390–430 px marker is missed and
    a ≥ 520 px one detected.
  - It runs in 7–14 ms against 17 ms for our current settings. Detection isn't a
    bottleneck at 17 ms per 200 ms.
  - So keep the explicit gate (plan A) and our standard settings.
- **AprilTag mode** takes ~100 ms at k=3, 6× our settings, with slightly tighter
  corners. For the offline calibration, not for triggering.
- **`arucotools-calib`** (multi-camera bundle adjustment, unknown marker layout,
  stereo-rig constraint, turntable example) is for offline processing. It's out of
  scope for the recorder.

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

- Re-measure the ROI after the cameras are adjusted to stop clipping the ring.
- A run with watering and a full rotation, for the rotation duration (`--frames`) and
  the change metric while water is flowing.
