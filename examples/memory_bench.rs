//! Retained Rust heap benchmark. No PTY, network, or GPU; use
//! scripts/measure_native_memory.py separately for native macOS physical footprint.
#[path = "../src/appearance.rs"]
mod appearance;
#[path = "../src/diagnostics.rs"]
#[allow(dead_code)]
mod diagnostics;
#[path = "../src/files.rs"]
mod files;
#[cfg(all(feature = "ghostty", target_os = "macos"))]
#[path = "../src/ghostty.rs"]
#[allow(dead_code)]
mod ghostty;
#[path = "../src/preview.rs"]
mod preview;
#[path = "../src/profiles.rs"]
mod profiles;
#[path = "../src/tree.rs"]
mod tree;

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicUsize, Ordering};
static LIVE: AtomicUsize = AtomicUsize::new(0);
static PEAK: AtomicUsize = AtomicUsize::new(0);
struct CountingAllocator;
fn add(bytes: usize) {
    let live = LIVE.fetch_add(bytes, Ordering::Relaxed) + bytes;
    PEAK.fetch_max(live, Ordering::Relaxed);
}
unsafe impl GlobalAlloc for CountingAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let ptr = unsafe { System.alloc(layout) };
        if !ptr.is_null() {
            add(layout.size());
        }
        ptr
    }
    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        let ptr = unsafe { System.alloc_zeroed(layout) };
        if !ptr.is_null() {
            add(layout.size());
        }
        ptr
    }
    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, size: usize) -> *mut u8 {
        let result = unsafe { System.realloc(ptr, layout, size) };
        if !result.is_null() {
            if size >= layout.size() {
                add(size - layout.size());
            } else {
                LIVE.fetch_sub(layout.size() - size, Ordering::Relaxed);
            }
        }
        result
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) };
        LIVE.fetch_sub(layout.size(), Ordering::Relaxed);
    }
}
#[global_allocator]
static ALLOCATOR: CountingAllocator = CountingAllocator;

#[allow(dead_code)]
mod app {
    include!("../src/app.rs");

    pub fn benchmark() -> serde_json::Value {
        let mut measurements = Vec::with_capacity(32);
        let mut sample = |stage: String| {
            let live = crate::LIVE.load(std::sync::atomic::Ordering::Relaxed);
            let peak = crate::PEAK.load(std::sync::atomic::Ordering::Relaxed);
            measurements.push(serde_json::json!({
                "stage": stage, "live_rust_bytes": live, "peak_rust_bytes": peak
            }));
        };
        let ctx = egui::Context::default();
        let mut app = Relay::from_context(&ctx);
        app.profiles.clear();
        app.config_path = None;
        app.file_panel = false;
        app.message.clear();
        let frame = |app: &mut Relay| {
            // Also tessellate, then drop each output; don't retain old frames in the harness.
            let output = ctx.run_ui(
                egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::vec2(1280.0, 800.0),
                    )),
                    ..Default::default()
                },
                |ui| app.render(ui),
            );
            std::hint::black_box(ctx.tessellate(output.shapes, output.pixels_per_point));
        };
        for _ in 0..5 {
            frame(&mut app);
        }
        sample("empty".into());
        let seed = "\x1b[32mINFO\x1b[0m status=200 path=/api/items café\r\n".repeat(2100);
        // Repeated open/use/close cycles distinguish retained caches from unbounded growth.
        for cycle in 1..=3 {
            for id in 1..=5 {
                app.tabs.push(Tab {
                    id,
                    label: format!("Local {id}"),
                    backend: TerminalBackend::in_memory(id),
                    ended: false,
                });
                app.active = Some(id);
                frame(&mut app);
                app.tabs
                    .last_mut()
                    .unwrap()
                    .backend
                    .feed_output(seed.as_bytes());
                for _ in 0..5 {
                    frame(&mut app);
                }
                if id == 1 {
                    sample(format!("cycle_{cycle}_one_tab"));
                }
            }
            sample(format!("cycle_{cycle}_five_tabs"));
            for id in 1..=5 {
                app.active = Some(id);
                frame(&mut app);
            }
            sample(format!("cycle_{cycle}_switched_tabs"));
            app.tabs.clear();
            app.active = None;
            for _ in 0..5 {
                frame(&mut app);
            }
            sample(format!("cycle_{cycle}_closed_tabs"));
        }
        serde_json::json!({
            "schema": 1, "viewport": [1280, 800], "history_lines": 2000,
            "tabs": 5, "cycles": 3, "measurements": measurements,
            "metric": "live Rust allocation bytes; excludes allocator overhead, native libraries and GPU"
        })
    }
}

fn main() {
    let path = std::env::args().nth(1).expect("output JSON path");
    let result = app::benchmark();
    std::fs::write(path, serde_json::to_string_pretty(&result).unwrap()).unwrap();
}
