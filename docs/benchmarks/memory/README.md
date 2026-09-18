# Retained memory

Measured on macOS 26.6.2 / Apple Silicon, 2026-09-17, using release builds.

## Changes

- Keep one immutable copy of each installed macOS system font for the process lifetime. Borrow those bytes when registering fonts with egui. Previously egui cloned the owned font bytes when constructing font faces. The same system fonts, fallback fonts, sizes, and weights remain in use; no new dependencies or unsafe code were added.
- Drop hidden terminals' viewport snapshots, paint commands, and glyph layouts. Rebuild these caches when the tab becomes visible. Keep the emulator, 2,000-line scrollback, selection, and incoming output. The active tab still reuses its drawing caches.

## Retained Rust heap

The benchmark renders and tessellates the real application UI at 1280 × 800, uses the real ANSI parser with in-memory terminals, fills each terminal's scrollback, switches between five tabs, closes them, and repeats three times. The allocator tracks successful allocations, resizing, and deallocation. Every frame's output is dropped before sampling.

| Workload | Before | After | Saved |
| --- | ---: | ---: | ---: |
| Empty window | 16.03 MiB | 8.28 MiB | 7.76 MiB |
| One terminal with full history | 24.54 MiB | 16.78 MiB | 7.76 MiB |
| Five terminals with full history | 57.94 MiB | 48.83 MiB | 9.12 MiB |
| All tabs closed after first cycle | 16.40 MiB | 8.64 MiB | 7.76 MiB |

These are **live Rust allocation bytes**, not RSS or Activity Monitor physical footprint. They exclude allocator overhead, native frameworks, graphics buffers, PTY processes, and network state. The two system-font byte buffers intentionally remain allocated until process exit. The benchmark's report also retains a small amount of metadata per sample.

After closing all tabs, the second and third cycles add only 5.35 KiB compared with the first closed-tab sample, including the benchmark's growing results list. This checks retention in the tested workload; it does not prove the absence of leaks in every production workload or explain the historical 388.9 MiB process peak.

[All stages](comparison.md), [before](before.json), [after](after.json), [before source hashes](before-source.sha256), [after source hashes](after-source.sha256).

```sh
cargo run --release --locked --features terminal-fixture --example memory_bench -- /tmp/relay-memory.json
python3 scripts/check_memory_bench.py docs/benchmarks/memory/before.json /tmp/relay-memory.json --min-saved-mib 7 --max-closed-growth-kib 64
```

The savings threshold is specific to the installed macOS fonts used in this run. Use appropriate budgets on other systems. The growth check remains useful independently of font size.

## Native footprint

**Correction (2026-09-17):** the historical native measurements below used a fixture whose startup zoom briefly produced a 12,500 × 12,500-point layout. They should not be used as representative native footprints or reliable before/after savings. [The follow-up investigation](../ghostty/memory-investigation.md) fixes the fixture and records measurements at verified window dimensions. The deterministic live-Rust-heap measurements above use explicit input dimensions and are unaffected.

`measure_native_memory.py` launches only isolated local `visual_preview` windows, waits 15 seconds, then takes three `vmmap` samples per state. It terminates its own fixture after sampling. It never attaches to the user's Relay process or connects to SSH. The fixture uses its existing 0.8 zoom/window scaling; the CLI viewport is 1280 × 800 in both runs. No framebuffer screenshots are requested during measurement.

[Native before](native-before.json) and [native after](native-after.json) record physical footprint and high-water footprint, with binary hashes and sampling settings. Samples within each state come from the same process, not independent launches. Native memory includes platform and graphics allocations and can vary with driver caching, compression, and window state; savings in live heap do not translate one-for-one into physical footprint.

```sh
cargo build --release --locked --example visual_preview
python3 scripts/measure_native_memory.py target/release/examples/visual_preview /tmp/relay-native-memory.json
```

Median samples from this run (one launch per state):

| Native fixture | Before footprint (MiB) | After footprint (MiB) | Before peak (MiB) | After peak (MiB) |
| --- | ---: | ---: | ---: | ---: |
| empty | 65.3 | 63.6 | 302.3 | 293.6 |
| workspace | 112.8 | 114.9 | 424.3 | 416.6 |
| terminals | 251.1 | 233.8 | 548.1 | 537.1 |

The one-terminal workspace footprint increased by 2.1 MiB despite the deterministic heap reduction; graphics/framework and compression differences can mask the saving. Startup high-water marks fell by roughly 8–11 MiB. These native observations are not a repeat-launch statistical result or a guarantee for existing long-running SSH sessions.

Run before and after binaries sequentially on the same desktop, with matching window dimensions and sampling settings. This requires desktop/process-inspection access on macOS.

## Validation

- 30 application tests and 16 terminal tests pass. Tests verify shared font storage and identical tessellated text, released hidden-tab drawing allocations, and restored history, selection, and hidden output.
- Both native PTY tests pass, including real tmux mouse scrolling.
- Clippy passes for the application/examples and vendored terminal widget.
- All 48 existing terminal CPU/allocation cases pass the existing budgets: idle p95 ≤ 0.25 ms, active p95 ≤ 1 ms, requested allocation volume ≤ 2,000,000 bytes per operation. [Measured CPU results](terminal-after.json). Cache rebuilding on tab activation is the memory/CPU tradeoff; these CPU cases do not separately time tab activation.

```sh
cargo test --locked --features terminal-fixture
cargo test --locked -p egui_term
cargo test --locked --test tmux_scroll -- --ignored --nocapture
cargo clippy --locked --all-targets --features terminal-fixture -- -D warnings
cargo clippy --locked -p egui_term --all-targets -- -D warnings
```
