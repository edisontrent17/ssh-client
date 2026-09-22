//! Native visual fixture: cargo run --example visual_preview -- /tmp/relay.ppm [width height] [workspace|empty|file|search|large|terminals] [file count]
//! Use --interactive instead of the output path to keep the mock window open.
//! For terminals: [tab count, default 5] [output lines, default 2100] [close after seconds].
//! Uses disposable profiles and a local terminal; never connects to a remote host.
#[path = "../src/appearance.rs"]
mod appearance;
#[path = "support/counting_allocator.rs"]
mod counting_allocator;
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
mod mock_tree;
#[path = "../src/preview.rs"]
mod preview;
#[path = "../src/profiles.rs"]
mod profiles;
#[path = "../src/tree.rs"]
mod tree;
mod app {
    include!("../src/app.rs");

    pub fn fixture(
        cc: &eframe::CreationContext<'_>,
        config: PathBuf,
        state: &str,
        file_count: usize,
        history_lines: usize,
    ) -> Relay {
        let mut app = Relay::new(cc);
        app.config_path = Some(config);
        app.config_error = false;
        app.message.clear();
        if state == "terminals" {
            app.profiles.clear();
            app.selected = None;
            app.file_panel = false;
            for id in 1..=file_count as u64 {
                let script = format!(
                    r#"i=0; while [ "$i" -lt {history_lines} ]; do printf '\033[32mINFO\033[0m request=%04d status=200 café 日本語\r\n' "$i"; i=$((i+1)); done; printf '\r\nLocal terminal demo — type to test echo. No SSH connection.\r\n'; exec /bin/cat"#
                );
                let backend = TerminalBackend::new(
                    id,
                    cc.egui_ctx.clone(),
                    app.terminal_sender.clone(),
                    BackendSettings {
                        shell: "/bin/sh".into(),
                        args: vec!["-c".into(), script],
                        working_directory: None,
                    },
                )
                .expect("local terminal fixture");
                app.tabs.push(Tab {
                    id,
                    label: format!("Local {id}"),
                    profile: None,
                    backend,
                    ended: false,
                });
            }
            app.active = (file_count > 0).then_some(1);
            app.next_id = file_count as u64 + 1;
            app.focus_terminal = true;
            app.message = format!(
                "Local demo: {file_count} terminals, {history_lines} output lines each. No SSH."
            );
            return app;
        }
        if state == "large" {
            let listings = crate::mock_tree::listings(file_count, true);
            app.profiles.clear();
            app.selected = None;
            app.files = crate::mock_tree::worker(cc.egui_ctx.clone(), listings.clone());
            app.files_connected = true;
            app.files_label = "Mock remote tree".into();
            app.root = "/mock".into();
            app.destination = app.root.clone();
            for (path, entries) in listings {
                app.expanded.insert(path.clone());
                app.tree.insert(path, entries);
            }
            app.message = format!(
                "Mock tree: {file_count} files. Search, scroll, expand folders, or preview a file. No SSH connection."
            );
            return app;
        }
        app.profiles = vec![
            Profile {
                name: "Production".into(),
                host: "server.example.com".into(),
                user: "deploy".into(),
                port: "22".into(),
                ..Default::default()
            },
            Profile {
                name: "Staging".into(),
                host: "staging.example.com".into(),
                ..Default::default()
            },
        ];
        if state == "empty" {
            app.profiles.clear();
            return app;
        }
        app.selected = Some(0);
        app.draft = app.profiles[0].clone();
        app.files_connected = true;
        app.files_label = "Production".into();
        app.root = "/home/deploy".into();
        app.destination = app.root.clone();
        app.tree.insert(
            app.root.clone(),
            ["app", "logs", "backups", ".bashrc", ".profile"]
                .into_iter()
                .map(|name| Entry {
                    name: name.into(),
                    path: format!("/home/deploy/{name}"),
                    directory: !name.starts_with('.'),
                    file: name.starts_with('.'),
                    size: 2048,
                })
                .collect(),
        );
        if state == "directory-error" {
            let directories = BTreeMap::from([
                (app.root.clone(), app.tree.get(&app.root).unwrap().clone()),
                ("/home/deploy/app".into(), vec![]),
            ]);
            app.files = crate::mock_tree::worker(cc.egui_ctx.clone(), directories);
            app.expanded.insert("/home/deploy/app".into());
            app.tree
                .fail("/home/deploy/app".into(), "Permission denied".into());
            app.message = "Could not load /home/deploy/app: Permission denied".into();
        }
        let backend = TerminalBackend::new(1, cc.egui_ctx.clone(), app.terminal_sender.clone(), BackendSettings {
            shell: "/bin/sh".into(), args: vec!["-c".into(), "printf 'deploy@server:~$ whoami\\r\\ndeploy\\r\\ndeploy@server:~$ pwd\\r\\n/home/deploy\\r\\ndeploy@server:~$ '; cat".into()], working_directory: None,
        }).expect("local terminal fixture");
        app.tabs.push(Tab {
            id: 1,
            label: "Production".into(),
            profile: None,
            backend,
            ended: false,
        });
        app.active = Some(1);
        app.next_id = 2;
        if state == "file" || state == "search" {
            if state == "search" {
                app.file_search = "bash".into();
            }
            let content = b"# ~/.bashrc\n\n# Interactive shell preferences\nexport EDITOR=vim\nexport LANG=en_US.UTF-8\n\nalias ll='ls -lah'\nalias logs='cd /var/log'\n\n# Add local tools to PATH\nexport PATH=\"$HOME/.local/bin:$PATH\"\n";
            let mut preview = crate::preview::FilePreview {
                id: 1,
                name: ".bashrc".into(),
                path: "/home/deploy/.bashrc".into(),
                host: "Production".into(),
                offset: 0,
                chunk: None,
                lines: vec![],
                binary: false,
                error: None,
            };
            preview.accept(Ok(crate::preview::Chunk {
                offset: 0,
                size: content.len() as u64,
                bytes: content.to_vec(),
            }));
            app.preview = Some(preview);
            app.preview_active = true;
            app.selected_file = app.tree[&app.root]
                .iter()
                .find(|e| e.name == ".bashrc")
                .cloned();
            return app;
        }
        if state != "workspace" && state != "directory-error" {
            app.edit_connection(None);
            let draft = &mut app.connection_editor.as_mut().unwrap().profile;
            draft.name = "Production".into();
            draft.host = "server.example.com".into();
        }
        app
    }

