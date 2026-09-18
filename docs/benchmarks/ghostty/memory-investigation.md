# Why the original five-terminal measurements were high

Follow-up: [implemented rendering resource optimizations and validation](resource-optimization.md).

The earlier **301 MiB standard / 531 MiB Ghostty** comparison mixed an oversized startup layout with native graphics residency. It is withdrawn as a representative measurement of five normal terminal panes. This investigation changes the local fixture and measurement tools; it does not change the terminal engines or the running user's sessions.

## Confirmed sizing bug

`visual_preview` called `Context::set_zoom_factor(0.8)` before egui's first input pass. In egui 0.35, a queued zoom transition rescales the previous input viewport and overrides the incoming screen rectangle. Before the first pass, that previous viewport defaults to 10,000 × 10,000 points. The result was a **12,500 × 12,500-point first layout** despite a requested 1,024 × 640-point native window (1,280 × 800 egui points at 0.8 zoom).

[Captured geometry and live allocations](oversized-viewport.log) confirm this. A symbolized macOS allocation trace also attributed tens of MiB to `alacritty_terminal::grid::resize`, called by `TerminalBackend::process_command` from the native UI. The trace included about 54 MiB of row-resize allocations in a one-terminal run, plus large drawing snapshots. This establishes an oversized-grid allocation problem, not just a hypothesis about GPU or shader overhead.

The fixture now sets its initial zoom option directly before the first pass, preserving the real incoming screen dimensions. A regression test verifies that the first layout stays at 1,280 × 800. Profiling mode also checks the layout against the native viewport before drawing. Runtime zoom behavior in the normal app is unchanged; the normal app did not use this fixture's startup zoom call.

With the same five-tab, 2,100-line workload and test features, live Rust allocations fell from roughly **169 MiB to 55 MiB** after fixing the layout. They fell to approximately **11 MiB** after the fixture confirmed all tabs closed. These are live Rust allocations, not the whole process footprint.

## What actually scales with tabs

The corrected first-pass scaling run used one visible tab and generated identical colored/Unicode output in every terminal. [Raw results and full vmmap summaries](tab-profile/results.json) contain zero through five tabs. Sampling was five seconds after fixture construction. The table below concerns Rust allocations in the standard terminal, which includes its parser and terminal grids:

| Tabs | Live Rust heap |
| ---: | ---: |
| 0 | 11.1 MiB |
| 1 | 23.3 MiB |
| 2 | 33.3 MiB |
| 3 | 39.4 MiB |
| 4 | 47.3 MiB |
| 5 | 55.3 MiB |
| 5, without generated history | 32.8 MiB |
| After closing all 5 | 11.2 MiB |

That build included `terminal-fixture`, which adds an unused in-memory test parser to each real standard backend. The final native comparison therefore builds **with `ghostty` only**. The allocation counter lives in the example itself and does not require test-only terminal state. Ghostty's terminal state is allocated by native/Zig code and is **not counted by the Rust allocator**, so its approximately 11 MiB Rust number is not its terminal-memory footprint.

Each real tab still owns parser/session state, a terminal grid, history, and I/O state. Ghostty also retains per-surface rendering resources while hidden. In the corrected five-tab Ghostty sample, `vmmap` attributed about 55 MiB to `IOAccelerator (graphics)` and 30 MiB to `IOSurface`, versus roughly 10 and 13 MiB in an empty window. These are process-level categories, not a precise attribution of every byte to Ghostty or to each tab. Reducing unnecessary hidden-surface resources remains an optimization target.

## The large fixed graphics component

In the visible-window run, the empty Relay window already had approximately **226 MiB** in the macOS `owned unmapped (graphics)` category. That cost existed before opening any terminal. The font atlas was only 16,384 × 32 pixels, about 2 MiB including the other small UI textures. Font-atlas size alone therefore did not explain the large graphics category.

Those graphics allocations are charged to Relay's physical footprint. They are distinct from Rust heap, and their residency varies with window visibility, repainting, time, and driver behavior. For example, one confirmed closed-tab run dropped to 75 MiB physical footprint while another remained above 300 MiB with approximately 11 MiB live Rust heap. This is why a single whole-process reading cannot be divided by five to derive a per-terminal cost. The exact lifetime and source of all driver allocations still need graphics profiling; the measurements do not prove that runtime Metal shader compilation is the dominant cause.

## Reproduce

```sh
python3 scripts/build_ghostty.py
cargo build --release --locked --features ghostty --example visual_preview
python3 scripts/profile_terminal_memory.py target/release/examples/visual_preview /tmp/relay-terminal-profile --counts 0 1 2 3 4 5 --settle-seconds 15
```

The script starts only disposable local shells, never SSH, and saves raw `vmmap` summaries plus live-Rust-heap and texture counters. A profiling fixture requests an always-on-top window and a one-second repaint wakeup to reduce occlusion effects. Consequently this is a visible, periodically repainted workload; it should not be compared directly with an idle, occluded user window. There is one process per configuration, not a statistical distribution of repeated launches.

Five-tab cases also request closing all tabs and sample again. If macOS suppresses UI frames and the close marker is missing, the sample is explicitly marked `close_not_observed`; it must not be interpreted as a leak or as successfully closed tabs. The preliminary runs in `tab-profile` and `settled-profile` stopped when that verification failed. Their completed open samples remain observations; they do not contain a verified five-tab Ghostty closure result.

The no-test-parser run is recorded in [production-profile/results.json](production-profile/results.json). It uses the same engine configuration as the optional production build, with profiling counters in the example and the visibility/repaint differences described above. One sample per state, 15 seconds after construction:

| Backend | Empty footprint | Five terminals, 2,100 lines each | Five-terminal live Rust heap |
| --- | ---: | ---: | ---: |
| Standard | 68.3 MiB | 90.4 MiB | 44.2 MiB |
| Ghostty | 68.5 MiB | 401.0 MiB | 11.1 MiB, excludes native/Zig allocations |

The Ghostty case still has substantial graphics overhead: approximately 50.3 MiB dirty/compressed `IOAccelerator (graphics)`, 29.5 MiB `IOSurface`, and 233.8 MiB `owned unmapped (graphics)`, about **314 MiB combined**. Its native malloc zones report approximately 48 MiB allocated, and Memory Tag 240 about 10 MiB dirty/compressed. These VM categories do not identify every allocation's owning component, but they clearly locate most of the remaining footprint in graphics resources. A smaller history limit cannot remove that graphics cost.

The standard run retained much less driver memory than the earlier continuously visible observations, despite the same requested profiling window behavior. Treat these as observations of variable graphics residency, not a guaranteed 90 MiB/401 MiB budget or an apples-to-apples claimed optimization percentage. The normal prototype's graphics resource lifecycle, including hidden surfaces, remains a profiling/optimization target. Both final scheduled close samples were explicitly `close_not_observed`, with `close_started=false`; they establish no post-close result.

Validation: the startup-viewport regression test and Clippy with both default and Ghostty/test features pass. The normal app was not restarted or modified by this investigation.
