# Optional Ghostty prototype (macOS)

Relay can host a native Ghostty terminal in its center pane. The existing egui_term / Alacritty terminal remains the default. Enabling Ghostty requires both a Cargo feature and a runtime flag; saved connections and the SFTP file browser use the same application code.

## Build and try

Run from the repository root on macOS with Rust, Python 3.12+, Git, Xcode Command Line Tools, and Relay's existing OpenSSL dependencies:

```sh
python3 scripts/build_ghostty.py
cargo run --release --locked --features ghostty -- --ghostty-demo
```

The demo opens a local `/bin/zsh -f` terminal. The status bar says **Ghostty prototype** and includes **Local terminal** to open another test tab. Use a saved connection's **Open terminal** button to try SSH with Ghostty. Nothing connects automatically.

To open the prototype without a local shell:

```sh
cargo run --release --locked --features ghostty -- --ghostty
```

To return to the standard build:

```sh
cargo run --release --locked
```

An already-built feature-enabled executable also uses the standard terminal when launched without `--ghostty`, `--ghostty-demo`, or `RELAY_GHOSTTY=1`. Unset that environment variable when comparing backends.

## Integration

- `src/ghostty.rs` selects the backend and keeps native resources on the UI thread. Each terminal retains the Ghostty runtime until its surface is freed. Only the repaint callback crosses threads.
- `native/ghostty_bridge.m` hosts a Metal-backed `NSView` inside the existing egui window. It routes keyboard, text composition, mouse selection, clipboard, resize, focus, and wheel events to Ghostty. Clicking surrounding Relay controls returns keyboard focus to egui.
- Tabs retain final output after their child process exits. Relay's tab-close button owns destruction. Copying remains available in ended tabs.
- Hidden tabs and file previews hide/occlude the native surface, including tabs that have never been shown. Hidden Metal frame buffers are compacted; showing the tab recreates them from retained terminal/font state. Opening New Connection temporarily hides it so the egui modal can appear above it; focus returns when the surface is shown again.
- The terminal uses Relay's light palette and Menlo. The standalone Ghostty application's configuration is not loaded. Explicit Cmd+C / Cmd+V work; remote clipboard reads and clipboard writes needing consent are denied in this prototype.
- SSH still runs through the existing OpenSSH command and profile validation. Each argument is quoted independently before passing it to Ghostty's command-string API. SFTP authentication and streaming previews are unchanged.
- Existing Relay diagnostics record startup, terminal lifecycle, and exits. Ghostty child PIDs are unavailable through this adapter and appear as `null`. Native Ghostty failures are not all captured by Relay's Rust panic logger.

## Pinned dependency and local build changes

The full embedding API is pinned to [Ghostty v1.3.1, commit 332b2aef](https://github.com/ghostty-org/ghostty/tree/332b2aefc6e72d363aa93ab6ecfc86eeeeb5ed28). This is Ghostty's application embedding API, **not a stable drop-in terminal widget API**. The separate [libghostty-vt API](https://github.com/ghostty-org/ghostty/tree/332b2aefc6e72d363aa93ab6ecfc86eeeeb5ed28/include/ghostty) handles terminal state; it does not provide this native renderer. Upgrades require reviewing the bridge against the pinned header.

The build script downloads and checksum-verifies Zig 0.15.2, clones the exact Ghostty release, and keeps the compiler, source, and caches under ignored `target/ghostty/`. It makes reproducible, narrowly matched changes in that checkout:

1. Install the native static library/resources without building an XCFramework or iOS targets.
2. Compile the original Metal shader source at runtime, avoiding the full Xcode `metal` tool requirement.
3. Combine archives with Zig's LLVM archiver. Apple's installed `libtool` omitted some Zig object members after alignment warnings.
4. Use an installed SDK 15 when SDK 26 has incompatible arm64e-only top-level libSystem stubs. `RELAY_GHOSTTY_SDK` can select an installed compatible SDK. The build uses a local `xcrun` shim and never changes `xcode-select`.
5. Compact hidden Metal frame state after waiting for in-flight draws, and force a fresh render on restoration. This is a Relay-specific renderer change, separate from the build-system adaptations above.

The library targets macOS 13+. It was built and exercised on Apple Silicon / macOS 26.6.2; Intel and other macOS versions have not been validated. Full Xcode is not installed on the test machine. Runtime shader compilation contributes startup work; a packaged implementation should investigate precompiled Metal libraries.

Source builds find resources in this checkout's `target/ghostty/source/zig-out/share/ghostty`. Packaged builds find them beside the executable inside `Relay.app/Contents/Resources`, and include OpenSSL in `Contents/Frameworks`. The packaged app selects Ghostty by default; `--standard-terminal` selects the fallback. See [release packaging](releasing.md). Apple Developer ID signing/notarization, accessibility, complete IME/key-layout coverage, drag-and-drop into terminals, and all standalone Ghostty actions remain outside this release.

## Validation

The native smoke fixture uses disposable local PTYs and an isolated tmux socket. It does not connect to SSH or touch an existing tmux server. Clipboard contents and formats are saved and restored around the shortcut check.

```sh
cargo run --release --locked --features ghostty,terminal-fixture --example ghostty_smoke
cargo test --locked --features ghostty,terminal-fixture
cargo clippy --locked --all-targets --features ghostty,terminal-fixture -- -D warnings
```

The smoke test requires `tmux` on PATH and a macOS desktop session. It passed native text/Unicode input, Cmd+C/V, copying after exit, focus handoff to the surrounding UI, modal visibility, switching tabs, resize, retained exit output, scrolling two different tmux panes, and destroying all surfaces. The feature-enabled application suite passed 31 tests (two fixture tests are ignored by default), and Clippy passed for all targets. These are programmatically driven native checks. Window capture was unavailable on this machine, so no screenshot-based visual validation is claimed. Live remote SSH and SFTP were not used by this fixture.

The standard deterministic CPU/heap benchmark examples continue to use the existing in-memory Alacritty fixture, even in feature-enabled builds. They are not Ghostty performance benchmarks.

## Memory and size

The [rendering resource optimization report](benchmarks/ghostty/resource-optimization.md) covers hidden-tab cleanup, visited-tab measurements, and the rejected Metal UI experiment. The visited-five-tab comparison fell from 406 MiB to 353 MiB; a large shared graphics footprint remains. These measurements preserve the UI and history settings.

The original 531 MiB versus 301 MiB comparison was inflated by a fixture startup-zoom bug that created a 12,500 × 12,500-point layout. It is withdrawn as a representative five-terminal comparison. See [the corrected memory investigation](benchmarks/ghostty/memory-investigation.md) for verified window dimensions, tab-count/history comparisons, and raw measurements. Results remain specific to this bridge/build and are not measurements of the standalone Ghostty app.

The release binary with both backends is approximately 19 MiB versus approximately 7.7 MiB for the standard build. The static archive, Zig compiler, source, build caches, native framework memory, and child processes are separate costs. Ghostty history is capped at 10,000,000 bytes per terminal; the standard terminal retains 2,000 lines, so their retention limits are not equivalent.
