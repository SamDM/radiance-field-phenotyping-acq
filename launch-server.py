#!/usr/bin/env python3
import subprocess
import sys

try:
    result = subprocess.run(
        ["cargo", "run", "--manifest-path", "app/Cargo.toml",
         "-p", "acquisition-server", "--release",
         "--features", "aruco-auto-record",
         "--",
         "--control-addr", "0.0.0.0:8081",
         "--preview-addr", "0.0.0.0:8082",
         "--n-cameras", "2",
         # A safety cap, not the normal end of a recording: auto-recordings stop when the
         # pot leaves. A held pot took 52 s without a full turn (2026-10-07); a full turn
         # with watering is still unmeasured, and hitting the cap mid-turn loses the rest
         # of that pot, so leave plenty of room.
         "--frames", "300",
         "--framerate", "1.0",
        #  "--auto-adjust-duration-secs", "30",
        #  "--auto-adjust-minimize-gain",
        #  "--auto-adjust-no-boost-framerate",
         "--frame-format", "png",
        #  "--frame-format", "jpeg",
         "--zoom-size", "101",
         # Auto-record: the ring of markers on every pot's underside. Any of them, on
         # either camera, starts a recording; it keeps running while any camera still
         # sees one. Both cameras are watched because --aruco-cameras defaults to all.
         "--aruco-markers", "DICT_6X6_50:1,2,3,4,5,6,7,8",
         # Only markers centred in this part of the frame count (x0,y0,x1,y1). Pots in
         # the rack carry the same markers and sit at the top (y <= 0.09); the gripped
         # pot's markers sit at y 0.20-0.64, x 0.70-0.96 (2026-10-07). y separates them;
         # x is left loose so the planned camera adjustment doesn't push the pot out.
         # Re-check with the "aruco marker" debug lines after moving the cameras.
         "--aruco-roi", "0.4,0.15,1.0,0.9",
         # Passes over both cameras without a marker before stopping. At 1 fps a pass
         # takes 2 s (each camera is examined every other frame), so this stops ~6 s
         # after the pot leaves. Pots are ~40 s apart.
         "--aruco-stop-after", "3",
        ],
        # aruco=debug logs every marker's position, size and whether the ROI admitted
        # it, for tuning. Drop back to plain "info" once the ROI is settled.
        env={**__import__("os").environ, "RUST_LOG": "info,acquisition_server::aruco=debug"},
    )
    sys.exit(result.returncode)
except KeyboardInterrupt:
    print("\nCtrl-C pressed, exiting.")
    sys.exit(1)
