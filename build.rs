fn main() {
    #[cfg(feature = "ghostty")]
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("macos") {
        use std::path::PathBuf;
        let root = PathBuf::from(std::env::var_os("CARGO_MANIFEST_DIR").unwrap());
        let source = root.join("target/ghostty/source");
        let lib = source.join("zig-out/lib");
        assert!(
            lib.join("libghostty.a").is_file(),
            "Run python3 scripts/build_ghostty.py before building --features ghostty"
        );
        println!("cargo:rerun-if-changed=native/ghostty_bridge.m");
        println!(
            "cargo:rerun-if-changed={}",
            source.join("include/ghostty.h").display()
        );
        println!(
            "cargo:rerun-if-changed={}",
            lib.join("libghostty.a").display()
        );
        cc::Build::new()
            .file("native/ghostty_bridge.m")
            .include(source.join("include"))
            .flag("-fobjc-arc")
            .flag("-fblocks")
            .flag("-mmacosx-version-min=13.0")
            .compile("relay_ghostty_bridge");
        println!("cargo:rustc-link-search=native={}", lib.display());
        println!("cargo:rustc-link-lib=static=ghostty");
        for framework in [
            "AppKit",
            "CoreFoundation",
            "CoreGraphics",
            "CoreText",
            "CoreVideo",
            "QuartzCore",
            "IOSurface",
            "Carbon",
            "Metal",
        ] {
            println!("cargo:rustc-link-lib=framework={framework}");
        }
        println!("cargo:rustc-link-lib=c++");
        println!("cargo:rustc-link-arg=-mmacosx-version-min=13.0");
        println!(
            "cargo:rustc-env=RELAY_GHOSTTY_RESOURCES={}",
            source.join("zig-out/share/ghostty").display()
        );
    }
}
