//! Opt-in native PTY check: cargo test --locked --test tmux_scroll -- --ignored
use eframe::egui::{self, Event, Modifiers, MouseWheelUnit, Pos2, Rect, Vec2};
use egui_term::{BackendSettings, TerminalBackend, TerminalMode, TerminalView};
use std::process::Command;
use std::time::{Duration, Instant};

struct TmuxServer(tempfile::TempDir);

impl TmuxServer {
    fn command(&self) -> Command {
        let mut command = Command::new("tmux");
        command.args(["-S"]).arg(self.0.path().join("socket"));
        command.env_remove("TMUX");
        command
    }

    fn run(&self, args: &[&str]) -> String {
        let output = self
            .command()
            .args(args)
            .output()
            .expect("tmux must be installed");
        assert!(
            output.status.success() && output.stderr.is_empty(),
            "tmux {args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout).unwrap().trim().into()
    }

    fn scroll_position(&self, pane: &str) -> Option<usize> {
        self.run(&["display-message", "-p", "-t", pane, "#{scroll_position}"])
            .parse()
            .ok()
    }
}

impl Drop for TmuxServer {
    fn drop(&mut self) {
        // Only the isolated socket created by this test, never the user's server.
        let _ = self.command().arg("kill-server").output();
    }
}

fn wait_for(mut check: impl FnMut() -> bool, message: &str) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while !check() {
        assert!(Instant::now() < deadline, "{message}");
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[cfg(unix)]
#[test]
#[ignore = "requires native PTY access"]
fn exited_child_preserves_final_output_and_status() {
    let ctx = egui::Context::default();
    let (sender, receiver) = std::sync::mpsc::channel();
    let mut backend = TerminalBackend::new(
        1,
        ctx,
        sender,
        BackendSettings {
            shell: "/bin/sh".into(),
            args: vec![
                "-c".into(),
                "printf 'synthetic-final-error\\r\\n'; exit 23".into(),
            ],
            working_directory: None,
        },
    )
    .unwrap();
    let mut code = None;
    wait_for(
        || {
            for (_, event) in receiver.try_iter() {
                if let egui_term::PtyEvent::ChildExit(status) = event {
                    code = status.code();
                }
            }
            code.is_some()
        },
        "child exit status was not delivered",
    );
    assert_eq!(code, Some(23));
    // Sync takes the emulator lock after its final PTY drain has completed.
    let text: String = backend.sync().cells.iter().map(|cell| cell.c).collect();
    assert!(text.contains("synthetic-final-error"));
}

#[test]
#[ignore = "requires local tmux and PTY access; uses an isolated server"]
fn wheel_scrolls_the_hovered_tmux_pane() {
    let server = TmuxServer(tempfile::tempdir().unwrap());
    let left = server.run(&[
        "-f",
        "/dev/null",
        "new-session",
        "-d",
        "-P",
        "-F",
        "#{pane_id}",
        "-s",
        "wheel",
        "-x",
        "100",
        "-y",
        "40",
        "seq 1 500; exec /bin/cat",
    ]);
    server.run(&["set-option", "-g", "mouse", "on"]);
    let right = server.run(&[
        "split-window",
        "-h",
        "-P",
        "-F",
        "#{pane_id}",
        "-t",
        &left,
        "seq 1001 1500; exec /bin/cat",
    ]);
    let ctx = egui::Context::default();
    let (sender, _receiver) = std::sync::mpsc::channel();
    let mut backend = TerminalBackend::new(
        1,
        ctx.clone(),
        sender,
        BackendSettings {
            shell: "/usr/bin/env".into(),
            args: vec![
                "-u".into(),
                "TMUX".into(),
                "TERM=xterm-256color".into(),
                "tmux".into(),
                "-S".into(),
                server.0.path().join("socket").to_string_lossy().into(),
                "attach-session".into(),
                "-t".into(),
                "wheel".into(),
            ],
            working_directory: None,
        },
    )
    .unwrap();
    let render = |backend: &mut TerminalBackend, events| {
        let mut rect = Rect::NOTHING;
        let _ = ctx.run_ui(
            egui::RawInput {
                screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(1000.0, 600.0))),
                events,
                ..Default::default()
            },
            |ui| {
                let view = TerminalView::new(ui, backend).set_focus(true);
                rect = ui.add(view).rect;
            },
        );
        rect
    };
    wait_for(
        || {
            render(&mut backend, vec![]);
            backend
                .sync()
                .terminal_mode
                .contains(TerminalMode::SGR_MOUSE)
                && backend
                    .last_content()
                    .terminal_mode
                    .intersects(TerminalMode::MOUSE_MODE)
        },
        "tmux did not enable mouse reporting",
    );
    wait_for(
        || {
            server
                .run(&["display-message", "-p", "-t", &left, "#{history_size}"])
                .parse::<usize>()
                .unwrap()
                > 100
                && server
                    .run(&["display-message", "-p", "-t", &right, "#{history_size}"])
                    .parse::<usize>()
                    .unwrap()
                    > 100
        },
        "tmux panes did not populate their history",
    );

    let mut wheel = |pane: &str, lines| {
        let rect = render(&mut backend, vec![]);
        let size = backend.last_content().terminal_size;
        let col: f32 = server
            .run(&["display-message", "-p", "-t", pane, "#{pane_left}"])
            .parse()
            .unwrap();
        let pos = rect.min
            + Vec2::new(
                (col + 2.5) * size.cell_width as f32,
                2.5 * size.cell_height as f32,
            );
        render(&mut backend, vec![Event::PointerMoved(pos)]);
        render(
            &mut backend,
            vec![Event::MouseWheel {
                unit: MouseWheelUnit::Line,
                delta: Vec2::new(0.0, lines),
                modifiers: Modifiers::NONE,
                phase: egui::TouchPhase::Move,
            }],
        );
    };
    wheel(&left, 3.0);
    wait_for(
        || server.scroll_position(&left).is_some_and(|n| n > 0),
        "left pane did not scroll into copy mode",
    );
    assert_eq!(
        server.run(&["display-message", "-p", "-t", &right, "#{pane_in_mode}"]),
        "0"
    );
    let left_scroll = server.scroll_position(&left).unwrap();
    wheel(&right, 3.0);
    wait_for(
        || server.scroll_position(&right).is_some_and(|n| n > 0),
        "right pane did not scroll into copy mode",
    );
    assert_eq!(server.scroll_position(&left), Some(left_scroll));
    let right_scroll = server.scroll_position(&right).unwrap();
    wheel(&right, -1.0);
    wait_for(
        || {
            server
                .scroll_position(&right)
                .is_some_and(|n| n < right_scroll)
        },
        "wheel down did not return toward newer output",
    );
    println!(
        "tmux mouse scrolling passed: both panes entered copy mode independently; wheel down reduced scroll position"
    );
}
