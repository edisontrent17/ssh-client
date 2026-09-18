# Relay SSH

A native Rust SSH client prototype for Windows, macOS, and Linux, built around a simple connection list, terminal tabs, and an SFTP file tree.

## Install on macOS

For Apple Silicon Macs running macOS 13 or later:

```sh
brew tap edisontrent17/tap
brew install --cask edisontrent17/tap/relay-ssh
open -a Relay
```

The app includes the Ghostty terminal, runtime resources, and OpenSSL. Rust and Zig are not required to install it. You can also download the app from [GitHub Releases](https://github.com/edisontrent17/ssh-client/releases). No SSH connection opens automatically.

This is an early, **unnotarized** release. If macOS blocks the first launch, use **System Settings → Privacy & Security → Open Anyway** after attempting to open Relay. Updates use `brew upgrade --cask edisontrent17/tap/relay-ssh`. Intel Macs, Windows, and Linux do not have prebuilt packages in this release.

The packaged app defaults to Ghostty. To try the standard terminal, launch `/Applications/Relay.app/Contents/MacOS/relay --standard-terminal`. [Packaging and release instructions](docs/releasing.md).

## Implemented

- Save, find, edit, and remove connection profiles locally.
- Open multiple real OpenSSH terminals with color, selection, copy/paste, resize, and 2,000 lines of scrollback per session.
- Browse remote directories in an expandable tree. Select a folder as the upload destination.
- Select a remote file to read its contents in the center pane. Preview reads 64 KiB ranges on demand over SFTP and keeps only the current range in memory, without creating a local file.
- Drag files or folders from the desktop into the app while the Files panel is open, or use the Upload button for files.
- Download a selected regular file through a native save dialog.
- Stream transfers through a 64 KiB buffer with progress and cancellation. Existing files are never intentionally replaced. Uploads use temporary remote files; downloads use temporary local files.
- Authenticate SFTP with an SSH agent, private key, or password. Passwords/passphrases remain in memory and are cleared after successful authentication.
- Verify host keys before SFTP authentication. Unknown keys require explicit session trust; known-key mismatches are blocked. Terminal authentication and host-key handling use OpenSSH.

## Build and run

### macOS quick start

Install the Xcode command-line tools if they are not already installed:

```sh
xcode-select --install
```

With Homebrew installed, install the build dependencies:

```sh
brew install rust pkg-config openssl@3
```

Clone this repository using your GitHub account, then open a terminal in the cloned repository and run:

```sh
cargo run --release --locked
```

This builds for your Mac's architecture and opens the native app. OpenSSL is normally discovered automatically from Homebrew. If compilation reports that it cannot locate OpenSSL, run `brew --prefix openssl@3` to find its actual absolute installation path, then set `OPENSSL_DIR` to that returned path before rebuilding. Your Mac's checkout and Homebrew paths have not been assumed here.

An [optional macOS Ghostty prototype](docs/ghostty-prototype.md) embeds a native Metal terminal in the center pane. It requires a separate build step and opt-in launch flag. The normal command above continues to use the existing terminal. It is available for compatibility and usability testing; see the linked notes for measured memory use and prototype limitations.

On the first run, choose **+ New connection**, enter a hostname or SSH alias in the modal, and choose **Save connection**. Choose **Open terminal** for the saved host. Use **Connect files** separately for the remote tree and file transfers in the right panel. Agent/key authentication is attempted first; a password or key passphrase can be entered in the Files panel when required.

### All platforms

Requires Rust 1.92 or newer and OpenSSH available on the executable search path. Linux requires a graphical desktop with OpenGL and X11 or Wayland libraries. Building on Debian/Ubuntu requires `build-essential`, `pkg-config`, and `libssl-dev`; X11 execution also needs `libxkbcommon-x11-0`. File dialogs use the desktop portal or Zenity. On macOS, install Xcode command-line tools and OpenSSL development libraries. Windows builds require the MSVC build tools and the Windows OpenSSH client.

Run build commands from the repository root. The app uses platform-specific configuration directories; hover over **Local storage** to see the paths on your machine. No servers connect automatically at startup.

## Usage

The desktop uses a macOS-style light appearance, including the terminal, with blue primary actions. Connections stay on the left and remote files on the right.

1. Choose **+ New connection** to open the modal. Enter a host or SSH alias, optionally a username, port, and absolute private-key path. Blank username and port preserve SSH config defaults.
2. Choose **Save connection**, then **Open terminal**. Complete any authentication prompt in that terminal. Select a saved host and choose **Edit…** to edit its details in the same modal. **Cancel** or Escape dismisses the new connection modal without saving.
3. Choose **Connect files** to establish the separate SFTP connection. If agent/key authentication is unavailable, enter the password or key passphrase in the Files panel. Verify an unknown fingerprint before trusting it.
4. Expand folders using their arrows; click a folder name to choose an upload destination. Use **Refresh** above the file tree to reload the root and all expanded folders when remote files change. **Search loaded files…** filters file and folder names already loaded in the current tree, ignoring case. Matching descendants appear with their parent folders. Folder arrows still work during search: expanding shows the folder's contents, reading its directory listing only if it is not already loaded. Typing a search makes no remote requests. **Clear** restores the previous expansion state. Drop local files/folders onto the app with the Files panel visible.
5. Select a regular file to preview it in the center pane. **Previous** and **Next** fetch another 64 KiB range; **Reload** reopens the file from the beginning. Text is selectable and read only; binary content appears as hexadecimal bytes. Select a terminal tab to return to your session.
6. To save a local copy, choose **Download** and pick a destination that does not already exist.

Copy/paste uses Ctrl+Shift+C / Ctrl+Shift+V on Linux and Windows, and Cmd+C / Cmd+V on macOS. Closing a terminal tab ends its SSH session. An exited session remains visible until closed so errors can be read.

Mouse-wheel and trackpad scrolling are forwarded to terminal applications that enable mouse reporting, including tmux with `mouse on`. In tmux, scroll over the pane whose history you want to view.

## Verification

The optional local integration check `cargo test --locked --test tmux_scroll -- --ignored --nocapture` validates wheel scrolling in two panes using a temporary tmux server and PTY. It requires tmux and does not use your existing server or configuration.

```sh
cargo test --locked
python3 scripts/test_sftp.py
```

The integration script needs OpenSSH server tools and permission to bind a loopback socket. It creates disposable keys and data in an automatically generated absolute temporary directory and stops its server afterward. It checks unknown-host trust, binary roundtrips, nested folders, no-overwrite behavior, and cancellation cleanup.

## Local diagnostics

SFTP sends keepalives during idle periods (30-second interval). Socket failures and transport timeouts discard the failed session and cached preview handle, and mark the file browser disconnected. **Reconnect files** in the preview reconnects its original host and resumes the same file/byte offset after authentication and host verification. Passwords/passphrases may need to be entered again. Uploads are never automatically replayed. Transport diagnostics include the numeric libssh2 session error code.

SFTP requests log their operation type, worker/request IDs, elapsed time, and outcome. Failures log categories such as permission denied, timeout, missing path, or connection failure; server-provided error text and remote paths stay out of the logs. A failed folder shows its error and a **Retry** button, and a refresh failure preserves previously loaded entries. **Disconnect** stays available during a pending operation so the file browser can be reconnected. A stopped file worker clears the busy state and reports that reconnection is needed.

The native failure/retry mock is available with `cargo run --release --locked --example visual_preview -- --interactive 1440 978 directory-error`. It starts with a simulated permission error; Retry loads an empty folder from memory.

The app writes local JSON-line diagnostics for startup/shutdown, window-close requests, terminal starts, explicit tab closes, SSH exit statuses, and Rust panics on any thread. Hover over **Local storage** for the current log path. Files live in the application's local data directory under `diagnostics`; each run has its own private log. Up to eight run logs are retained at startup, with a 1 MiB limit per log; reaching the limit replaces that run's older records.

Terminal input/output, connection profiles, hosts, usernames, file contents, passwords, and keys are not written to these logs. Panic records contain thread name, source location, and a bounded backtrace; panic message payloads are omitted because they can contain sensitive data. Logs are not uploaded. Diagnostic disk-write failures are best-effort and must not stop a terminal session. Native crashes or forced process termination may prevent a final record from being written; a Rust panic hook cannot capture every kind of process failure.

An exited SSH session remains visible with `ended` in its tab label. Its exit status is shown in the status bar when available, and final PTY output is drained before the session finishes.

## Current limits

This is a working prototype. The first packaged release targets Apple Silicon macOS; source builds remain available for other platforms. GitHub Actions validates standard builds on macOS and Linux. The optional Ghostty bridge and native interaction fixtures need a macOS desktop session and are validated separately.

- SFTP supports direct connections. SSH config aliases resolve through OpenSSH, but proxy/jump connections are terminal-only for now.
- SFTP checks the standard user known-hosts file and, on Unix, `/etc/ssh/ssh_known_hosts`; custom known-hosts file directives and host certificates are not yet supported.
- SFTP keyboard-interactive/MFA, hardware-backed keys, and complex SSH configuration options need further implementation. The terminal delegates these capabilities to installed OpenSSH.
- Drag-and-drop currently uploads from the operating system. Download uses a native dialog; dragging a remote file out to the desktop is not implemented.
- Folder downloads, overwrite confirmation, resume, remote editing, rename/delete, and transfer concurrency are not implemented. File operations run serially; terminal sessions remain independent.
- Symlinks and special files are not transferred. Cancellation is checked between I/O chunks and may wait for the network timeout; completed files from a cancelled folder upload remain.
- The terminal widget is an upstream prototype; compatibility with complex terminal applications, IME, accessibility, and native OS behavior needs broader testing.

## Footprint approach

The file tree caches local search results and lays out only rows inside the scroll viewport. See [remote tree benchmarks and the interactive mock](docs/benchmarks/README.md) for before/after measurements with 1,000, 10,000, and 100,000 files, plus commands to rerun the checks.

The terminal caches visible cells and glyph layouts, shares palettes and default bindings, and coalesces output notifications for the active tab. See [terminal benchmarks and the five-tab local mock](docs/benchmarks/terminal/README.md) for CPU/allocation measurements, regression checks, and validation commands.

System font bytes are shared across font faces on macOS. Hidden terminal tabs release drawing caches while preserving their full 2,000-line scrollback. See [retained-memory benchmarks](docs/benchmarks/memory/README.md) for measured savings, repeated tab-close checks, and native macOS footprint measurements.

The UI uses egui with OpenGL rendering, and re-renders in response to events. There is no bundled browser or JavaScript runtime. Only selected terminal tabs are drawn. SFTP operations run off the UI thread, use bounded transfer buffers, and publish progress at most ten times per second during copying. Terminal history is capped.

Executable size, total installed dependencies, process memory, startup time, and transfer throughput are separate measurements. Check actual release artifacts before setting budgets. Linux dynamic OpenSSL and graphics libraries, the installed OpenSSH executable, and driver memory are not included in the application executable size.

The terminal widget source and MIT license are preserved in `vendor/egui_term`. Provenance and local modifications are recorded in `vendor/egui_term/RELAY_CHANGES.md`.

## Historical Linux validation

The Linux release executable measured 11,540,112 bytes (11.01 MiB). In one Xvfb / Mesa software-OpenGL run with no SSH sessions, the window became visible in 218 ms and the process used approximately 121.8 MiB RSS after one second. The subsequent two-second idle sample consumed no measurable CPU ticks. These are environment-specific observations, not production performance guarantees; memory and startup still need profiling on real Windows, macOS, and Linux desktops. Raw measurements are in `verification.json`.

Six application tests, one terminal-paste test, and the real SFTP integration test passed. Formatting and Clippy passed. The Linux window, fingerprint prompt, file tree, terminal keyboard focus, and session-exit display were inspected on a virtual display. OS-level drag-and-drop needs further native-platform testing; the raw file-drop event route and real transfer backend are tested independently.
