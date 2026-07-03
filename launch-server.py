#!/usr/bin/env python3
import subprocess
import sys

try:
    result = subprocess.run(
        ["cargo", "run", "--manifest-path", "app/Cargo.toml",
         "-p", "acquisition-server", "--release",
         "--",
         "--n-cameras", "2",
         "--frames", "500",
         "--framerate", "3.0",
        #  "--auto-adjust-duration-secs", "30",
        #  "--auto-adjust-minimize-gain",
        #  "--auto-adjust-no-boost-framerate",
         "--frame-format", "png",
        #  "--frame-format", "jpeg",
        ],
        env={**__import__("os").environ, "RUST_LOG": "info"},
    )
    sys.exit(result.returncode)
except KeyboardInterrupt:
    print("\nCtrl-C pressed, exiting.")
    sys.exit(1)
