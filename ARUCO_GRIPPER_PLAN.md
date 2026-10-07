# Aruco auto-record on the watering machine: findings and plan

Temporary working note (2026-10-07). Delete once the plan below is implemented or
dropped. No code has been changed for it yet.

## The setting

Every day the machine picks each pot in turn, rotates it in front of the stereo pair
while watering it, and puts it back. Each pot should become one recording.

- Every pot carries the **same** ring of ArUco markers, family `DICT_6X6_50`. There are
  probably 8 of them: ids 1 and 8 are both visible on a single pot, so a consecutive
  set would be 1–8. Confirm on a real recording.
- Each pot also has a QR code that identifies it.
- The markers are there for calibration and 3D reconstruction. The plant deforms
  (wobble, watering), which is why the rig is stereo: each time-matched pair stands on
  its own.

## Problems

1. **Pots that aren't being handled trigger recordings.** Pots waiting further back are
   in view and carry the same marker ids. Only the pot in the gripper (centre of both
   frames) should start or sustain a recording.
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

### A. Marker gate: count a marker only if it is in the right place and large enough

- Add `--aruco-roi x0,y0,x1,y1`: the rectangle the marker's centre must fall in, as
  fractions of the frame.
- Add `--aruco-min-side-px N`: the minimum mean side length, in full-resolution pixels.
- Filter in the detector thread, before the trigger, so a gated-out marker neither
  starts nor sustains a recording.
- Log every marker's geometry and whether it passed, at debug level, for tuning.
- A draft exists but was not applied. It lived in the session scratchpad, so treat it
  as gone. It added `MarkerGate { roi, min_side_px }` to `AutoRecordCfg`, and passed it
  plus the scale `2*downscale` to `worker::run`/`detect`.
- Size the gate on a full rotation: side markers foreshortened at a steep angle look
  smaller, and the ROI must cover the whole ring's projection.
- Zero-code stopgap: `--aruco-min-perimeter-rate ≈ 0.25` rejects markers under about
  220 px at k=3. It filters on size only, and may also cut foreshortened gripped
  markers.

### B. Skip frame groups that haven't changed since the last saved group

- Compare each camera's thumbnail against that camera's last **saved** frame, not the
  previous frame. That way slow drift accumulates and is still recorded, and a stalled
  machine saves nothing.
- Save the whole group if any camera exceeds the threshold, which preserves the
  time-matched pairs. Start with a threshold of about 0.5% of pixels above 16 levels.
- The frame limit counts only saved frames, so a stall doesn't use it up.
- Costs under 1 ms per camera on the acquisition thread.
- Server flag, off by default, enabled in `launch-server.py`. Manual recordings of
  static scenes (calibration, sync tests) are unaffected.
- Wobble and watering while the machine is stalled still cause saves. Accept that: the
  geometry really did change. Optionally restrict the comparison to the gripper ROI.
- Later upgrade: estimate the pot's rotation angle from the markers and save one frame
  per N degrees, which gives even coverage for reconstruction.

### C. Tune the trigger for this cycle

- Raise `--aruco-stop-after` so the no-marker window is a few seconds. Today it is
  3 passes × 2 cameras × 200 ms ≈ 1.2 s. A brief occlusion by the gripper or watering
  arm would otherwise stop the recording, and after the 5 s cooldown a new one starts:
  one pot split over several recordings. The window must stay shorter than the gap
  between pots.
- Size `--frames` well above one rotation's worth of frames. Hitting the limit
  mid-rotation stops the recording, and the absence latch then blocks a restart while
  that pot is still in view, so the rest of the turn is lost.
- Configure ids generously, e.g. `DICT_6X6_50:0,1,2,3,4,5,6,7,8,9`, until the real set
  is confirmed.

### D. Stall handling: warn, don't stop

Send the client a notice like "no new frames for N s" instead of auto-stopping. With B
in place a stalled recording costs nothing, and stopping would lose the rest of the
turn if the machine resumes on the same pot.

### E. Pot identity from the QR code

Read it offline, from the saved frames, in downstream processing. Not in the server.

## Still needed

A recording of a pot in the gripper through a full rotation, ideally while watering.
It settles:

- the actual marker ids
- the ROI and minimum size
- the longest gap with no marker visible (for stop-after)
- the rotation duration (for `--frames`)
- the change metric while turning versus while stalled (for B's threshold)
