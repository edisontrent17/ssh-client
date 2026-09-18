"""Compare terminal benchmark runs and optionally enforce local p95/allocation budgets."""
import argparse
import json
from pathlib import Path


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("before", type=Path)
    parser.add_argument("after", type=Path)
    parser.add_argument("--max-idle-ms", type=float)
    parser.add_argument("--max-active-ms", type=float)
    parser.add_argument("--max-allocated-bytes", type=int)
    args = parser.parse_args()
    before = json.loads(args.before.read_text())
    after = json.loads(args.after.read_text())
    for field in ("benchmark", "os", "arch", "scrollback_lines"):
        if before[field] != after[field]:
            raise SystemExit(f"Incompatible benchmark metadata: {field}")

    def index(run):
        rows = {(tuple(row["viewport"]), row["tabs"], row["scenario"]): row
                for row in run["results"]}
        if not rows or len(rows) != len(run["results"]):
            raise SystemExit("Empty or duplicate benchmark cases")
        return rows

    baseline, current = index(before), index(after)
    if baseline.keys() != current.keys():
        raise SystemExit("Runs contain different benchmark cases")
    failed = []
    print("| Viewport | Tabs | Scenario | Before ms | After ms | After p95 ms | Before allocations | After allocations | Before bytes | After bytes |")
    print("| --- | ---: | --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: |")
    for case, row in current.items():
        old = baseline[case]
        viewport = " × ".join(str(int(n)) for n in case[0])
        print(f'| {viewport} | {case[1]} | {case[2]} | {old["median_ms"]:.3f} | {row["median_ms"]:.3f} | {row["p95_ms"]:.3f} | {old["median_allocations"]:,} | {row["median_allocations"]:,} | {old["median_allocated_bytes"]:,} | {row["median_allocated_bytes"]:,} |')
        budget = args.max_idle_ms if case[2] == "idle_redraw" else args.max_active_ms
        if budget is not None and row["p95_ms"] > budget:
            failed.append(f"{case}: p95 {row['p95_ms']:.3f} ms exceeds {budget:.3f} ms")
        if args.max_allocated_bytes is not None and row["median_allocated_bytes"] > args.max_allocated_bytes:
            failed.append(f"{case}: median allocated bytes exceeds {args.max_allocated_bytes:,}")
    if failed:
        raise SystemExit("\n".join(failed))


if __name__ == "__main__":
    main()
