mod activity;
pub mod settings;
use activity::Activity;

use crate::types::Size;
use alacritty_terminal::event::{Event, EventListener, Notify, OnResize, WindowSize};
use alacritty_terminal::event_loop::{EventLoop, Msg, Notifier};
use alacritty_terminal::grid::Indexed;
use alacritty_terminal::grid::{Dimensions, Scroll};
use alacritty_terminal::index::{Column, Direction, Line, Point, Side};
use alacritty_terminal::selection::{
    Selection, SelectionRange, SelectionType as AlacrittySelectionType,
};
use alacritty_terminal::sync::FairMutex;
use alacritty_terminal::term::search::{Match, RegexIter, RegexSearch};
use alacritty_terminal::term::{
    self, cell::Cell, test::TermSize, viewport_to_point, Term, TermMode,
};
use alacritty_terminal::tty;
use egui::Modifiers;
use settings::BackendSettings;
use std::borrow::Cow;
use std::cmp::min;
use std::io::Result;
use std::ops::{Index, RangeInclusive};
use std::sync::mpsc::Sender;
use std::sync::{mpsc, Arc};

pub type TerminalMode = TermMode;
pub type PtyEvent = Event;
pub type SelectionType = AlacrittySelectionType;

#[derive(Debug, Clone)]
pub enum BackendCommand {
    Write(Vec<u8>),
    Scroll(i32),
    /// Wheel input in terminal-local pixels; mouse coordinates use the viewport.
    MouseWheel {
        lines: i32,
        modifiers: Modifiers,
        position: egui::Vec2,
    },
    Resize(Size, Size),
    SelectStart(SelectionType, f32, f32),
    SelectUpdate(f32, f32),
    ProcessLink(LinkAction, Point),
    MouseReport(MouseButton, Modifiers, Point, bool),
}

#[derive(Debug, Clone)]
pub enum MouseMode {
    Sgr,
    Normal(bool),
}

impl From<TermMode> for MouseMode {
    fn from(term_mode: TermMode) -> Self {
        if term_mode.contains(TermMode::SGR_MOUSE) {
            MouseMode::Sgr
        } else if term_mode.contains(TermMode::UTF8_MOUSE) {
            MouseMode::Normal(true)
        } else {
            MouseMode::Normal(false)
        }
    }
}

#[derive(Debug, Clone)]
pub enum MouseButton {
    LeftButton = 0,
    MiddleButton = 1,
    RightButton = 2,
    LeftMove = 32,
    MiddleMove = 33,
    RightMove = 34,
    NoneMove = 35,
    ScrollUp = 64,
    ScrollDown = 65,
    Other = 99,
}

#[derive(Debug, Clone)]
pub enum LinkAction {
    Clear,
    Hover,
    Open,
}

#[derive(Clone, Copy, Debug)]
pub struct TerminalSize {
    pub cell_width: u16,
    pub cell_height: u16,
    num_cols: u16,
    num_lines: u16,
    layout_size: Size,
}

impl Default for TerminalSize {
    fn default() -> Self {
        Self {
            cell_width: 1,
            cell_height: 1,
            num_cols: 80,
            num_lines: 50,
            layout_size: Size::default(),
        }
    }
}

impl Dimensions for TerminalSize {
    fn total_lines(&self) -> usize {
        self.screen_lines()
    }

    fn screen_lines(&self) -> usize {
        self.num_lines as usize
    }

    fn columns(&self) -> usize {
        self.num_cols as usize
    }

    fn last_column(&self) -> Column {
        Column(self.num_cols as usize - 1)
    }

    fn bottommost_line(&self) -> Line {
        Line(self.num_lines as i32 - 1)
    }
}

impl From<TerminalSize> for WindowSize {
    fn from(size: TerminalSize) -> Self {
        Self {
            num_lines: size.num_lines,
            num_cols: size.num_cols,
            cell_width: size.cell_width,
            cell_height: size.cell_height,
        }
    }
}

