#!/usr/bin/env python3
import subprocess
import sys

try:
    result = subprocess.run(
        ["cargo", "run", "--manifest-path", "app/Cargo.toml",
         "-p", "acquisition-client", "--release",
         "--",
         "--control-url", "ws://127.0.0.1:8081",
         "--preview-url", "ws://127.0.0.1:8082",
        ],
        env={**__import__("os").environ, "RUST_LOG": "info"},
    )
    sys.exit(result.returncode)
except KeyboardInterrupt:
    print("\nCtrl-C pressed, exiting.")
    sys.exit(1)