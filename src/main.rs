#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
mod files;
mod profiles;

fn main() -> eframe::Result {
    // Set terminal capabilities before the GUI or worker threads are started.
    unsafe {
        std::env::set_var("TERM", "xterm-256color");
        std::env::set_var("COLORTERM", "truecolor");
    }
    eframe::run_native(
        "Relay — SSH",
        eframe::NativeOptions {
            viewport: eframe::egui::ViewportBuilder::default()
                .with_inner_size([1100.0, 720.0])
                .with_min_inner_size([760.0, 480.0]),
            renderer: eframe::Renderer::Glow,
            ..Default::default()
        },
        Box::new(|cc| Ok(Box::new(app::Relay::new(cc)))),
    )
}