pub struct TerminalBackend {
    id: u64,
    pty_id: u32,
    url_regex: RegexSearch,
    term: Arc<FairMutex<Term<EventProxy>>>,
    size: TerminalSize,
    notifier: Option<Notifier>,
    last_content: RenderableContent,
    render_cache: crate::view::RenderCache,
    activity: Arc<Activity>,
    snapshot_dirty: bool,
    synced_generation: u64,
    #[cfg(any(test, feature = "test-support"))]
    fixture_parser: alacritty_terminal::vte::ansi::Processor,
    #[cfg(any(test, feature = "test-support"))]
    fixture_input: std::sync::Mutex<Vec<u8>>,
}

impl TerminalBackend {
    pub fn new(
        id: u64,
        app_context: egui::Context,
        pty_event_proxy_sender: Sender<(u64, PtyEvent)>,
        settings: BackendSettings,
    ) -> Result<Self> {
        let pty_config = tty::Options {
            shell: Some(tty::Shell::new(settings.shell, settings.args)),
            working_directory: settings.working_directory,
            ..tty::Options::default()
        };
        let config = term::Config {
            scrolling_history: 2000,
            ..term::Config::default()
        };
        let terminal_size = TerminalSize::default();
        let pty = tty::new(&pty_config, terminal_size.into(), id)?;
        #[cfg(not(windows))]
        let pty_id = pty.child().id();
        #[cfg(windows)]
        let pty_id = pty
            .child_watcher()
            .pid()
            .ok_or(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "Failed to get child process ID",
            ))?
            .into();
        let (event_sender, event_receiver) = mpsc::channel();
        let activity = Arc::new(Activity::default());
        let event_proxy = EventProxy(event_sender, activity.clone());
        let mut term = Term::new(config, &terminal_size, event_proxy.clone());
        let initial_content = RenderableContent {
            cells: term
                .grid()
                .display_iter()
                .map(|indexed| Indexed {
                    point: indexed.point,
                    cell: indexed.cell.clone(),
                })
                .collect(),
            display_offset: term.grid().display_offset(),
            cursor_point: term.grid().cursor.point,
            revision: 0,
            selectable_range: None,
            terminal_mode: *term.mode(),
            terminal_size,
            cursor: term.grid_mut().cursor_cell().clone(),
            hovered_hyperlink: None,
        };
        let term = Arc::new(FairMutex::new(term));
        // Keep final SSH error output when the child exits.
        let pty_event_loop = EventLoop::new(term.clone(), event_proxy, pty, true, false)?;
        let notifier = Notifier(pty_event_loop.channel());
        let pty_notifier = Notifier(pty_event_loop.channel());
        let url_regex = RegexSearch::new(r#"(ipfs:|ipns:|magnet:|mailto:|gemini://|gopher://|https://|http://|news:|file://|git://|ssh:|ftp://)[^\u{0000}-\u{001F}\u{007F}-\u{009F}<>"\s{-}\^⟨⟩`]+"#).unwrap();
        let _pty_event_loop_thread = pty_event_loop.spawn();
        let _pty_event_subscription = std::thread::Builder::new()
            .name(format!("pty_event_subscription_{}", id))
            .spawn(move || {
                while let Ok(event) = event_receiver.recv() {
                    // Content notifications are coalesced before entering this channel.
                    // Only lifecycle/protocol events need to reach the application queue.
                    if !matches!(event, Event::Wakeup)
                        && pty_event_proxy_sender.send((id, event.clone())).is_err()
                    {
                        break;
                    }
                    app_context.request_repaint();
                    match event {
                        Event::Exit => break,
                        Event::PtyWrite(pty) => pty_notifier.notify(pty.into_bytes()),
                        _ => {}
                    }
                }
            })?;

        Ok(Self {
            id,
            pty_id,
            url_regex,
            term: term.clone(),
            size: terminal_size,
            notifier: Some(notifier),
            last_content: initial_content,
            render_cache: Default::default(),
            activity,
            snapshot_dirty: true,
            synced_generation: 0,
            #[cfg(any(test, feature = "test-support"))]
            fixture_parser: Default::default(),
            #[cfg(any(test, feature = "test-support"))]
            fixture_input: Default::default(),
        })
    }

