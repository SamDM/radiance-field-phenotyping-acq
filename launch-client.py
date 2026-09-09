#!/usr/bin/env python3
import subprocess
import sys

try:
    result = subprocess.run(
        ["cargo", "run", "--manifest-path", "app/Cargo.toml",
         "-p", "acquisition-client", "--release",
         "--",
         "--control-url", "ws://phenominator:8081",
         "--preview-url", "ws://phenominator:8082",
        ],
        env={**__import__("os").environ, "RUST_LOG": "info"},
    )
    sys.exit(result.returncode)
except KeyboardInterrupt:
    print("\nCtrl-C pressed, exiting.")
    sys.exit(1)