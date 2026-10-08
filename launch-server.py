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
         "--frames", "100",
         "--framerate", "1.0",
        #  "--auto-adjust-duration-secs", "30",
        #  "--auto-adjust-minimize-gain",
        #  "--auto-adjust-no-boost-framerate",
         "--frame-format", "png",
        #  "--frame-format", "jpeg",
         "--zoom-size", "101",
         # Auto-record: any of these markers, on either camera, starts a recording; it
         # keeps running while any camera still sees one. Both cameras are watched
         # because --aruco-cameras defaults to all of them.
         # "--aruco-markers", "DICT_5X5_250:1,2,3,4,5,6,7,8,9",
        ],
        env={**__import__("os").environ, "RUST_LOG": "info"},
    )
    sys.exit(result.returncode)
except KeyboardInterrupt:
    print("\nCtrl-C pressed, exiting.")
    sys.exit(1)