    /// Deterministic terminal using the real ANSI parser, without a PTY or network.
    #[cfg(any(test, feature = "test-support"))]
    pub fn in_memory(id: u64) -> Self {
        let (sender, _) = mpsc::channel();
        let activity = Arc::new(Activity::default());
        let term = Term::new(
            term::Config {
                scrolling_history: 2000,
                ..Default::default()
            },
            &TerminalSize::default(),
            EventProxy(sender, activity.clone()),
        );
        Self {
            id,
            pty_id: 0,
            url_regex: RegexSearch::new(r"https?://[^ ]+").unwrap(),
            term: Arc::new(FairMutex::new(term)),
            size: TerminalSize::default(),
            notifier: None,
            last_content: RenderableContent::default(),
            render_cache: Default::default(),
            activity,
            snapshot_dirty: true,
            synced_generation: 0,
            fixture_parser: Default::default(),
            fixture_input: Default::default(),
        }
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn feed_output(&mut self, bytes: &[u8]) {
        assert!(
            self.notifier.is_none(),
            "Only use fixture input on in-memory terminals"
        );
        self.fixture_parser.advance(&mut *self.term.lock(), bytes);
        self.activity.changed();
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn take_fixture_input(&self) -> Vec<u8> {
        std::mem::take(&mut *self.fixture_input.lock().unwrap())
    }

    pub fn set_visible(&mut self, visible: bool) {
        if self.activity.set_visible(visible) {
            self.snapshot_dirty = true;
        }
        if !visible && self.last_content.cells.capacity() != 0 {
            // Hidden tabs retain the emulator and scrollback, but have no use for
            // a second copy of visible cells, paint commands, or glyph layouts.
            // Drop allocations, rather than clear() which keeps their capacity.
            self.last_content.cells = Vec::new();
            self.render_cache = Default::default();
            self.snapshot_dirty = true;
        }
    }

    pub fn process_command(&mut self, cmd: BackendCommand) {
        if let BackendCommand::Resize(layout, font) = &cmd {
            if *layout == self.size.layout_size
                && font.width as u16 == self.size.cell_width
                && font.height as u16 == self.size.cell_height
            {
                return;
            }
        }
        self.snapshot_dirty = true;
        let term = self.term.clone();
        let mut term = term.lock();
        match cmd {
            BackendCommand::Write(input) => {
                self.write(input);
                term.scroll_display(Scroll::Bottom);
            }
            BackendCommand::Scroll(delta) => {
                self.scroll(&mut term, delta);
            }
            BackendCommand::MouseWheel {
                lines,
                modifiers,
                position,
            } => {
                if term.mode().intersects(TermMode::MOUSE_MODE) {
                    let point = Self::selection_point(position.x, position.y, &self.size, 0);
                    let button = if lines > 0 {
                        MouseButton::ScrollUp
                    } else {
                        MouseButton::ScrollDown
                    };
                    for _ in 0..lines.unsigned_abs() {
                        self.process_mouse_report(
                            *term.mode(),
                            button.clone(),
                            modifiers,
                            point,
                            true,
                        );
                    }
                } else {
                    self.scroll(&mut term, lines);
                }
            }
            BackendCommand::Resize(layout_size, font_size) => {
                self.resize(&mut term, layout_size, font_size);
            }
            BackendCommand::SelectStart(selection_type, x, y) => {
                self.start_selection(&mut term, selection_type, x, y);
            }
            BackendCommand::SelectUpdate(x, y) => {
                self.update_selection(&mut term, x, y);
            }
            BackendCommand::ProcessLink(link_action, point) => {
                self.process_link_action(&term, link_action, point);
            }
            BackendCommand::MouseReport(button, modifiers, point, pressed) => {
                self.process_mouse_report(*term.mode(), button, modifiers, point, pressed);
            }
        };
    }

    pub fn selection_point(
        x: f32,
        y: f32,
        terminal_size: &TerminalSize,
        display_offset: usize,
    ) -> Point {
        let col = (x as usize) / (terminal_size.cell_width as usize);
        let col = min(Column(col), Column(terminal_size.num_cols as usize - 1));

        let line = (y as usize) / (terminal_size.cell_height as usize);
        let line = min(line, terminal_size.num_lines as usize - 1);

        viewport_to_point(display_offset, Point::new(line, col))
    }

    pub fn selectable_content(&self) -> String {
        let content = self.last_content();
        let mut result = String::new();
        if let Some(range) = content.selectable_range {
            for indexed in &content.cells {
                if range.contains(indexed.point) {
                    result.push(indexed.c);
                }
            }
        }
        result
    }

    pub fn sync(&mut self) -> &RenderableContent {
        // Clear the notification latch before observing the generation. Output arriving
        // during this snapshot can schedule a subsequent frame without being lost.
        self.activity.begin_frame();
        let generation = self.activity.generation();
        if !self.snapshot_dirty && generation == self.synced_generation {
            return &self.last_content;
        }
        let term = self.term.clone();
        let mut terminal = term.lock();
        self.last_content.cells.clear();
        self.last_content
            .cells
            .extend(terminal.grid().display_iter().map(|indexed| Indexed {
                point: indexed.point,
                cell: indexed.cell.clone(),
            }));
        self.last_content.display_offset = terminal.grid().display_offset();
        self.last_content.cursor_point = terminal.grid().cursor.point;
        self.last_content.selectable_range = terminal
            .selection
            .as_ref()
            .and_then(|s| s.to_range(&terminal));
        self.last_content.cursor = terminal.grid_mut().cursor_cell().clone();
        self.last_content.terminal_mode = *terminal.mode();
        self.last_content.terminal_size = self.size;
        self.last_content.revision = self.last_content.revision.wrapping_add(1);
        self.synced_generation = generation;
        self.snapshot_dirty = false;
        &self.last_content
    }

    pub(crate) fn render_data(&mut self) -> (&RenderableContent, &mut crate::view::RenderCache) {
        self.sync();
        (&self.last_content, &mut self.render_cache)
    }

    pub fn last_content(&self) -> &RenderableContent {
        &self.last_content
    }

    pub fn id(&self) -> u64 {
        self.id
    }

    pub fn pty_id(&self) -> u32 {
        self.pty_id
    }

    fn process_link_action(
        &mut self,
        terminal: &Term<EventProxy>,
        link_action: LinkAction,
        point: Point,
    ) {
        match link_action {
            LinkAction::Hover => {
                self.last_content.hovered_hyperlink =
                    self.regex_match_at(terminal, point, &mut self.url_regex.clone());
            }
            LinkAction::Clear => {
                self.last_content.hovered_hyperlink = None;
            }
            LinkAction::Open => {
                self.open_link(terminal);
            }
        };
    }

    fn open_link(&self, terminal: &Term<EventProxy>) {
        if let Some(range) = &self.last_content.hovered_hyperlink {
            let start = range.start();
            let end = range.end();

            let mut url = String::from(terminal.grid().index(*start).c);
            for indexed in terminal.grid().iter_from(*start) {
                url.push(indexed.c);
                if indexed.point == *end {
                    break;
                }
            }

            open::that(url).unwrap_or_else(|_| {
                panic!("link opening is failed");
            })
        }
    }

    fn process_mouse_report(
        &self,
        mode: TermMode,
        button: MouseButton,
        modifiers: Modifiers,
        point: Point,
        pressed: bool,
    ) {
        let mut mods = 0;
        if modifiers.contains(Modifiers::SHIFT) {
            mods += 4;
        }
        if modifiers.contains(Modifiers::ALT) {
            mods += 8;
        }
        if modifiers.ctrl {
            mods += 16;
        }

        match MouseMode::from(mode) {
            MouseMode::Sgr => self.sgr_mouse_report(point, button as u8 + mods, pressed),
            MouseMode::Normal(is_utf8) => {
                if pressed {
                    self.normal_mouse_report(point, button as u8 + mods, is_utf8)
                } else {
                    self.normal_mouse_report(point, 3 + mods, is_utf8)
                }
            }
        }
    }

    fn sgr_mouse_report(&self, point: Point, button: u8, pressed: bool) {
        let c = if pressed { 'M' } else { 'm' };

        let msg = format!(
            "\x1b[<{};{};{}{}",
            button,
            point.column + 1,
            point.line + 1,
            c
        );

        self.write(msg.into_bytes());
    }

    fn normal_mouse_report(&self, point: Point, button: u8, is_utf8: bool) {
        let Point { line, column } = point;
        let max_point = if is_utf8 { 2015 } else { 223 };

        if line >= max_point || column >= max_point {
            return;
        }

        let mut msg = vec![b'\x1b', b'[', b'M', 32 + button];

        let mouse_pos_encode = |pos: usize| -> Vec<u8> {
            let pos = 32 + 1 + pos;
            let first = 0xC0 + pos / 64;
            let second = 0x80 + (pos & 63);
            vec![first as u8, second as u8]
        };

        if is_utf8 && column >= Column(95) {
            msg.append(&mut mouse_pos_encode(column.0));
        } else {
            msg.push(32 + 1 + column.0 as u8);
        }

        if is_utf8 && line >= 95 {
            msg.append(&mut mouse_pos_encode(line.0 as usize));
        } else {
            msg.push(32 + 1 + line.0 as u8);
        }

        self.write(msg);
    }

    fn start_selection(
        &mut self,
        terminal: &mut Term<EventProxy>,
        selection_type: SelectionType,
        x: f32,
        y: f32,
    ) {
        let location = Self::selection_point(x, y, &self.size, terminal.grid().display_offset());
        terminal.selection = Some(Selection::new(
            selection_type,
            location,
            self.selection_side(x),
        ));
    }

    fn update_selection(&mut self, terminal: &mut Term<EventProxy>, x: f32, y: f32) {
        let display_offset = terminal.grid().display_offset();
        if let Some(ref mut selection) = terminal.selection {
            let location = Self::selection_point(x, y, &self.size, display_offset);
            selection.update(location, self.selection_side(x));
        }
    }

    fn selection_side(&self, x: f32) -> Side {
        let cell_x = x as usize % self.size.cell_width as usize;
        let half_cell_width = (self.size.cell_width as f32 / 2.0) as usize;

        if cell_x > half_cell_width {
            Side::Right
        } else {
            Side::Left
        }
    }

    fn resize(&mut self, terminal: &mut Term<EventProxy>, layout_size: Size, font_size: Size) {
        if layout_size == self.size.layout_size
            && font_size.width as u16 == self.size.cell_width
            && font_size.height as u16 == self.size.cell_height
        {
            return;
        }

        let lines = (layout_size.height / font_size.height.floor()) as u16;
        let cols = (layout_size.width / font_size.width.floor()) as u16;
        if lines > 0 && cols > 0 {
            self.size = TerminalSize {
                layout_size,
                cell_height: font_size.height as u16,
                cell_width: font_size.width as u16,
                num_lines: lines,
                num_cols: cols,
            };

            if let Some(notifier) = &mut self.notifier {
                notifier.on_resize(self.size.into());
            }
            terminal.resize(TermSize::new(
                self.size.num_cols as usize,
                self.size.num_lines as usize,
            ));
        }
    }

    fn write<I: Into<Cow<'static, [u8]>>>(&self, input: I) {
        if let Some(notifier) = &self.notifier {
            notifier.notify(input);
        } else {
            #[cfg(any(test, feature = "test-support"))]
            self.fixture_input
                .lock()
                .unwrap()
                .extend_from_slice(&input.into());
        }
    }

    fn scroll(&mut self, terminal: &mut Term<EventProxy>, delta_value: i32) {
        if delta_value != 0 {
            let scroll = Scroll::Delta(delta_value);
            if terminal
                .mode()
                .contains(TermMode::ALTERNATE_SCROLL | TermMode::ALT_SCREEN)
            {
                let line_cmd = if delta_value > 0 { b'A' } else { b'B' };
                let mut content = vec![];

                for _ in 0..delta_value.abs() {
                    content.push(0x1b);
                    content.push(b'O');
                    content.push(line_cmd);
                }

                self.write(content);
            } else {
                terminal.grid_mut().scroll_display(scroll);
            }
        }
    }

    /// Based on alacritty/src/display/hint.rs > regex_match_at
    /// Retrieve the match, if the specified point is inside the content matching the regex.
    fn regex_match_at(
        &self,
        terminal: &Term<EventProxy>,
        point: Point,
        regex: &mut RegexSearch,
    ) -> Option<Match> {
        let x = visible_regex_match_iter(terminal, regex).find(|rm| rm.contains(&point));
        x
    }
}

/// Copied from alacritty/src/display/hint.rs:
/// Iterate over all visible regex matches.
fn visible_regex_match_iter<'a>(
    term: &'a Term<EventProxy>,
    regex: &'a mut RegexSearch,
) -> impl Iterator<Item = Match> + 'a {
    let viewport_start = Line(-(term.grid().display_offset() as i32));
    let viewport_end = viewport_start + term.bottommost_line();
    let mut start = term.line_search_left(Point::new(viewport_start, Column(0)));
    let mut end = term.line_search_right(Point::new(viewport_end, Column(0)));
    start.line = start.line.max(viewport_start - 100);
    end.line = end.line.min(viewport_end + 100);

    RegexIter::new(start, end, Direction::Right, term, regex)
        .skip_while(move |rm| rm.end().line < viewport_start)
        .take_while(move |rm| rm.start().line <= viewport_end)
}

