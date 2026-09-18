#!/usr/bin/env python3
"""Build Relay's optional, pinned macOS Ghostty prototype inside target/ghostty."""
import hashlib
import os
from pathlib import Path
import platform
import subprocess
import tarfile
import urllib.request

ROOT = Path(__file__).resolve().parents[1]
WORK = ROOT / "target/ghostty"
SOURCE = WORK / "source"
REVISION = "332b2aefc6e72d363aa93ab6ecfc86eeeeb5ed28"  # Ghostty v1.3.1


def run(*args, **kwargs):
    subprocess.run(args, check=True, **kwargs)


def patch(path, old, new):
    target = SOURCE / path
    text = target.read_text()
    if new in text:
        return
    if text.count(old) != 1:
        raise RuntimeError(f"Unexpected pinned source in {path}; refusing to patch")
    target.write_text(text.replace(old, new))


def main():
    if platform.system() != "Darwin":
        raise SystemExit("This prototype requires macOS.")
    WORK.mkdir(parents=True, exist_ok=True)
    if not SOURCE.exists():
        run("git", "clone", "--depth", "1", "--branch", "v1.3.1",
            "https://github.com/ghostty-org/ghostty.git", str(SOURCE))
    revision = subprocess.check_output(["git", "rev-parse", "HEAD"], cwd=SOURCE, text=True).strip()
    if revision != REVISION:
        raise SystemExit("Unexpected Ghostty revision; refusing to change this checkout.")
    # Keep compilers and dependency caches local; no Homebrew or system install.
    machine = "aarch64" if platform.machine() == "arm64" else "x86_64"
    zig = WORK / f"zig-{machine}-macos-0.15.2/zig"
    if not zig.exists():
        import json
        with urllib.request.urlopen("https://ziglang.org/download/index.json", timeout=30) as response:
            release = json.load(response)["0.15.2"][f"{machine}-macos"]
        archive = WORK / "zig.tar.xz"
        print("Downloading Zig 0.15.2…", flush=True)
        urllib.request.urlretrieve(release["tarball"], archive)
        if hashlib.sha256(archive.read_bytes()).hexdigest() != release["shasum"]:
            raise SystemExit("Zig checksum mismatch")
        with tarfile.open(archive) as bundle:
            bundle.extractall(WORK, filter="data")

    # Upstream only installs an XCFramework on Darwin. Install the same static
    # archive directly, so this Rust consumer doesn't need Xcode's packaging step.
    patch("build.zig", '        // We shouldn\'t have this guard but we don\'t currently',
          '        if (config.target.result.os.tag.isDarwin()) {\n'
          '            libghostty_static.install("libghostty.a");\n'
          '            resources.install();\n'
          '        }\n\n'
          '        // We shouldn\'t have this guard but we don\'t currently')
    # Use Metal's runtime compiler with the exact upstream shader source. This
    # avoids requiring full Xcode/metal on machines with Command Line Tools only.
    patch("build.zig", '    if (config.target.result.os.tag.isDarwin()) {\n        // Ghostty xcframework',
          '    if (config.target.result.os.tag.isDarwin() and (config.emit_xcframework or config.emit_macos_app)) {\n        // Ghostty xcframework')
    patch("build.zig", '        if (config.target.result.os.tag.isDarwin()) {\n            const xcframework_native',
          '        if (config.target.result.os.tag.isDarwin() and (config.emit_xcframework or config.emit_macos_app)) {\n            const xcframework_native')
    patch("src/build/SharedDeps.zig",
          '        const metallib = self.metallib.?;\n'
          '        metallib.output.addStepDependencies(&step.step);\n'
          '        step.root_module.addAnonymousImport("ghostty_metallib", .{\n'
          '            .root_source_file = metallib.output,\n'
          '        });',
          '        step.root_module.addAnonymousImport("ghostty_metallib", .{\n'
          '            .root_source_file = b.path("src/renderer/shaders/shaders.metal"),\n'
          '        });')
    patch("src/renderer/metal/shaders.zig",
          '    const data = try macos.dispatch.Data.create(\n'
          '        @embedFile("ghostty_metallib"),\n'
          '        macos.dispatch.queue.getMain(),\n'
          '        macos.dispatch.Data.DESTRUCTOR_DEFAULT,\n'
          '    );',
          '    const data = try macos.foundation.String.createWithBytes(\n'
          '        @embedFile("ghostty_metallib"), .utf8, false,\n'
          '    );')
    patch("src/renderer/metal/shaders.zig",
          '        objc.sel("newLibraryWithData:error:"),\n'
          '        .{\n            data,\n            &err,\n        },',
          '        objc.sel("newLibraryWithSource:options:error:"),\n'
          '        .{ data, @as(?*anyopaque, null), &err },')
    # Relay displays one terminal at a time. Release hidden Metal frame buffers,
    # preserving terminal/history and CPU font data. Wait for in-flight GPU work
    # under the same mutex as drawFrame before replacing the swap chain. Fresh
    # frame state forces texture uploads; cells_rebuilt forces a complete redraw.
    patch("src/renderer/generic.zig",
          '        pub fn setVisible(self: *Self, visible: bool) void {\n',
          '''        pub fn setVisible(self: *Self, visible: bool) void {
            // Relay: hidden tabs need terminal state, but not full-size GPU frames.
            if (comptime GraphicsAPI == renderer.Metal) {
                if (!visible) {
                    self.draw_mutex.lock();
                    defer self.draw_mutex.unlock();
                    // Allocate first: on failure the existing renderer stays valid.
                    const compact = SwapChain.init(self.api, self.has_custom_shaders) catch |err| {
                        log.warn("unable to compact hidden terminal frames err={}", .{err});
                        return;
                    };
                    // deinit waits for all frame-completion callbacks before we
                    // replace the semaphore and release textures/buffers.
                    self.swap_chain.deinit();
                    self.swap_chain = compact;
                    self.cells_rebuilt = true;
                }
            }
''')
    # Apple's newer libtool silently omits some valid Zig 0.15 objects after
    # alignment warnings. LLVM's archiver retains all members and exports.
    (SOURCE / "relay-combine.py").write_text(
        'import os, subprocess, sys, tempfile\n'
        'from pathlib import Path\n'
        'output, *libraries = sys.argv[3:]\n'
        'output = Path(output).resolve()\n'
        'with tempfile.TemporaryDirectory(dir=output.parent) as work:\n'
        '    script = "CREATE combined.a\\n"\n'
        '    for index, library in enumerate(libraries):\n'
        '        name = f"lib{index}.a"\n'
        '        Path(work, name).symlink_to(Path(library).resolve())\n'
        '        script += "ADDLIB " + name + "\\n"\n'
        '    subprocess.run([os.environ["RELAY_GHOSTTY_ZIG"], "ar", "-M"], input=script + "SAVE\\nEND\\n", cwd=work, text=True, check=True)\n'
        '    os.replace(Path(work, "combined.a"), output)\n')
    patch("src/build/LibtoolStep.zig", '    run_step.addArgs(&.{ "libtool", "-static", "-o" });',
          '    run_step.addArgs(&.{ "python3", b.pathFromRoot("relay-combine.py"), "-static", "-o" });')
    env = dict(os.environ, ZIG_GLOBAL_CACHE_DIR=str(WORK / "zig-cache"))
    # Zig 0.15 cannot link the arm64e-only libSystem stubs in SDK 26.
    # Use an installed SDK 15 when needed, without changing xcode-select.
    sdk = Path(os.environ.get("RELAY_GHOSTTY_SDK") or subprocess.check_output(
        ["/usr/bin/xcrun", "--sdk", "macosx", "--show-sdk-path"], text=True).strip())
    if machine == "aarch64" and "arm64-macos" not in (sdk / "usr/lib/libSystem.tbd").read_text().split("install-name:")[0]:
        candidates = sorted(sdk.parent.glob("MacOSX15.*.sdk"))
        if not candidates:
            raise SystemExit("Zig 0.15 requires SDK 15 on this Mac; set RELAY_GHOSTTY_SDK to a compatible installed SDK.")
        sdk = candidates[-1]
    shims = WORK / "tools"
    shims.mkdir(exist_ok=True)
    shim = shims / "xcrun"
    shim.write_text('#!/bin/sh\nif [ "$1" = "--sdk" ] && [ "$2" = "macosx" ]; then\n'
                    '  shift 2\n  exec /usr/bin/xcrun --sdk "$RELAY_GHOSTTY_SDK" "$@"\nfi\n'
                    'exec /usr/bin/xcrun "$@"\n')
    shim.chmod(0o755)
    env.update(PATH=str(shims) + os.pathsep + env["PATH"], RELAY_GHOSTTY_SDK=str(sdk), RELAY_GHOSTTY_ZIG=str(zig))
    print(f"Building Ghostty with {sdk}", flush=True)
    run(str(zig), "build", "-Doptimize=ReleaseFast", "-Dapp-runtime=none",
        "-Demit-xcframework=false", "-Demit-macos-app=false", "-Demit-docs=false",
        "-Demit-themes=false", "-Dsentry=false", "-Dstrip=true", "-j4", cwd=SOURCE, env=env)
    print(f"Ghostty library ready: {SOURCE / 'zig-out/lib/libghostty.a'}")
    print("Run: cargo run --release --locked --features ghostty -- --ghostty")


if __name__ == "__main__":
    main()
