//! CPU/allocator benchmark using the real terminal parser and application UI.
//! cargo run --release --locked --features terminal-fixture --example terminal_bench -- results.json [samples]
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
use std::sync::atomic::{AtomicU64, Ordering};
static ALLOCATIONS: AtomicU64 = AtomicU64::new(0);
static ALLOCATED_BYTES: AtomicU64 = AtomicU64::new(0);
struct CountingAllocator;
fn allocated(ptr: *mut u8, size: usize) -> *mut u8 {
    if !ptr.is_null() {
        ALLOCATIONS.fetch_add(1, Ordering::Relaxed);
        ALLOCATED_BYTES.fetch_add(size as u64, Ordering::Relaxed);
    }
    ptr
}
unsafe impl GlobalAlloc for CountingAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        allocated(unsafe { System.alloc(layout) }, layout.size())
    }
    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        allocated(unsafe { System.alloc_zeroed(layout) }, layout.size())
    }
    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, size: usize) -> *mut u8 {
        allocated(unsafe { System.realloc(ptr, layout, size) }, size)
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) }
    }
}
#[global_allocator]
static ALLOCATOR: CountingAllocator = CountingAllocator;

#[allow(dead_code)]
mod app {
    include!("../src/app.rs");

    pub fn benchmark(samples: usize) -> serde_json::Value {
        use std::time::Instant;
        let mut results = vec![];
        let colored = (0..20).map(|i| format!("\x1b[38;5;{}mINFO\x1b[0m request={i:04} status=200 latency=12ms path=/api/items\r\n", 16+i)).collect::<String>();
        let unicode =
            "\x1b[32mUnicode\x1b[0m café 日本語 🦀 e\u{301} — full width and combining text\r\n"
                .repeat(20);
        for viewport in [[1280.0, 800.0], [1920.0, 1080.0]] {
            for tab_count in [1, 5, 10] {
                let ctx = egui::Context::default();
                let mut app = Relay::from_context(&ctx);
                app.profiles.clear();
                app.config_path = None;
                app.file_panel = false;
                app.message.clear();
                let frame = |app: &mut Relay, width: f32| {
                    ctx.run_ui(
                        egui::RawInput {
                            screen_rect: Some(egui::Rect::from_min_size(
                                egui::Pos2::ZERO,
                                egui::vec2(width, viewport[1]),
                            )),
                            ..Default::default()
                        },
                        |ui| app.render(ui),
                    )
                };
                for index in 0..tab_count {
                    let id = index as u64 + 1;
                    app.tabs.push(Tab {
                        id,
                        label: format!("Mock {}", index + 1),
                        profile: None,
                        backend: TerminalBackend::in_memory(id),
                        ended: false,
                    });
                    app.active = Some(id);
                    frame(&mut app, viewport[0]);
                    let seed = colored.repeat(125); // Fill the 2,000-line scrollback.
                    app.tabs[index].backend.feed_output(seed.as_bytes());
                    frame(&mut app, viewport[0]);
                }
                app.active = Some(1);
                for scenario in [
                    "idle_redraw",
                    "typing_echo",
                    "active_output",
                    "inactive_output",
                    "all_tabs_output",
                    "unicode_output",
                    "scrollback",
                    "resize",
                ] {
                    for _ in 0..5 {
                        std::hint::black_box(frame(&mut app, viewport[0]));
                    }
                    let mut times = Vec::with_capacity(samples);
                    let mut allocations = Vec::with_capacity(samples);
                    let mut allocated_bytes = Vec::with_capacity(samples);
                    for iteration in 0..samples {
                        let before_allocations = crate::ALLOCATIONS.load(Ordering::Relaxed);
                        let before_bytes = crate::ALLOCATED_BYTES.load(Ordering::Relaxed);
                        let start = Instant::now();
                        match scenario {
                            "typing_echo" => app.tabs[0].backend.feed_output(b"x"),
                            "active_output" => app.tabs[0].backend.feed_output(colored.as_bytes()),
                            "inactive_output" => {
                                for tab in app.tabs.iter_mut().skip(1) {
                                    tab.backend.feed_output(colored.as_bytes());
                                }
                            }
                            "all_tabs_output" => {
                                for tab in &mut app.tabs {
                                    tab.backend.feed_output(colored.as_bytes());
                                }
                            }
                            "unicode_output" => app.tabs[0].backend.feed_output(unicode.as_bytes()),
                            "scrollback" => app.tabs[0].backend.process_command(
                                egui_term::BackendCommand::Scroll(if iteration % 2 == 0 {
                                    2
                                } else {
                                    -2
                                }),
                            ),
                            _ => {}
                        }
                        let width = if scenario == "resize" && iteration % 2 == 0 {
                            viewport[0] - 160.0
                        } else {
                            viewport[0]
                        };
                        std::hint::black_box(frame(&mut app, width));
                        times.push(start.elapsed().as_secs_f64() * 1000.0);
                        allocations
                            .push(crate::ALLOCATIONS.load(Ordering::Relaxed) - before_allocations);
                        allocated_bytes
                            .push(crate::ALLOCATED_BYTES.load(Ordering::Relaxed) - before_bytes);
                    }
                    times.sort_by(f64::total_cmp);
                    allocations.sort();
                    allocated_bytes.sort();
                    let median = times[samples / 2];
                    let p95 = times[((samples as f64 * 0.95).ceil() as usize - 1).min(samples - 1)];
                    eprintln!(
                        "{}x{} {tab_count:2} tabs {scenario:16} median {median:.3} ms p95 {p95:.3} ms, {} allocs / {} bytes",
                        viewport[0],
                        viewport[1],
                        allocations[samples / 2],
                        allocated_bytes[samples / 2]
                    );
                    results.push(serde_json::json!({"viewport":viewport,"tabs":tab_count,"scenario":scenario,"samples":samples,"median_ms":median,"p95_ms":p95,"median_allocations":allocations[samples/2],"median_allocated_bytes":allocated_bytes[samples/2],"raw_ms":times}));
                }
            }
        }
        serde_json::json!({"benchmark":"terminal ANSI parsing plus headless application UI frame; counting allocator enabled; no PTY, network or GPU presentation","os":std::env::consts::OS,"arch":std::env::consts::ARCH,"scrollback_lines":2000,"results":results})
    }
}

fn main() {
    let args: Vec<_> = std::env::args().collect();
    let path = args.get(1).expect("output JSON path");
    let samples = args
        .get(2)
        .map(|s| s.parse::<usize>().expect("sample count"))
        .unwrap_or(31);
    assert!(samples >= 5);
    let result = app::benchmark(samples);
    std::fs::write(path, serde_json::to_string_pretty(&result).unwrap()).unwrap();
}