pub struct RenderableContent {
    pub cells: Vec<Indexed<Cell>>,
    pub display_offset: usize,
    pub cursor_point: Point,
    pub revision: u64,
    pub hovered_hyperlink: Option<RangeInclusive<Point>>,
    pub selectable_range: Option<SelectionRange>,
    pub cursor: Cell,
    pub terminal_mode: TermMode,
    pub terminal_size: TerminalSize,
}

impl Default for RenderableContent {
    fn default() -> Self {
        Self {
            cells: vec![],
            display_offset: 0,
            cursor_point: Point::default(),
            revision: 0,
            hovered_hyperlink: None,
            selectable_range: None,
            cursor: Cell::default(),
            terminal_mode: TermMode::empty(),
            terminal_size: TerminalSize::default(),
        }
    }
}

impl Drop for TerminalBackend {
    fn drop(&mut self) {
        if let Some(notifier) = &self.notifier {
            let _ = notifier.0.send(Msg::Shutdown);
        }
    }
}

#[derive(Clone)]
pub struct EventProxy(mpsc::Sender<Event>, Arc<Activity>);

impl EventListener for EventProxy {
    fn send_event(&self, event: Event) {
        if matches!(
            event,
            Event::Wakeup | Event::MouseCursorDirty | Event::CursorBlinkingChange
        ) {
            self.1.changed();
            if self.1.queue_frame() {
                let _ = self.0.send(Event::Wakeup);
            }
        } else {
            let _ = self.0.send(event);
        }
    }
}

