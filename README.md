# Relay SSH

A native Rust SSH client prototype for Windows, macOS, and Linux, built around a simple connection list, terminal tabs, and an SFTP file tree.

## Implemented

- Save, find, edit, and remove connection profiles locally.
- Open multiple real OpenSSH terminals with color, selection, copy/paste, resize, and 2,000 lines of scrollback per session.
- Browse remote directories in an expandable tree. Select a folder as the upload destination.
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

On the first run, enter a hostname or SSH alias and choose **Open terminal**. Use **Connect files** separately for the remote tree and file transfers. Agent/key authentication is attempted first; a password or key passphrase can be entered in the Files panel when required. The app is currently built from source, without a signed installer.

### All platforms

Requires Rust 1.92 or newer and OpenSSH available on the executable search path. Linux requires a graphical desktop with OpenGL and X11 or Wayland libraries. Building on Debian/Ubuntu requires `build-essential`, `pkg-config`, and `libssl-dev`; X11 execution also needs `libxkbcommon-x11-0`. File dialogs use the desktop portal or Zenity. On macOS, install Xcode command-line tools and OpenSSL development libraries. Windows builds require the MSVC build tools and the Windows OpenSSH client.

In this Linux workspace:

```sh
cargo run --release --manifest-path /home/ubuntu/Projects/ssh-client/Cargo.toml
```

The Linux release executable is `/home/ubuntu/Projects/ssh-client/target/release/ssh-client`. This machine has an isolated Rust toolchain, so the exact build command here is:

```sh
OPENSSL_LIB_DIR=/usr/lib/x86_64-linux-gnu OPENSSL_INCLUDE_DIR=/usr/include RUSTUP_HOME=/tmp/ssh-client-rustup CARGO_HOME=/tmp/ssh-client-cargo /tmp/ssh-client-cargo/bin/cargo build --release --locked --manifest-path /home/ubuntu/Projects/ssh-client/Cargo.toml
```

Other machines must use their actual checkout location; their absolute paths are not known here. The source uses platform-specific configuration directories; hover over **Local storage** in the app to see the full path for that machine. No servers are connected automatically at startup.

## Usage

1. Enter a host or SSH alias, optionally a username, port, and absolute private-key path. Blank username and port preserve SSH config defaults.
2. Save the connection, then open a terminal. Complete any authentication prompt in that terminal.
3. Choose **Connect files** to establish the separate SFTP connection. If agent/key authentication is unavailable, enter the password or key passphrase in the Files panel. Verify an unknown fingerprint before trusting it.
4. Expand folders using their arrows; click a folder name to choose an upload destination. Drop local files/folders onto the app with the Files panel visible.
5. Select a regular file and choose **Download**. Pick a destination that does not already exist.

Copy/paste uses Ctrl+Shift+C / Ctrl+Shift+V on Linux and Windows, and Cmd+C / Cmd+V on macOS. Closing a terminal tab ends its SSH session. An exited session remains visible until closed so errors can be read.

## Verification

```sh
cargo test --locked --manifest-path /home/ubuntu/Projects/ssh-client/Cargo.toml
python3 /home/ubuntu/Projects/ssh-client/scripts/test_sftp.py
```

The integration script needs OpenSSH server tools and permission to bind a loopback socket. It creates disposable keys and data in an automatically generated absolute temporary directory and stops its server afterward. It checks unknown-host trust, binary roundtrips, nested folders, no-overwrite behavior, and cancellation cleanup.

## Current limits

This is a working prototype, not yet a packaged cross-platform release. Linux was tested in the development workspace. A CI template for all three desktop operating systems is preserved at `/home/ubuntu/Projects/ssh-client/ci/desktop.yml` on the development machine. Automatic GitHub Actions builds are not enabled because the publishing credential lacks the GitHub `workflow` scope. The template can be activated later with an appropriately authorized login; no remote CI jobs have run yet. The macOS instructions above build directly on your Mac.

- SFTP supports direct connections. SSH config aliases resolve through OpenSSH, but proxy/jump connections are terminal-only for now.
- SFTP checks the standard user known-hosts file and, on Unix, `/etc/ssh/ssh_known_hosts`; custom known-hosts file directives and host certificates are not yet supported.
- SFTP keyboard-interactive/MFA, hardware-backed keys, and complex SSH configuration options need further implementation. The terminal delegates these capabilities to installed OpenSSH.
- Drag-and-drop currently uploads from the operating system. Download uses a native dialog; dragging a remote file out to the desktop is not implemented.
- Folder downloads, overwrite confirmation, resume, remote editing, rename/delete, and transfer concurrency are not implemented. File operations run serially; terminal sessions remain independent.
- Symlinks and special files are not transferred. Cancellation is checked between I/O chunks and may wait for the network timeout; completed files from a cancelled folder upload remain.
- The terminal widget is an upstream prototype; compatibility with complex terminal applications, IME, accessibility, and native OS behavior needs broader testing.

## Footprint approach

The UI uses egui with OpenGL rendering, and re-renders in response to events. There is no bundled browser or JavaScript runtime. Only selected terminal tabs are drawn. SFTP operations run off the UI thread, use bounded transfer buffers, and publish progress at most ten times per second during copying. Terminal history is capped.

Executable size, total installed dependencies, process memory, startup time, and transfer throughput are separate measurements. Check actual release artifacts before setting budgets. Linux dynamic OpenSSL and graphics libraries, the installed OpenSSH executable, and driver memory are not included in the application executable size.

The terminal widget source and MIT license are preserved in `/home/ubuntu/Projects/ssh-client/vendor/egui_term`. Provenance and local modifications are recorded in `/home/ubuntu/Projects/ssh-client/vendor/egui_term/RELAY_CHANGES.md`.

## Local results

The Linux release executable measured 11,540,112 bytes (11.01 MiB). In one Xvfb / Mesa software-OpenGL run with no SSH sessions, the window became visible in 218 ms and the process used approximately 121.8 MiB RSS after one second. The subsequent two-second idle sample consumed no measurable CPU ticks. These are environment-specific observations, not production performance guarantees; memory and startup still need profiling on real Windows, macOS, and Linux desktops. Raw measurements are in `/home/ubuntu/Projects/ssh-client/verification.json`.

Six application tests, one terminal-paste test, and the real SFTP integration test passed. Formatting and Clippy passed. The Linux window, fingerprint prompt, file tree, terminal keyboard focus, and session-exit display were inspected on a virtual display. OS-level drag-and-drop needs further native-platform testing; the raw file-drop event route and real transfer backend are tested independently.
