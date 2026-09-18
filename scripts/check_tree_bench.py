"""Compare local benchmark runs and optionally enforce machine-specific budgets."""
import argparse
import json
from pathlib import Path


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("before", type=Path)
    parser.add_argument("after", type=Path)
    parser.add_argument("--max-redraw-ms", type=float)
    parser.add_argument("--max-query-ms", type=float)
    args = parser.parse_args()
    before = json.loads(args.before.read_text())
    after = json.loads(args.after.read_text())
    for field in ("benchmark", "viewport", "os", "arch"):
        if before[field] != after[field]:
            raise SystemExit(f"Incompatible benchmark metadata: {field}")
    key = lambda row: (row["files"], row["layout"], row["scenario"])
    baseline = {key(row): row for row in before["results"]}
    current = {key(row): row for row in after["results"]}
    if baseline.keys() != current.keys():
        raise SystemExit("Runs contain different benchmark cases")
    failed = []
    print("| Files | Layout | Scenario | Before median (ms) | After median (ms) | After p95 (ms) |")
    print("| ---: | --- | --- | ---: | ---: | ---: |")
    for case, row in current.items():
        old = baseline[case]
        print(f'| {case[0]:,} | {case[1]} | {case[2]} | {old["median_ms"]:.3f} | {row["median_ms"]:.3f} | {row["p95_ms"]:.3f} |')
        budget = args.max_query_ms if case[2] == "query_change" else args.max_redraw_ms
        if budget is not None and row["p95_ms"] > budget:
            failed.append(f"{case}: p95 {row['p95_ms']:.3f} ms exceeds {budget:.3f} ms")
    if failed:
        raise SystemExit("\n".join(failed))


if __name__ == "__main__":
    main()