#[cfg(test)]
mod relay_terminal_tests {
    use super::*;

    #[test]
    fn wheel_reports_use_live_mouse_mode_coordinates_and_modifiers() {
        let mut backend = TerminalBackend::in_memory(1);
        backend.process_command(BackendCommand::Resize(
            Size {
                width: 800.0,
                height: 400.0,
            },
            Size {
                width: 10.0,
                height: 10.0,
            },
        ));
        backend.sync();
        // Intentionally do not sync after the mode change: input must use live modes.
        backend.feed_output(b"\x1b[?1049h\x1b[?1007h\x1b[?1000h\x1b[?1006h");
        backend.process_command(BackendCommand::MouseWheel {
            lines: 2,
            modifiers: Modifiers::NONE,
            position: egui::vec2(45.0, 65.0),
        });
        assert_eq!(backend.take_fixture_input(), b"\x1b[<64;5;7M\x1b[<64;5;7M");
        backend.process_command(BackendCommand::MouseWheel {
            lines: -1,
            modifiers: Modifiers {
                shift: true,
                alt: true,
                ctrl: true,
                ..Default::default()
            },
            position: egui::vec2(45.0, 65.0),
        });
        assert_eq!(backend.take_fixture_input(), b"\x1b[<93;5;7M");
        backend.process_command(BackendCommand::MouseWheel {
            lines: 1,
            modifiers: Modifiers::COMMAND,
            position: egui::vec2(45.0, 65.0),
        });
        assert_eq!(
            backend.take_fixture_input(),
            b"\x1b[<64;5;7M",
            "macOS Command is not the protocol's Control modifier"
        );
        backend.feed_output(b"\x1b[?1006l");
        backend.process_command(BackendCommand::MouseWheel {
            lines: -1,
            modifiers: Modifiers::NONE,
            position: egui::vec2(45.0, 65.0),
        });
        assert_eq!(
            backend.take_fixture_input(),
            b"\x1b[Ma%'",
            "legacy X10 wheel encoding"
        );
        backend.feed_output(b"\x1b[?1000l");
        backend.process_command(BackendCommand::MouseWheel {
            lines: 2,
            modifiers: Modifiers::NONE,
            position: egui::vec2(45.0, 65.0),
        });
        assert_eq!(
            backend.take_fixture_input(),
            b"\x1bOA\x1bOA",
            "retain alternate-screen fallback when mouse reporting is off"
        );
        backend.feed_output(b"\x1b[?1049l");
        backend.feed_output("history\r\n".repeat(100).as_bytes());
        backend.process_command(BackendCommand::MouseWheel {
            lines: 3,
            modifiers: Modifiers::NONE,
            position: egui::vec2(45.0, 65.0),
        });
        assert_eq!(backend.sync().display_offset, 3);
        assert!(backend.take_fixture_input().is_empty());
        backend.process_command(BackendCommand::MouseWheel {
            lines: -2,
            modifiers: Modifiers::NONE,
            position: egui::vec2(45.0, 65.0),
        });
        assert_eq!(backend.sync().display_offset, 1);
    }