    pub fn close_fixture_terminals(app: &mut Relay) {
        app.tabs.clear();
        app.active = None;
        app.message = "Local fixture: all terminals closed.".into();
    }

    pub fn visit_fixture_terminal(app: &mut Relay, index: usize) -> bool {
        if let Some(tab) = app.tabs.get(index) {
            app.active = Some(tab.id);
            app.focus_terminal = true;
            true
        } else {
            app.active = app.tabs.first().map(|tab| tab.id);
            false
        }
    }
}

struct Preview {
    relay: app::Relay,
    output: Option<std::path::PathBuf>,
    frames: usize,
    requested: bool,
    _temp: tempfile::TempDir,
    close_at: Option<std::time::Instant>,
    memory_probe: bool,
    _probe_wake: Option<std::sync::mpsc::Sender<()>>,
    next_sample: std::time::Instant,
    next_visit: Option<(usize, std::time::Instant)>,
}

fn initial_fixture_zoom(ctx: &eframe::egui::Context) {
    // A queued zoom transition before the first input pass would scale egui's
    // placeholder 10,000-point viewport instead of the real window rectangle.
    ctx.options_mut(|options| options.zoom_factor = 0.8);
}

impl eframe::App for Preview {
    fn raw_input_hook(&mut self, _ctx: &eframe::egui::Context, input: &mut eframe::egui::RawInput) {
        // Capture the focused-window state, regardless of another app receiving OS focus.
        input.focused = true;
        input
            .events
            .retain(|e| !matches!(e, eframe::egui::Event::WindowFocused(false)));
    }

