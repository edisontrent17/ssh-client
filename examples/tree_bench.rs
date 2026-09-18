//! Headless CPU benchmark of the actual application UI, using in-memory listings.
//! cargo run --release --locked --example tree_bench -- results.json [samples]
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
#[path = "support/mock_tree.rs"]
#[allow(dead_code)]
mod mock_tree;
#[path = "../src/preview.rs"]
mod preview;
#[path = "../src/profiles.rs"]
mod profiles;
#[path = "../src/tree.rs"]
mod tree;
#[allow(dead_code)]
mod app {
    include!("../src/app.rs");

    pub fn benchmark(samples: usize) -> serde_json::Value {
        use std::time::Instant;
        let mut results = vec![];
        for count in [1_000, 10_000, 100_000] {
            for layout in ["flat", "nested"] {
                let ctx = egui::Context::default();
                let mut app = Relay::from_context(&ctx);
                // A disconnected channel makes accidental remote requests detectable.
                let (sender, requests) = mpsc::channel();
                let (_events, receiver) = mpsc::channel();
                app.files = Worker {
                    sender,
                    receiver,
                    cancel: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
                };
                app.profiles.clear();
                app.config_path = None;
                app.files_connected = true;
                app.root = "/mock".into();
                app.destination = app.root.clone();
                app.files_label = "Benchmark (mock data)".into();
                app.message.clear();
                app.tree.clear();
                for (path, entries) in crate::mock_tree::listings(count, layout == "nested") {
                    if path != app.root {
                        app.expanded.insert(path.clone());
                    }
                    app.tree.insert(path, entries);
                }
                let frame = |app: &mut Relay| {
                    ctx.run_ui(
                        egui::RawInput {
                            screen_rect: Some(egui::Rect::from_min_size(
                                egui::Pos2::ZERO,
                                egui::vec2(1280.0, 800.0),
                            )),
                            ..Default::default()
                        },
                        |ui| app.render(ui),
                    )
                };
                for (scenario, query) in [
                    ("unfiltered_redraw", ""),
                    ("broad_search_redraw", "file"),
                    ("narrow_search_redraw", "target"),
                    ("no_match_redraw", "does-not-exist"),
                    ("query_change", ""),
                ] {
                    app.file_search = query.into();
                    for _ in 0..3 {
                        std::hint::black_box(frame(&mut app));
                    }
                    let mut times = Vec::with_capacity(samples);
                    for iteration in 0..samples {
                        if scenario == "query_change" {
                            app.file_search =
                                if iteration % 2 == 0 { "file" } else { "target" }.into();
                        }
                        let start = Instant::now();
                        std::hint::black_box(frame(&mut app));
                        times.push(start.elapsed().as_secs_f64() * 1000.0);
                    }
                    assert!(
                        requests.try_recv().is_err(),
                        "Benchmark triggered a remote operation"
                    );
                    times.sort_by(f64::total_cmp);
                    let median = times[samples / 2];
                    let p95 = times[((samples as f64 * 0.95).ceil() as usize - 1).min(samples - 1)];
                    eprintln!(
                        "{count:>6} files {layout:6} {scenario:22}: median {median:.3} ms, p95 {p95:.3} ms"
                    );
                    results.push(serde_json::json!({"files": count, "layout": layout, "scenario": scenario, "samples": samples, "median_ms": median, "p95_ms": p95, "raw_ms": times}));
                }
            }
        }
        serde_json::json!({"benchmark": "headless application UI frame; CPU layout/paint generation, no GPU, network, or filesystem I/O in timed region", "viewport": [1280,800], "os": std::env::consts::OS, "arch": std::env::consts::ARCH, "results": results})
    }
}

fn main() {
    let args: Vec<_> = std::env::args().collect();
    let output = args.get(1).expect("output JSON path");
    let samples: usize = args
        .get(2)
        .map(|s| s.parse().expect("sample count"))
        .unwrap_or(21);
    assert!(samples >= 3);
    let results = app::benchmark(samples);
    std::fs::write(output, serde_json::to_string_pretty(&results).unwrap()).unwrap();
}
