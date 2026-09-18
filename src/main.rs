#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
mod appearance;
mod diagnostics;
mod files;
#[cfg(all(feature = "ghostty", target_os = "macos"))]
mod ghostty;
mod preview;
mod profiles;
mod tree;

fn main() -> eframe::Result {
    if std::env::args().any(|arg| arg == "--version") {
        println!("Relay SSH {}", env!("CARGO_PKG_VERSION"));
        return Ok(());
    }
    if std::env::args().any(|arg| arg == "--check-installation") {
        #[cfg(all(feature = "ghostty", target_os = "macos"))]
        {
            let resources = ghostty::resources_dir();
            if !resources.join("shell-integration").is_dir()
                || !resources
                    .parent()
                    .is_some_and(|p| p.join("terminfo").is_dir())
            {
                eprintln!(
                    "Ghostty runtime resources are missing: {}",
                    resources.display()
                );
                std::process::exit(1);
            }
            println!(
                "Relay SSH {}: Ghostty resources OK ({})",
                env!("CARGO_PKG_VERSION"),
                resources.display()
            );
        }
        #[cfg(not(all(feature = "ghostty", target_os = "macos")))]
        println!("Relay SSH {}: standard terminal", env!("CARGO_PKG_VERSION"));
        return Ok(());
    }
    #[cfg(all(feature = "ghostty", target_os = "macos"))]
    ghostty::prepare_environment();
    #[cfg(not(all(feature = "ghostty", target_os = "macos")))]
    if std::env::args().any(|arg| arg == "--ghostty" || arg == "--ghostty-demo") {
        eprintln!(
            "Build the macOS prototype with --features ghostty first; see docs/ghostty-prototype.md"
        );
        std::process::exit(2);
    }
    if let Err(error) = diagnostics::init() {
        eprintln!("Relay diagnostics could not be initialized: {error}");
    }
    // Set terminal capabilities before the GUI or worker threads are started.
    unsafe {
        std::env::set_var("TERM", "xterm-256color");
        std::env::set_var("COLORTERM", "truecolor");
    }
    let result = eframe::run_native(
        "Relay — SSH",
        eframe::NativeOptions {
            viewport: eframe::egui::ViewportBuilder::default()
                .with_inner_size([1280.0, 800.0])
                .with_min_inner_size([760.0, 480.0]),
            renderer: eframe::Renderer::Glow,
            ..Default::default()
        },
        Box::new(|cc| Ok(Box::new(app::Relay::new(cc)))),
    );
    diagnostics::record(
        "app_returned",
        serde_json::json!({"success": result.is_ok()}),
    );
    result
}
