# Native Ghostty prototype comparison

Latest: [hidden-tab rendering resource optimizations](resource-optimization.md), including measurements after visiting every tab.

**Correction (2026-09-17): the original figures below are not representative of the intended window size.** The fixture queued a zoom change before its first input pass. egui scaled its default 10,000-point placeholder viewport to 12,500 × 12,500 points, creating oversized terminal grids and rendering targets before the real window dimensions took effect. The 301/531 MiB comparison is therefore withdrawn as a normal five-terminal comparison. See [the corrected investigation](memory-investigation.md).

Measured on Apple Silicon / macOS 26.6.2, 2026-09-17. Both runs use the **same release `visual_preview` binary**, with Ghostty enabled only in the second run. This tests local terminal fixtures inside Relay, not standalone Ghostty or a remote SSH workload.

| Fixture | Standard footprint | Ghostty footprint | Standard peak | Ghostty peak |
| --- | ---: | ---: | ---: | ---: |
| Empty window | 63.6 MiB | 55.7 MiB | 292.2 MiB | 290.6 MiB |
| One terminal with mock file tree | 176.8 MiB | 548.0 MiB | 412.1 MiB | 1,433.6 MiB |
| Five terminals with generated history | 301.1 MiB | 530.6 MiB | 536.3 MiB | 937.3 MiB |

These are medians of three `vmmap` samples after a 15-second settling interval, two seconds apart. There is one independent process launch per state, not three independent runs. Results are sensitive to native graphics/compiler caching, compression, and window state. In particular, the larger one-terminal result than five-terminal result shows why these numbers should not be used to extrapolate a per-tab cost. This prototype compiles Metal shaders at runtime; profiling is needed to separate compiler/driver retention from terminal memory.

The fixtures receive identical shell commands and output, with CLI dimensions 1280 × 800 and the fixture's existing 0.8 UI scaling. Each of five terminals prints 2,100 colored Unicode lines; one tab is visible. Native font metrics, wrapping, and history policies differ: Ghostty uses Menlo 14 and a 10,000,000-byte history cap, while the standard widget keeps 2,000 lines. This is a comparison of the two prototype configurations, not a controlled comparison of equal terminal grids/history allocations.

The empty-window difference should be treated as measurement variability, not a Ghostty memory saving. These original measurements cannot establish the normal memory cost of adopting Ghostty. The feature remains opt-in.

Raw reports: [standard](standard.json), [Ghostty](ghostty.json). They include matching executable hashes and sampling settings. Physical footprint includes native and graphics allocations attributed to Relay; it is not live Rust heap, and separate shell/tmux child processes are not included. The final keyboard-focus/copy fixes landed after this measurement and do not have a separate memory measurement.

```sh
python3 scripts/build_ghostty.py
cargo build --release --locked --features ghostty,terminal-fixture --example visual_preview
env -u RELAY_GHOSTTY python3 scripts/measure_native_memory.py target/release/examples/visual_preview /tmp/relay-standard.json
RELAY_GHOSTTY=1 python3 scripts/measure_native_memory.py target/release/examples/visual_preview /tmp/relay-ghostty.json
```

Run sequentially on the same desktop. The measurement script launches and terminates only its own fixtures. It requires macOS GUI/process-inspection access. Existing deterministic `terminal_bench` and `memory_bench` results remain specific to the standard backend.
