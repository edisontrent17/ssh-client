#!/usr/bin/env python3
"""Measure isolated Relay UI fixtures on macOS; never attach to the user's app."""
import argparse
import hashlib
import json
import pathlib
import platform
import re
import subprocess
import time


def size_bytes(value):
    match = re.fullmatch(r"([\d.]+)([BKMG])", value)
    if not match:
        raise ValueError(f"Unexpected vmmap size: {value}")
    return round(float(match[1]) * 1024 ** "BKMG".index(match[2]))


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("binary", type=pathlib.Path, help="release visual_preview binary")
    parser.add_argument("output", type=pathlib.Path)
    parser.add_argument("--settle-seconds", type=float, default=15)
    parser.add_argument("--samples", type=int, default=3)
    parser.add_argument("--interval-seconds", type=float, default=2)
    args = parser.parse_args()
    if platform.system() != "Darwin":
        parser.error("requires macOS vmmap and access to the desktop")
    if args.samples < 1 or args.settle_seconds < 0 or args.interval_seconds < 0:
        parser.error("invalid sampling settings")
    binary = args.binary.resolve(strict=True)
    report = {
        "schema": 1,
        "platform": platform.platform(),
        "binary_sha256": hashlib.sha256(binary.read_bytes()).hexdigest(),
        "fixture_viewport": [1280, 800],
        "settle_seconds": args.settle_seconds,
        "interval_seconds": args.interval_seconds,
        "samples_per_state": args.samples,
        "measurements": [],
    }
    for state in ["empty", "workspace", "terminals"]:
        # Only local shell fixtures; none of these states connects to an SSH host.
        process = subprocess.Popen([str(binary), "--interactive", "1280", "800", state])
        try:
            time.sleep(args.settle_seconds)
            for sample in range(args.samples):
                if process.poll() is not None:
                    raise RuntimeError(f"{state} fixture exited before measurement")
                output = subprocess.check_output(
                    ["/usr/bin/vmmap", "-summary", str(process.pid)], text=True, timeout=30
                )
                measurement = {"state": state, "sample": sample}
                for label, key in [
                    ("Physical footprint:", "footprint_bytes"),
                    ("Physical footprint (peak):", "peak_footprint_bytes"),
                ]:
                    match = re.search(r"^" + re.escape(label) + r"\s+(\S+)", output, re.M)
                    if not match:
                        raise ValueError(f"vmmap did not report {label}")
                    measurement[key] = size_bytes(match[1])
                report["measurements"].append(measurement)
                args.output.write_text(json.dumps(report, indent=2) + "\n")
                print(json.dumps(measurement), flush=True)
                if sample + 1 < args.samples:
                    time.sleep(args.interval_seconds)
        finally:
            # Terminate only the child fixture we launched. Closing its PTY
            # masters also hangs up the local demo shells, including /bin/cat.
            process.terminate()
            try:
                process.wait(timeout=10)
            except subprocess.TimeoutExpired:
                process.kill()
                process.wait()


if __name__ == "__main__":
    main()
