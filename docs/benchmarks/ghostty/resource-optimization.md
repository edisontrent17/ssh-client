# Ghostty rendering resource optimization — 2026-09-17

The optional macOS bridge now marks a newly created hidden surface as occluded in Ghostty, as well as hiding its NSView. Previously, `hide()` returned early because the NSView was already hidden, leaving Ghostty's renderer internally visible. This caused unused tabs to render and allocate full frame buffers.

The pinned Ghostty build also compacts Metal frame state when a surface becomes hidden. On the renderer thread, under the drawing mutex, it creates minimal replacement frame state, waits for the old GPU frames to complete, and releases their textures and buffers. Showing the surface forces a redraw and uploads the retained CPU font/terminal data. PTYs, terminal contents, history limits, font, and palette are unchanged. Switching tabs now involves recreating rendering resources; switching latency has not been statistically benchmarked.

Native bridge initialization, ticking, and destruction have explicit autorelease pools so temporary Cocoa objects are released at those operation boundaries. No independent footprint saving is claimed for the pools.

## Measurements

All cases use disposable local PTYs, 2,100 colored/Unicode output lines per tab, a 1,024 × 640 native window (1,280 × 800 egui points), release builds without `terminal-fixture`, and sampling 15 seconds after construction. The fixture requests an always-on-top window and periodic repainting. These are observations on this Mac, not a memory budget for every session. Child processes are excluded.

| Case | Physical footprint |
| --- | ---: |
| Original bridge, five tabs | 386.8–401.0 MiB |
| Initial occlusion fix alone, five tabs | 328.8 MiB |
| Occlusion + autorelease pools, five tabs | 338.0 MiB |
| Same fixes after displaying **every** tab, before compaction | 406.0 MiB |
| With Metal frame compaction, after displaying **every** tab (two launches) | 346.7–353.3 MiB |

The direct visited-tab comparison saves **52.7–59.3 MiB (13–15%)** across the two optimized launches versus the 406 MiB observation. These are not confidence intervals. In the first optimized run, the `IOAccelerator (graphics)` resident category falls to 20.3 MiB, and `IOSurface` to 17.7 MiB. Approximately 235 MiB of dirty/compressed `owned unmapped (graphics)` remains. The large shared graphics cost is unresolved; this is a reduction in per-tab rendering retention, not a claim that the entire footprint is now small.

The initial occlusion fix alone was insufficient: once each tab was displayed, memory returned to about 406 MiB. `--visit-all-tabs` was added specifically to expose this distinction. Reports record `all_tabs_visited`; do not interpret a run without that confirmation as a visited-tab result.

Raw data:

- [Original corrected baseline](production-profile/results.json) and [repeat](resource-baseline-repeat/results.json).
- [Initial occlusion fix](visibility-fix-profile/results.json) and [native pool cleanup](resource-fix-profile/results.json).
- [Visited tabs before compaction](resource-visited-profile/results.json).
- [Visited tabs with compaction](compact-metal-profile/results.json).
- [Repeated compacted-tab run, including verified closure](compact-metal-repeat/results.json).

Each directory includes process logs and complete `vmmap` summaries. The first compaction run's scheduled closure was **not observed**, so its later 339.9 MiB reading is not a closed-tab result. The repeat confirmed closing all five tabs and measured 303.5 MiB afterward, with approximately 11 MiB live Rust allocations. This illustrates the shared graphics residency that can remain after terminal state is gone. The native smoke test also passed surface destruction.

macOS graphics residency varies substantially: an empty-window observation was 298 MiB while a one-terminal observation was 82 MiB after driver resources were reclaimed. Do not subtract those unmatched samples to calculate a per-tab cost or extrapolate the reduction to all workloads.

## Rejected experiment

Using wgpu/Metal for the surrounding egui UI increased the five-terminal footprint to 491 MiB and the empty window to 449 MiB on this machine. That dependency/configuration change was removed. Relay continues to use its existing OpenGL UI with a native Metal Ghostty surface. [Experiment data](metal-profile/results.json).

An initial compaction experiment did not match the canonical Metal type and therefore did not execute the cleanup. Its [unchanged footprint](compact-visited-profile/results.json) is retained for traceability; the final patch uses `renderer.Metal`.

## Reproduce

```sh
python3 scripts/build_ghostty.py
cargo build --release --locked --features ghostty --example visual_preview
python3 scripts/profile_terminal_memory.py target/release/examples/visual_preview /tmp/relay-visited-memory --engines ghostty --counts 5 --skip-empty-history --visit-all-tabs
cargo run --release --locked --features ghostty,terminal-fixture --example ghostty_smoke
```

The smoke fixture passed native input, clipboard, focus, modals, exits, tmux scrolling, repeated tab switching, and surface destruction. Its new pixel probe reads the completed IOSurface frame and verifies nonblank text after restoration, alongside terminal-model checks. The fixture now requests an always-on-top window: an earlier run paused in the idle macOS event loop and timed out when covered. With the visible fixture, all phases passed. This is a native pixel check, not a screenshot-based visual review.

Validation also passed 31 application tests with Ghostty/test features, 29 default-build tests, the startup-viewport regression, formatting, Python syntax checks, and Clippy with Ghostty/test features. The real SSH/SFTP integration fixtures were not run for this renderer change. The optimized executable is `target/ghostty/relay-ghostty-optimized` (launch with `--ghostty` or `--ghostty-demo`); `target/release/ssh-client` was restored to the standard build. Existing user sessions were not restarted.
