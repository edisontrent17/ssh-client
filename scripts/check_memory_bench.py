#!/usr/bin/env python3
"""Compare retained Rust heap, and bound growth across terminal open/close cycles."""
import argparse
import json


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("before")
    parser.add_argument("after")
    parser.add_argument("--min-saved-mib", type=float, default=0)
    parser.add_argument("--max-closed-growth-kib", type=float, default=64)
    args = parser.parse_args()
    with open(args.before) as file:
        before = json.load(file)
    with open(args.after) as file:
        after = json.load(file)
    for key in ["schema", "viewport", "history_lines", "tabs", "cycles", "metric"]:
        if before[key] != after[key]:
            parser.error(f"incompatible {key}")
    old = before["measurements"]
    new = after["measurements"]
    if not new or [x["stage"] for x in old] != [x["stage"] for x in new]:
        parser.error("incompatible or empty stage lists")
    failures = []
    for a, b in zip(old, new):
        saved = (a["live_rust_bytes"] - b["live_rust_bytes"]) / 2**20
        print(f"{a['stage']}: {a['live_rust_bytes']/2**20:.2f} -> "
              f"{b['live_rust_bytes']/2**20:.2f} MiB ({saved:.2f} MiB saved)")
        if saved < args.min_saved_mib:
            failures.append(f"{a['stage']}: savings below {args.min_saved_mib} MiB")
    closed = [x["live_rust_bytes"] for x in new if x["stage"].endswith("_closed_tabs")]
    if len(closed) != after["cycles"] or len(closed) < 2:
        parser.error("missing closed-tab cycle measurements")
    growth = (max(closed[1:]) - closed[0]) / 1024
    print(f"Closed-tab retained growth after the first cycle: {growth:.2f} KiB")
    if growth > args.max_closed_growth_kib:
        failures.append(f"closed-tab growth exceeds {args.max_closed_growth_kib} KiB")
    if failures:
        raise SystemExit("\n".join(failures))


if __name__ == "__main__":
    main()
