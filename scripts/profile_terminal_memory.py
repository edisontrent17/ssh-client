#!/usr/bin/env python3
"""Profile disposable native terminal fixtures; saves complete vmmap summaries."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import platform
import re
import subprocess
import time


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("binary", type=Path)
    parser.add_argument("output", type=Path)
    parser.add_argument("--engines", nargs="+", choices=["standard", "ghostty"], default=["standard", "ghostty"])
    parser.add_argument("--counts", nargs="+", type=int, default=[0, 1, 2, 3, 4, 5])
    parser.add_argument("--settle-seconds", type=float, default=15)
    parser.add_argument("--skip-empty-history", action="store_true")
    parser.add_argument("--visit-all-tabs", action="store_true", help="Display every tab before measuring retained rendering resources")
    args = parser.parse_args()
    if platform.system() != "Darwin" or args.settle_seconds <= 0 or any(n < 0 for n in args.counts):
        parser.error("requires macOS, positive settling time, and nonnegative counts")
    binary = args.binary.resolve(strict=True)
    args.output.mkdir(parents=True, exist_ok=True)
    report = {"platform": platform.platform(), "binary_sha256": hashlib.sha256(binary.read_bytes()).hexdigest(),
              "settle_seconds": args.settle_seconds, "viewport": [1280, 800],
              "visit_all_tabs": args.visit_all_tabs, "measurements": []}
    for engine in args.engines:
        env = dict(os.environ)
        env["RELAY_MEMORY_PROBE"] = "1"
        env.pop("RELAY_MEMORY_VISIT_ALL", None)
        if args.visit_all_tabs:
            env["RELAY_MEMORY_VISIT_ALL"] = "1"
        env.pop("RELAY_GHOSTTY", None)
        if engine == "ghostty":
            env["RELAY_GHOSTTY"] = "1"
        cases = [(n, 2100) for n in args.counts]
        if not args.skip_empty_history:
            cases.append((5, 0))
        for count, lines in cases:
            label = f"{engine}-{count}-tabs-{lines}-lines"
            # Close the five-tab fixture after the live sample, then measure
            # the same process again. Never terminate an existing user app.
            close_after = int(args.settle_seconds) + 8
            command = [str(binary), "--interactive", "1280", "800", "terminals", str(count), str(lines)]
            if count == 5:
                command += [str(close_after)]
            with (args.output / f"{label}.log").open("w") as log:
                process = subprocess.Popen(command, env=env, stdout=log, stderr=subprocess.STDOUT)
                started = time.monotonic()
                try:
                    deadline = started + 90
                    log_path = args.output / f"{label}.log"
                    while "Fixture ready" not in log_path.read_text():
                        if process.poll() is not None or time.monotonic() >= deadline:
                            raise RuntimeError(f"{label} did not become ready")
                        time.sleep(0.1)
                    ready = time.monotonic()
                    stages = [("open", args.settle_seconds)]
                    if count == 5:
                        stages += [("closed", close_after + 10)]
                    for stage, elapsed in stages:
                        time.sleep(max(0, ready + elapsed - time.monotonic()))
                        if process.poll() is not None:
                            raise RuntimeError(f"{label} exited before sampling")
                        output = subprocess.check_output(["/usr/bin/vmmap", "-summary", str(process.pid)], text=True, timeout=30)
                        (args.output / f"{label}-{stage}.txt").write_text(output)
                        row = {"engine": engine, "tabs": count, "output_lines": lines, "stage": stage,
                               "elapsed_seconds": round(time.monotonic() - started, 1), "startup_seconds": round(ready-started, 2)}
                        log_text = log_path.read_text()
                        if args.visit_all_tabs:
                            row["all_tabs_visited"] = "Fixture all tabs visited" in log_text
                        if stage == "closed" and "Fixture tabs closed" not in log_text:
                            # macOS may suppress UI frames in an occluded native
                            # window. Keep the observation, never call it closed.
                            row["stage"] = "close_not_observed"
                            row["close_started"] = "Fixture closing tabs" in log_text
                        samples = [json.loads(line) for line in log_text.splitlines() if line.startswith('{') and '"live_rust_bytes":' in line]
                        if samples:
                            row.update(samples[-1])
                        for title, key in [("Physical footprint:", "footprint_mib"), ("Physical footprint (peak):", "peak_mib")]:
                            match = re.search(r"^" + re.escape(title) + r"\s+([\d.]+)([BKMG])", output, re.M)
                            if not match:
                                raise RuntimeError(f"missing {title}")
                            row[key] = round(float(match[1]) * 1024 ** ("BKMG".index(match[2]) - 2), 3)
                        report["measurements"].append(row)
                        (args.output / "results.json").write_text(json.dumps(report, indent=2) + "\n")
                        print(json.dumps(row), flush=True)
                finally:
                    process.terminate()
                    try:
                        process.wait(timeout=10)
                    except subprocess.TimeoutExpired:
                        process.kill()
                        process.wait()


if __name__ == "__main__":
    main()