    fn ui(&mut self, ui: &mut eframe::egui::Ui, frame: &mut eframe::Frame) {
        use eframe::egui::{Event, ViewportCommand};
        if self.memory_probe {
            if let Some(window_rect) = ui.ctx().input(|i| i.viewport().inner_rect) {
                let expected = window_rect.size();
                let actual = ui.ctx().content_rect().size();
                assert!(
                    actual.x <= expected.x + 2.0 && actual.y <= expected.y + 2.0,
                    "Fixture layout {actual:?} exceeds native viewport {expected:?}"
                );
            }
            ui.ctx()
                .request_repaint_after(std::time::Duration::from_secs(1));
            if std::time::Instant::now() >= self.next_sample {
                use std::sync::atomic::Ordering;
                let atlas = ui.fonts_mut(|f| f.font_image_size());
                let textures: usize = ui
                    .ctx()
                    .tex_manager()
                    .read()
                    .allocated()
                    .map(|(_, meta)| meta.bytes_used())
                    .sum();
                if self.frames < 4 {
                    println!(
                        "Fixture geometry: content={:?}, ui={:?}, viewport={:?}",
                        ui.ctx().content_rect(),
                        ui.max_rect(),
                        ui.ctx().input(|i| i.viewport().inner_rect)
                    );
                }
                println!(
                    "{}",
                    serde_json::json!({"live_rust_bytes": counting_allocator::LIVE.load(Ordering::Relaxed), "peak_rust_bytes": counting_allocator::PEAK.load(Ordering::Relaxed), "font_atlas": atlas, "texture_bytes": textures})
                );
                self.next_sample = std::time::Instant::now() + std::time::Duration::from_secs(1);
            }
        }
        if let Some(close_at) = self.close_at {
            if let Some(delay) = close_at.checked_duration_since(std::time::Instant::now()) {
                ui.ctx()
                    .request_repaint_after(delay.min(std::time::Duration::from_secs(1)));
            } else {
                println!("Fixture closing tabs");
                app::close_fixture_terminals(&mut self.relay);
                self.close_at = None;
                println!("Fixture tabs closed");
            }
        }
        self.relay.ui(ui, frame);
        if let Some((index, at)) = self.next_visit {
            if std::time::Instant::now() >= at {
                if app::visit_fixture_terminal(&mut self.relay, index) {
                    self.next_visit = Some((
                        index + 1,
                        std::time::Instant::now() + std::time::Duration::from_millis(500),
                    ));
                } else {
                    self.next_visit = None;
                    println!("Fixture all tabs visited");
                }
            }
            ui.ctx()
                .request_repaint_after(std::time::Duration::from_millis(50));
        }
        let screenshot = ui.ctx().input(|i| {
            i.events.iter().find_map(|event| {
                if let Event::Screenshot { image, .. } = event {
                    Some(image.clone())
                } else {
                    None
                }
            })
        });
        if let Some(image) = screenshot {
            use std::io::Write;
            let output = self.output.as_ref().expect("screenshot output path");
            let mut file = std::io::BufWriter::new(std::fs::File::create(output).unwrap());
            write!(file, "P6\n{} {}\n255\n", image.width(), image.height()).unwrap();
            for color in &image.pixels {
                file.write_all(&color.to_array()[..3]).unwrap();
            }
            file.flush().unwrap();
            println!(
                "Captured {} ({}×{})",
                output.display(),
                image.width(),
                image.height()
            );
            ui.ctx().send_viewport_cmd(ViewportCommand::Close);
        }
        self.frames += 1;
        if self.output.is_some() && self.frames >= 8 && !self.requested {
            println!(
                "Viewport {:?}; modal {:?}",
                ui.ctx().content_rect(),
                ui.ctx()
                    .memory(|m| m.area_rect(eframe::egui::Id::new("connection_editor")))
            );
            ui.ctx()
                .send_viewport_cmd(ViewportCommand::Screenshot(Default::default()));
            self.requested = true;
        }
        if self.output.is_some() {
            ui.ctx()
                .request_repaint_after(std::time::Duration::from_millis(50));
        }
    }
}

