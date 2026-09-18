#!/usr/bin/env python3
"""Build a relocatable Apple Silicon Relay.app and versioned release archive."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import platform
import plistlib
import re
import shutil
import subprocess
import tempfile
import tomllib

ROOT = Path(__file__).resolve().parents[1]


def run(*args, **kwargs):
    return subprocess.run(args, check=True, **kwargs)


def dependencies(binary):
    output = subprocess.check_output(["otool", "-L", str(binary)], text=True)
    return [line.strip().split(" (compatibility", 1)[0] for line in output.splitlines()[1:]]


def check_deployment_target(binary):
    output = subprocess.check_output(["otool", "-l", str(binary)], text=True)
    versions = re.findall(r"cmd LC_BUILD_VERSION\s+cmdsize \d+\s+platform \d+\s+minos ([\d.]+)", output)
    versions += re.findall(r"cmd LC_VERSION_MIN_MACOSX\s+cmdsize \d+\s+version ([\d.]+)", output)
    if not versions or any(tuple(map(int, version.split('.')[:2])) > (13, 0) for version in versions):
        raise RuntimeError(f"{binary.name} does not support the advertised macOS 13 minimum: {versions}")


def bundle_libraries(executable, frameworks):
    """Copy non-system libraries and make every install name bundle-relative."""
    frameworks.mkdir()
    copied = {}
    pending = [executable]
    while pending:
        binary = pending.pop()
        for dependency in dependencies(binary):
            if dependency.startswith(("/System/", "/usr/lib/", "@")):
                continue
            source = Path(dependency).resolve(strict=True)
            target = frameworks / source.name
            if source.name in copied and copied[source.name] != source:
                raise RuntimeError(f"Conflicting library names: {source.name}")
            if source.name not in copied:
                copied[source.name] = source
                shutil.copy2(source, target)
                target.chmod(0o755)
                run("install_name_tool", "-id", f"@rpath/{target.name}", str(target))
                pending.append(target)
            relative = ("@executable_path/../Frameworks/" if binary == executable else "@loader_path/") + target.name
            run("install_name_tool", "-change", dependency, relative, str(binary))
    for library in frameworks.iterdir():
        run("codesign", "--force", "--sign", "-", str(library))
    for binary in [executable, *frameworks.iterdir()]:
        check_deployment_target(binary)
        forbidden = [p for p in dependencies(binary) if not p.startswith(("/System/", "/usr/lib/", "@"))]
        if forbidden:
            raise RuntimeError(f"External dependencies remain: {forbidden}")


def notices(destination):
    """Include license texts from the resolved Rust and pinned native sources."""
    destination.mkdir()
    metadata = json.loads(subprocess.check_output([
        "cargo", "metadata", "--locked", "--offline", "--format-version", "1",
        "--features", "macos-distribution", "--filter-platform", "aarch64-apple-darwin",
    ], cwd=ROOT, text=True))
    sources = [(f"rust-{p['name']}-{p['version']}", Path(p["manifest_path"]).parent)
               for p in metadata["packages"] if p["name"] != "ssh-client"]
    ghostty = ROOT / "target/ghostty/source"
    sources += [("ghostty", ghostty), ("nerd-fonts", ghostty / "vendor/nerd-fonts"),
                ("ghostty-fonts", ghostty / "src/font/res"),
                ("lucide-icons", ROOT / "src/assets/icons")]
    sources += [(f"ghostty-dependency-{p.name}", p) for p in (ROOT / "target/ghostty/zig-cache/p").iterdir() if p.is_dir()]
    sources += [("openssl", Path(p["manifest_path"]).parent / "openssl")
                for p in metadata["packages"] if p["name"] == "openssl-src"]
    for name, folder in sources:
        matches = []
        for subdir in [folder, folder / "docs", folder / "licenses"]:
            if subdir.is_dir():
                matches.extend(p for p in subdir.iterdir() if p.is_file()
                               and p.name.upper().startswith(("LICENSE", "COPYING", "NOTICE", "FTL", "OFL")))
        for source in matches:
            target = destination / name / source.relative_to(folder)
            target.parent.mkdir(parents=True, exist_ok=True)
            shutil.copy2(source, target)
    manifest = [{"name": p["name"], "version": p["version"], "license": p["license"], "repository": p["repository"]}
                for p in metadata["packages"] if p["name"] != "ssh-client"]
    (destination / "rust-packages.json").write_text(json.dumps(manifest, indent=2) + "\n")
    (destination / "README.txt").write_text("Third-party notices for Relay SSH. The source/build dependency inventory may include components not linked into this macOS executable. Ghostty is pinned by scripts/build_ghostty.py.\n")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--skip-build", action="store_true", help="Package an already built release binary")
    args = parser.parse_args()
    if platform.system() != "Darwin" or platform.machine() != "arm64":
        parser.error("This release script currently supports Apple Silicon macOS only")
    version = tomllib.loads((ROOT / "Cargo.toml").read_text())["package"]["version"]
    if not args.skip_build:
        run("python3", "scripts/build_ghostty.py", cwd=ROOT)
        run("cargo", "build", "--release", "--locked", "--features", "macos-distribution", "--bin", "ssh-client",
            cwd=ROOT, env=dict(os.environ, MACOSX_DEPLOYMENT_TARGET="13.0"))
    output = ROOT / "target/dist"
    output.mkdir(exist_ok=True)
    with tempfile.TemporaryDirectory(prefix="relay-package-", dir=output) as temporary:
        app = Path(temporary) / "Relay.app"
        contents = app / "Contents"
        executable = contents / "MacOS/relay"
        resources = contents / "Resources"
        executable.parent.mkdir(parents=True)
        resources.mkdir()
        shutil.copy2(ROOT / "target/release/ssh-client", executable)
        executable.chmod(0o755)
        share = ROOT / "target/ghostty/source/zig-out/share"
        for name in ["ghostty", "terminfo"]:
            shutil.copytree(share / name, resources / name)
        notices(resources / "ThirdPartyNotices")
        info = {"CFBundleName": "Relay", "CFBundleDisplayName": "Relay SSH",
                "CFBundleIdentifier": "app.relay.Relay", "CFBundleExecutable": "relay",
                "CFBundlePackageType": "APPL", "CFBundleShortVersionString": version,
                "CFBundleVersion": version, "LSMinimumSystemVersion": "13.0",
                "NSHighResolutionCapable": True,
                "NSHumanReadableCopyright": "Relay SSH contributors; third-party notices in Resources."}
        (contents / "Info.plist").write_bytes(plistlib.dumps(info))
        bundle_libraries(executable, contents / "Frameworks")
        run("codesign", "--force", "--sign", "-", str(app))
        run("codesign", "--verify", "--deep", "--strict", str(app))
        # Relocation and paths containing spaces are deliberate release checks.
        relocated = Path(temporary) / "Relocated app"
        relocated.mkdir()
        shutil.move(str(app), relocated / app.name)
        app = relocated / app.name
        executable = str(app / "Contents/MacOS/relay")
        check = subprocess.check_output([executable, "--check-installation"], text=True)
        actual_version = subprocess.check_output([executable, "--version"], text=True).strip()
        if "Ghostty resources OK" not in check or actual_version != f"Relay SSH {version}":
            raise RuntimeError("Release binary must match Cargo.toml and include Ghostty")
        print(check.strip())
        print(actual_version)
        archive = output / f"Relay-{version}-macos-arm64.zip"
        run("ditto", "-c", "-k", "--sequesterRsrc", "--keepParent", str(app), str(archive))
    checksum = hashlib.sha256(archive.read_bytes()).hexdigest()
    (output / "SHA256SUMS").write_text(f"{checksum}  {archive.name}\n")
    print(f"Release: {archive}\nSHA256: {checksum}")


if __name__ == "__main__":
    main()
