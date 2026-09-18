# Publishing Relay SSH

The public source repository is `edisontrent17/ssh-client`. The Homebrew tap is `edisontrent17/homebrew-tap`, with cask token `relay-ssh`. Releases currently support Apple Silicon macOS 13+.

## Build and validate

On an Apple Silicon Mac with the source build prerequisites installed:

```sh
cargo test --locked --features ghostty,terminal-fixture
cargo clippy --locked --all-targets --features ghostty,terminal-fixture -- -D warnings
python3 scripts/package_macos.py
```

For the first Ghostty build, run `python3 scripts/build_ghostty.py` before the feature-enabled tests. The packaging script also builds the pinned Ghostty library, builds Relay with a macOS 13 deployment target, copies runtime resources and third-party notices, bundles non-system libraries, rewrites library paths, and ad-hoc signs the bundle. It verifies the signature and runs `--check-installation` after moving the app into a different directory containing spaces. It emits `target/dist/Relay-VERSION-macos-arm64.zip` and `SHA256SUMS`.

Use a clean checkout for releases. The package's version comes from `Cargo.toml`; update its lockfile entry together. Run the native smoke fixture on a macOS desktop:

```sh
cargo run --release --locked --features ghostty,terminal-fixture --example ghostty_smoke
```

Extract the release archive and test its actual app, including the local terminal, before upload. `Relay.app/Contents/MacOS/relay --check-installation` validates resource discovery without opening a GUI or connecting to a host. `--version` prints the package version. `--ghostty-demo` opens a disposable local shell; normal Finder launch opens no connection automatically.

## Publish

Commit the validated source and tag that commit `vVERSION`. Upload the archive and `SHA256SUMS` to the corresponding GitHub Release. Do not replace assets of an existing published version; bump the version for changes. Update `Casks/relay-ssh.rb` in the tap to the new version and exact archive SHA-256, then validate the download and installation with Homebrew. The tap references immutable versioned asset URLs, not a moving `latest` URL.

The app contains no connection profiles, SSH keys, diagnostics from real sessions, compiler, or checkout. Uninstall removes the app; user connection profiles and diagnostics remain. The cask deliberately has no automatic data-deletion step.

## Signing and first launch

Ad-hoc signing checks bundle integrity locally; it is **not** Apple Developer ID signing or notarization. This release can require the user to approve the app under System Settings → Privacy & Security → Open Anyway. The cask does not disable Gatekeeper or strip quarantine. Future notarized releases require a Developer ID certificate and Apple notarization credentials; sign nested libraries and the app, notarize, staple, then recreate and checksum the archive.

Homebrew reference: [tap maintenance](https://docs.brew.sh/How-to-Create-and-Maintain-a-Tap) and [Cask Cookbook](https://docs.brew.sh/Cask-Cookbook).