fn main() -> eframe::Result {
    #[cfg(all(feature = "ghostty", target_os = "macos"))]
    ghostty::prepare_environment();
    let args: Vec<_> = std::env::args().collect();
    let output = args.get(1).expect("output PPM path or --interactive");
    let output = if output == "--interactive" {
        None
    } else {
        Some(output.into())
    };
    let width = args.get(2).and_then(|v| v.parse().ok()).unwrap_or(1440.0);
    let height = args.get(3).and_then(|v| v.parse().ok()).unwrap_or(978.0);
    let state = args.get(4).cloned().unwrap_or_default();
    let file_count = args
        .get(5)
        .map(|v| v.parse().expect("file or terminal count"))
        .unwrap_or(if state == "terminals" { 5 } else { 100_000 });
    let history_lines = args
        .get(6)
        .map(|v| v.parse().expect("output line count"))
        .unwrap_or(2100);
    let close_after: Option<u64> = args
        .get(7)
        .map(|v| v.parse().expect("seconds before closing tabs"));
    eframe::run_native(
        if state == "terminals" {
            "Relay — Terminal mock"
        } else {
            "Relay — Mock preview"
        },
        eframe::NativeOptions {
            viewport: eframe::egui::ViewportBuilder::default()
                .with_inner_size([width * 0.8, height * 0.8])
                .with_window_level(
                    if std::env::var("RELAY_MEMORY_PROBE").as_deref() == Ok("1") {
                        eframe::egui::WindowLevel::AlwaysOnTop
                    } else {
                        eframe::egui::WindowLevel::Normal
                    },
                ),
            renderer: eframe::Renderer::Glow,
            ..Default::default()
        },
        Box::new(move |cc| {
            let memory_probe = std::env::var("RELAY_MEMORY_PROBE").as_deref() == Ok("1");
            initial_fixture_zoom(&cc.egui_ctx);
            let temp = tempfile::tempdir().unwrap();
            let relay = app::fixture(
                cc,
                temp.path().join("connections.json"),
                &state,
                file_count,
                history_lines,
            );
            println!("Fixture ready");
            let probe_wake = if memory_probe {
                let (sender, receiver) = std::sync::mpsc::channel();
                let context = cc.egui_ctx.clone();
                std::thread::spawn(move || {
                    while matches!(
                        receiver.recv_timeout(std::time::Duration::from_secs(1)),
                        Err(std::sync::mpsc::RecvTimeoutError::Timeout)
                    ) {
                        context.request_repaint();
                    }
                });
                Some(sender)
            } else {
                None
            };
            Ok(Box::new(Preview {
                relay,
                output,
                frames: 0,
                requested: false,
                _temp: temp,
                close_at: close_after.map(|seconds| {
                    std::time::Instant::now() + std::time::Duration::from_secs(seconds)
                }),
                memory_probe,
                _probe_wake: probe_wake,
                next_sample: std::time::Instant::now(),
                next_visit: (state == "terminals"
                    && std::env::var("RELAY_MEMORY_VISIT_ALL").as_deref() == Ok("1"))
                .then(|| (0, std::time::Instant::now())),
            }))
        }),
    )
}

#[cfg(test)]
mod fixture_tests {
    #[test]
    fn startup_zoom_preserves_the_first_real_viewport() {
        use eframe::egui::{Context, Pos2, RawInput, Rect, vec2};
        let ctx = Context::default();
        super::initial_fixture_zoom(&ctx);
        let rect = Rect::from_min_size(Pos2::ZERO, vec2(1280.0, 800.0));
        let _ = ctx.run_ui(
            RawInput {
                screen_rect: Some(rect),
                ..Default::default()
            },
            |ui| {
                assert_eq!(ui.ctx().content_rect(), rect);
                assert_eq!(ui.ctx().zoom_factor(), 0.8);
            },
        );
    }
}