    fn assert_snapshot_matches_terminal(backend: &mut TerminalBackend) {
        backend.sync();
        let terminal = backend.term.lock();
        let expected: Vec<_> = terminal
            .grid()
            .display_iter()
            .map(|c| (c.point, c.cell.clone()))
            .collect();
        let actual: Vec<_> = backend
            .last_content
            .cells
            .iter()
            .map(|c| (c.point, c.cell.clone()))
            .collect();
        assert_eq!(actual, expected);
        assert_eq!(
            backend.last_content.display_offset,
            terminal.grid().display_offset()
        );
        assert_eq!(
            backend.last_content.cursor_point,
            terminal.grid().cursor.point
        );
        assert_eq!(actual.len(), terminal.screen_lines() * terminal.columns());
    }

    #[test]
    fn hiding_releases_snapshot_without_losing_history_selection_or_hidden_output() {
        let mut backend = TerminalBackend::in_memory(1);
        backend.feed_output("\x1b[31mhistory café\x1b[0m\r\n".repeat(2100).as_bytes());
        backend.process_command(BackendCommand::Scroll(12));
        backend.process_command(BackendCommand::SelectStart(SelectionType::Simple, 0.0, 0.0));
        backend.process_command(BackendCommand::SelectUpdate(8.0, 0.0));
        let before = backend.sync();
        let cells: Vec<_> = before.cells.iter().map(|c| c.cell.clone()).collect();
        let display_offset = before.display_offset;
        let selectable_range = before.selectable_range;
        for _ in 0..3 {
            backend.set_visible(false);
            assert_eq!(backend.last_content.cells.capacity(), 0);
            assert_eq!(backend.term.lock().grid().history_size(), 2000);
            backend.set_visible(true);
            assert_snapshot_matches_terminal(&mut backend);
            assert_eq!(backend.last_content.display_offset, display_offset);
            assert_eq!(backend.last_content.selectable_range, selectable_range);
            assert_eq!(backend.last_content.cells.len(), cells.len());
            for (after, before) in backend.last_content.cells.iter().zip(&cells) {
                assert_eq!(&after.cell, before);
            }
        }
        backend.set_visible(false);
        backend.feed_output(b"hidden output\r\n");
        assert_eq!(backend.last_content.cells.capacity(), 0);
        backend.set_visible(true);
        backend.process_command(BackendCommand::Scroll(-2000));
        assert_snapshot_matches_terminal(&mut backend);
        let text: String = backend
            .last_content
            .cells
            .iter()
            .map(|cell| cell.c)
            .collect();
        assert!(text.contains("hidden output"));
    }

