//! Native macOS integration check using disposable local PTYs and tmux.
//! cargo run --release --locked --features ghostty,terminal-fixture --example ghostty_smoke
#[cfg(target_os = "macos")]
#[path = "../src/appearance.rs"]
mod appearance;
#[cfg(target_os = "macos")]
#[path = "../src/diagnostics.rs"]
#[allow(dead_code)]
mod diagnostics;
#[cfg(target_os = "macos")]
#[path = "../src/files.rs"]
mod files;
#[cfg(target_os = "macos")]
#[path = "../src/ghostty.rs"]
#[allow(dead_code)]
mod ghostty;
#[cfg(target_os = "macos")]
#[path = "../src/preview.rs"]
mod preview;
#[cfg(target_os = "macos")]
#[path = "../src/profiles.rs"]
mod profiles;
#[cfg(target_os = "macos")]
#[path = "../src/tree.rs"]
mod tree;

#[cfg(target_os = "macos")]
#[allow(dead_code)]
mod app {
    include!("../src/app.rs");
    use std::{
        process::Command,
        time::{Duration, Instant},
    };

    pub struct Smoke {
        relay: Relay,
        temp: tempfile::TempDir,
        socket: String,
        phase: u8,
        started: Instant,
        columns: u16,
        frames: u32,
        focus_check: u8,
    }
    impl Smoke {
        pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
            let temp = tempfile::tempdir().unwrap();
            let socket = temp.path().join("tmux.sock").to_string_lossy().into_owned();
            let mut relay = Relay::new(cc);
            assert!(relay.ghostty_runtime.is_some(), "{}", relay.message);
            relay.profiles.clear();
            relay.config_path = Some(temp.path().join("connections.json"));
            relay.message = "Ghostty smoke test — local PTYs only".into();
            let mut smoke = Self {
                relay,
                temp,
                socket,
                phase: 0,
                started: Instant::now(),
                columns: 0,
                frames: 0,
                focus_check: 0,
            };
            smoke.add(
                cc.egui_ctx.clone(),
                "Typing + clipboard",
                "/bin/sh",
                vec![
                    "-c".into(),
                    "printf '\\033[32mREADY\\033[0m café 日本語\\r\\n'; stty -echo; exec /bin/cat"
                        .into(),
                ],
            );
            smoke.add(
                cc.egui_ctx.clone(),
                "Exit retention",
                "/bin/sh",
                vec!["-c".into(), "printf 'EXIT_SENTINEL\\r\\n'; exit 7".into()],
            );
            smoke.relay.active = Some(1);
            smoke.relay.focus_terminal = true;
            smoke
        }
        fn add(&mut self, ctx: egui::Context, label: &str, shell: &str, args: Vec<String>) {
            let id = self.relay.next_id;
            let backend = TerminalBackend::new(
                id,
                ctx,
                self.relay.terminal_sender.clone(),
                BackendSettings {
                    shell: shell.into(),
                    args,
                    working_directory: None,
                },
            )
            .unwrap();
            self.relay.tabs.push(Tab {
                id,
                label: label.into(),
                profile: None,
                backend,
                ended: false,
            });
            self.relay.next_id += 1;
        }
        fn tmux(&self, args: &[&str]) -> String {
            let output = Command::new("tmux")
                .args(["-S", &self.socket])
                .args(args)
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
            String::from_utf8(output.stdout).unwrap().trim().into()
        }
        fn mode(&self, pane: &str) -> String {
            self.tmux(&["display-message", "-p", "-t", pane, "#{pane_in_mode}"])
        }
        fn next(&mut self) {
            self.phase += 1;
            self.frames = 0;
            println!("Ghostty smoke phase {}", self.phase);
        }
    }
    impl Drop for Smoke {
        fn drop(&mut self) {
            // The socket is unique to this fixture; no user's tmux server is touched.
            let _ = Command::new("tmux")
                .args(["-S", &self.socket, "kill-server"])
                .output();
        }
    }
    impl eframe::App for Smoke {
        fn raw_input_hook(&mut self, _ctx: &egui::Context, input: &mut egui::RawInput) {
            if self.focus_check == 1 {
                // A click delivered to Relay's surrounding egui UI must return
                // first responder to winit so text fields can accept input.
                let pos = egui::pos2(20.0, 20.0);
                input.events.push(egui::Event::PointerMoved(pos));
                for pressed in [true, false] {
                    input.events.push(egui::Event::PointerButton {
                        pos,
                        button: egui::PointerButton::Primary,
                        pressed,
                        modifiers: egui::Modifiers::NONE,
                    });
                }
                self.focus_check = 2;
            }
        }
        fn ui(&mut self, ui: &mut egui::Ui, frame: &mut eframe::Frame) {
            self.relay.ui(ui, frame);
            self.frames += 1;
            assert!(
                self.started.elapsed() < Duration::from_secs(45),
                "Ghostty smoke timed out in phase {}",
                self.phase
            );
            ui.ctx().request_repaint_after(Duration::from_millis(50));
            if self.frames < 3 {
                return;
            }
            match self.phase {
                0 => {
                    if !self.relay.tabs[0].backend.probe_text().contains("READY") {
                        return;
                    }
                    assert!(self.relay.tabs[0].backend.probe_visible());
                    match self.focus_check {
                        0 => {
                            assert!(self.relay.tabs[0].backend.probe_focused());
                            self.focus_check = 1;
                            return;
                        }
                        2 => {
                            assert!(
                                !self.relay.tabs[0].backend.probe_focused(),
                                "Relay UI click must release terminal focus"
                            );
                            self.relay.focus_terminal = true;
                            self.focus_check = 3;
                            return;
                        }
                        _ => assert!(self.relay.tabs[0].backend.probe_focused()),
                    }
                    for character in "typed-café".chars() {
                        self.relay.tabs[0]
                            .backend
                            .probe_key(&character.to_string(), 0, false);
                    }
                    self.relay.tabs[0].backend.probe_key("\r", 36, false);
                    self.next();
                }
                1 => {
                    if !self.relay.tabs[0]
                        .backend
                        .probe_text()
                        .contains("typed-café")
                    {
                        return;
                    }
                    assert!(
                        self.relay.tabs[0]
                            .backend
                            .probe_clipboard("typed-café", Some("PASTE_SENTINEL")),
                        "native clipboard shortcuts"
                    );
                    self.relay.tabs[0].backend.probe_key("\r", 36, false);
                    self.relay.edit_connection(None);
                    self.next();
                }
                2 => {
                    if !self.relay.tabs[0]
                        .backend
                        .probe_text()
                        .contains("PASTE_SENTINEL")
                    {
                        return;
                    }
                    assert!(
                        !self.relay.tabs[0].backend.probe_visible(),
                        "modal must cover native surface"
                    );
                    self.relay.connection_editor = None;
                    self.relay.active = Some(2);
                    self.relay.focus_terminal = true;
                    self.next();
                }
                3 => {
                    if !self.relay.tabs[1].ended {
                        return;
                    }
                    assert_eq!(self.relay.tabs.len(), 2, "exit must retain the tab");
                    assert!(
                        self.relay.tabs[1]
                            .backend
                            .probe_text()
                            .contains("EXIT_SENTINEL")
                    );
                    assert!(!self.relay.tabs[0].backend.probe_visible());
                    assert!(self.relay.tabs[1].backend.probe_visible());
                    assert!(
                        self.relay.tabs[1]
                            .backend
                            .probe_clipboard("EXIT_SENTINEL", None),
                        "copy from ended session"
                    );
                    self.relay.tabs[1].backend.probe_key("\r", 36, false);
                    self.relay.active = Some(1);
                    self.relay.focus_terminal = true;
                    self.next();
                }
                4 => {
                    assert!(self.relay.tabs[0].backend.probe_visible());
                    assert!(self.relay.tabs[0].backend.probe_focused());
                    assert!(!self.relay.tabs[1].backend.probe_visible());
                    assert!(
                        self.relay.tabs[0]
                            .backend
                            .probe_text()
                            .contains("typed-café")
                    );
                    if self.relay.tabs[0].backend.probe_ink_pixels() < 200 {
                        return;
                    }
                    self.columns = self.relay.tabs[0].backend.probe_columns();
                    ui.ctx()
                        .send_viewport_cmd(egui::ViewportCommand::InnerSize(egui::vec2(
                            1100.0, 720.0,
                        )));
                    self.next();
                }
                5 => {
                    if self.relay.tabs[0].backend.probe_columns() == self.columns {
                        return;
                    }
                    let seed = "i=0; while [ $i -lt 300 ]; do printf 'TMUX_LINE_%s\\n' $i; i=$((i+1)); done; exec cat";
                    self.tmux(&[
                        "-f",
                        "/dev/null",
                        "new-session",
                        "-d",
                        "-x",
                        "100",
                        "-y",
                        "24",
                        "-s",
                        "probe",
                        "/bin/sh",
                        "-c",
                        seed,
                    ]);
                    self.tmux(&["set-option", "-g", "mouse", "on"]);
                    self.tmux(&["split-window", "-h", "-t", "probe:0", "/bin/sh", "-c", seed]);
                    self.add(
                        ui.ctx().clone(),
                        "tmux scrolling",
                        "tmux",
                        vec![
                            "-S".into(),
                            self.socket.clone(),
                            "attach-session".into(),
                            "-t".into(),
                            "probe".into(),
                        ],
                    );
                    self.relay.active = Some(3);
                    self.relay.focus_terminal = true;
                    self.next();
                }
                6 => {
                    if !self.relay.tabs[2]
                        .backend
                        .probe_text()
                        .contains("TMUX_LINE")
                    {
                        return;
                    }
                    self.relay.tabs[2].backend.probe_scroll(0.25, 3.0);
                    self.next();
                }
                7 => {
                    if self.mode("probe:0.0") != "1" {
                        return;
                    }
                    assert_eq!(self.mode("probe:0.1"), "0");
                    self.relay.tabs[2].backend.probe_scroll(0.75, 3.0);
                    self.next();
                }
                8 => {
                    if self.mode("probe:0.1") != "1" {
                        return;
                    }
                    // Repeated hide/show must preserve both the model and the
                    // rendered output, including an ended tab.
                    self.relay.active = Some(1);
                    self.next();
                }
                9..=14 => {
                    let index = (self.phase as usize - 9) % 2;
                    if self.relay.tabs[index].backend.probe_ink_pixels() < 200 {
                        return;
                    }
                    let sentinel = if index == 0 {
                        "typed-café"
                    } else {
                        "EXIT_SENTINEL"
                    };
                    assert!(
                        self.relay.tabs[index]
                            .backend
                            .probe_text()
                            .contains(sentinel)
                    );
                    self.relay.active = Some(if index == 0 { 2 } else { 1 });
                    self.next();
                }
                15 => {
                    self.relay.tabs.clear();
                    self.relay.active = None;
                    self.next();
                }
                16 => {
                    println!(
                        "PASS: native typing, Unicode, Cmd+C/V, copy after exit, focus handoff, modal, tab switching, resize, child exit retention, two-pane tmux scrolling, rendered pixels after repeated hide/show, surface destruction"
                    );
                    self.phase = 17;
                    ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
                }
                _ => {}
            }
        }
    }
}

#[cfg(target_os = "macos")]
fn main() -> eframe::Result {
    // Before any GUI/PTY threads are started.
    unsafe {
        std::env::set_var("RELAY_GHOSTTY", "1");
    }
    ghostty::prepare_environment();
    eframe::run_native(
        "Relay — Ghostty native smoke test",
        eframe::NativeOptions {
            viewport: eframe::egui::ViewportBuilder::default()
                .with_inner_size([1280.0, 800.0])
                .with_window_level(eframe::egui::WindowLevel::AlwaysOnTop),
            renderer: eframe::Renderer::Glow,
            ..Default::default()
        },
        Box::new(|cc| Ok(Box::new(app::Smoke::new(cc)))),
    )
}
#[cfg(not(target_os = "macos"))]
fn main() {
    eprintln!("Ghostty prototype requires macOS");
    std::process::exit(2);
}
