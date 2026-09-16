#!/usr/bin/env python3
"""Move finished recordings off the acquisition PC to a network drive.

At ~180 recordings a day of ~1.3 GiB each, the local disk holds well under a day's
work, so recordings have to leave continuously rather than in a nightly batch.

Run one pass per invocation (suitable for cron), or use --watch to keep running:

    */10 * * * * /path/to/launch-offload.py >> /var/log/rfp-offload.log 2>&1

A recording is finished exactly when its folder contains `metadata.json` -- the server
writes that once, in `finish_recording`, after every frame is on disk. A folder without
one is either in progress or was abandoned, and is never touched.

Nothing here coordinates with the running server, and nothing needs to: the server
re-checks its library against the disk while it runs, so recordings that leave stop
being listed on their own.
"""

import argparse
import fcntl
import os
import subprocess
import sys
import time
from pathlib import Path

# --- configuration -------------------------------------------------------------------

# Where the server writes; matches --output-dir's default of $HOME/phenominator.
SOURCE = Path.home() / "phenominator"

# Where recordings go. Point this at the real mount when it is available. The final
# path component is created if missing; its parent has to exist already.
DEST = Path("/tmp/test-offload")

# Leave recordings alone for this long after they finish. `metadata.json` already means
# "complete", so this is not about catching a half-written recording; it is slack for the
# operator to rate or comment on something they just recorded before it disappears.
MIN_AGE_SECONDS = 300

# Set True once DEST is a real mount. Without it, an unmounted target means the script
# happily writes into the local filesystem under the mountpoint -- filling the very disk
# this exists to protect, while reporting success.
REQUIRE_MOUNTPOINT = False

# One pass at a time. Cron will happily start a second run while a slow transfer is still
# going, and two rsyncs moving the same files is not a situation worth reasoning about.
LOCK_FILE = Path("/tmp/rfp-offload.lock")

WATCH_INTERVAL_SECONDS = 600

# --- implementation ------------------------------------------------------------------


def log(message: str) -> None:
    print(f"{time.strftime('%Y-%m-%dT%H:%M:%S')} {message}", flush=True)


def finished_recordings(min_age_seconds: int) -> list[Path]:
    """Recording folders old enough to move, oldest first.

    A recording folder is one containing `metadata.json`, and it is a leaf: the walk does
    not descend into it. That mirrors the server's own scan, so the two agree on what is
    a recording and what is just a directory on the way to one.
    """
    if not SOURCE.is_dir():
        return []

    now = time.time()
    found: list[Path] = []

    def walk(directory: Path, depth: int) -> None:
        if depth == 0:
            return
        metadata = directory / "metadata.json"
        if metadata.is_file():
            if now - metadata.stat().st_mtime >= min_age_seconds:
                found.append(directory)
            return
        for child in sorted(directory.iterdir()):
            if child.is_dir():
                walk(child, depth - 1)

    walk(SOURCE, 8)
    return found


def folder_size(directory: Path) -> int:
    return sum(f.stat().st_size for f in directory.rglob("*") if f.is_file())


def move_recording(source_dir: Path) -> bool:
    """Move one recording. Returns whether it left completely."""
    relative = source_dir.relative_to(SOURCE)
    target = DEST / relative
    target.mkdir(parents=True, exist_ok=True)

    # Two passes so `metadata.json` is the last file to leave the source.
    #
    # It is the marker that makes a folder a recording, to this script and to the server
    # alike. If a transfer dies half way with the marker already gone, the frames left
    # behind stop looking like a recording to either of us: this script would never
    # retry them and the server would never list them, so they would sit there forever.
    # Moving it last means an interrupted run always leaves something both ends still
    # recognise, and the next run simply finishes the job.
    passes = [
        ["--exclude", "metadata.json", f"{source_dir}/", f"{target}/"],
        [f"{source_dir}/metadata.json", f"{target}/"],
    ]
    for arguments in passes:
        result = subprocess.run(
            ["rsync", "-a", "--remove-source-files", *arguments],
            capture_output=True,
            text=True,
        )
        if result.returncode != 0:
            log(f"FAILED {relative}: rsync exited {result.returncode}: {result.stderr.strip()}")
            return False

    # --remove-source-files clears files but leaves directories behind.
    leftovers = list(source_dir.iterdir())
    if leftovers:
        log(f"FAILED {relative}: {len(leftovers)} files did not transfer, leaving it in place")
        return False
    source_dir.rmdir()
    return True


def prune_empty_dirs() -> None:
    """Remove the date directories left behind once their recordings have gone."""
    for directory in sorted(SOURCE.rglob("*"), key=lambda p: len(p.parts), reverse=True):
        if directory.is_dir() and not any(directory.iterdir()):
            directory.rmdir()


def check_destination() -> None:
    """Create DEST if its parent already exists, and refuse to go deeper than that.

    Creating the final component is a convenience; creating a whole missing path is how
    a typo or an unmounted drive turns into recordings quietly filling the local disk.
    Requiring the parent to exist keeps the former without allowing the latter.
    """
    if not DEST.parent.is_dir():
        sys.exit(f"{DEST.parent} does not exist -- refusing to create a path that deep")
    DEST.mkdir(exist_ok=True)
    if REQUIRE_MOUNTPOINT and not os.path.ismount(DEST):
        sys.exit(f"destination {DEST} is not a mountpoint -- refusing to write locally")


def one_pass(min_age_seconds: int, dry_run: bool) -> None:
    check_destination()
    recordings = finished_recordings(min_age_seconds)
    if not recordings:
        log("nothing to offload")
        return

    moved = 0
    moved_bytes = 0
    failed = 0
    for source_dir in recordings:
        relative = source_dir.relative_to(SOURCE)
        size = folder_size(source_dir)
        if dry_run:
            log(f"would move {relative} ({size / 2**30:.2f} GiB)")
            continue
        if move_recording(source_dir):
            log(f"moved {relative} ({size / 2**30:.2f} GiB)")
            moved += 1
            moved_bytes += size
        else:
            failed += 1

    if not dry_run:
        prune_empty_dirs()
        log(f"offloaded {moved} recordings ({moved_bytes / 2**30:.2f} GiB), {failed} failed")
        if failed:
            sys.exit(1)


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--watch", action="store_true", help="keep running instead of one pass")
    parser.add_argument("--interval", type=int, default=WATCH_INTERVAL_SECONDS)
    parser.add_argument("--min-age", type=int, default=MIN_AGE_SECONDS)
    parser.add_argument("--dry-run", action="store_true", help="report without moving")
    args = parser.parse_args()

    lock = open(LOCK_FILE, "w")
    try:
        fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
    except BlockingIOError:
        log("another offload is already running, exiting")
        sys.exit(0)

    if not args.watch:
        one_pass(args.min_age, args.dry_run)
        return

    log(f"watching {SOURCE} -> {DEST} every {args.interval}s")
    try:
        while True:
            one_pass(args.min_age, args.dry_run)
            time.sleep(args.interval)
    except KeyboardInterrupt:
        log("Ctrl-C pressed, exiting.")


if __name__ == "__main__":
    main()