    #[test]
    fn snapshots_preserve_unicode_colors_scrollback_selection_resize_and_alt_screen() {
        let mut backend = TerminalBackend::in_memory(1);
        backend.feed_output(
            "\x1b[31mred\x1b[0m café 日本語 🦀 e\u{301}\r\n"
                .repeat(2500)
                .as_bytes(),
        );
        assert_snapshot_matches_terminal(&mut backend);
        assert_eq!(backend.term.lock().grid().history_size(), 2000);
        assert!(backend.last_content.cells.iter().any(|c| c
            .zerowidth()
            .is_some_and(|chars| chars.contains(&'\u{301}'))));
        let revision = backend.sync().revision;
        let allocation = backend.last_content.cells.as_ptr();
        for _ in 0..100 {
            assert_eq!(backend.sync().revision, revision);
        }
        backend.feed_output(b"new output");
        assert_snapshot_matches_terminal(&mut backend);
        assert_eq!(
            backend.last_content.cells.as_ptr(),
            allocation,
            "Reuse the visible-cell buffer"
        );
        backend.process_command(BackendCommand::Scroll(20));
        assert_snapshot_matches_terminal(&mut backend);
        assert_eq!(backend.last_content.display_offset, 20);
        backend.process_command(BackendCommand::SelectStart(SelectionType::Simple, 0.0, 0.0));
        backend.process_command(BackendCommand::SelectUpdate(5.0, 0.0));
        assert_snapshot_matches_terminal(&mut backend);
        assert!(!backend.selectable_content().is_empty());
        backend.process_command(BackendCommand::Resize(
            Size::new(640.0, 320.0),
            Size::new(8.0, 16.0),
        ));
        assert_snapshot_matches_terminal(&mut backend);
        assert_eq!(backend.last_content.cells.len(), 80 * 20);
        let revision = backend.sync().revision;
        backend.process_command(BackendCommand::Resize(
            Size::new(640.0, 320.0),
            Size::new(8.0, 16.0),
        ));
        assert_eq!(
            backend.sync().revision,
            revision,
            "Unchanged sizing must not dirty the snapshot"
        );
        backend.feed_output(b"\x1b[?1049hALT SCREEN");
        assert_snapshot_matches_terminal(&mut backend);
        assert!(backend
            .last_content
            .terminal_mode
            .contains(TermMode::ALT_SCREEN));
        backend.feed_output(b"\x1b[?1049l");
        assert_snapshot_matches_terminal(&mut backend);
        assert!(!backend
            .last_content
            .terminal_mode
            .contains(TermMode::ALT_SCREEN));
    }

    #[test]
    fn inactive_output_is_retained_without_flooding_ui_notifications() {
        let mut terminals: Vec<_> = (0..10).map(TerminalBackend::in_memory).collect();
        let (sender, receiver) = mpsc::channel();
        for (index, terminal) in terminals.iter_mut().enumerate() {
            terminal.set_visible(index == 0);
            let proxy = EventProxy(sender.clone(), terminal.activity.clone());
            for _ in 0..1000 {
                proxy.send_event(Event::Wakeup);
            }
        }
        assert_eq!(
            receiver.try_iter().count(),
            1,
            "Coalesce active output and suppress inactive notifications"
        );
        let proxy = EventProxy(sender.clone(), terminals[0].activity.clone());
        terminals[0].sync();
        proxy.send_event(Event::Wakeup);
        assert!(
            matches!(receiver.try_recv(), Ok(Event::Wakeup)),
            "New output after a snapshot can schedule another frame"
        );
        terminals[9].feed_output(b"background output survived");
        let exit_proxy = EventProxy(sender, terminals[9].activity.clone());
        exit_proxy.send_event(Event::Exit);
        assert!(
            matches!(receiver.try_recv(), Ok(Event::Exit)),
            "Hidden tab exits still reach the app"
        );
        terminals[9].set_visible(true);
        let content = terminals[9]
            .sync()
            .cells
            .iter()
            .map(|c| c.c)
            .collect::<String>();
        assert!(content.contains("background output survived"));
    }
}
