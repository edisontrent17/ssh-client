use alacritty_terminal::index::Point as TerminalGridPoint;
use alacritty_terminal::term::cell;
use alacritty_terminal::term::TermMode;
use alacritty_terminal::vte::ansi::{Color, NamedColor};
use egui::epaint::RectShape;
use egui::Modifiers;
use egui::MouseWheelUnit;
use egui::Shape;
use egui::Widget;
use egui::{Align2, Painter, Pos2, Rect, Response, Stroke, Vec2};
use egui::{CornerRadius, Key};
use egui::{Id, PointerButton};

use crate::backend::BackendCommand;
use crate::backend::TerminalBackend;
use crate::backend::{LinkAction, MouseButton, SelectionType};
use crate::bindings::Binding;
use crate::bindings::{BindingAction, BindingsLayout, InputKind};
use crate::font::TerminalFont;
use crate::theme::TerminalTheme;
use crate::types::Size;
use std::{collections::HashMap, sync::Arc};

const EGUI_TERM_WIDGET_ID_PREFIX: &str = "egui_term::instance::";

#[derive(Debug, Clone)]
enum InputAction {
    BackendCall(BackendCommand),
    WriteToClipboard(String),
    Ignore,
}

#[derive(Clone, Default)]
pub struct TerminalViewState {
    is_dragged: bool,
    scroll_pixels: f32,
    current_mouse_position_on_grid: TerminalGridPoint,
}

/// Owned by the backend so closing a tab releases its retained paint data.
#[derive(Default)]
pub(crate) struct RenderCache {
    paint_key: Option<PaintKey>,
    shapes: Vec<Shape>,
    glyphs: GlyphCache,
    #[cfg(test)]
    builds: usize,
}

#[derive(Default)]
struct GlyphCache {
    font: Option<(egui::FontId, f32)>,
    galleys: HashMap<char, Arc<egui::Galley>>,
}

impl GlyphCache {
    fn prepare(&mut self, font: egui::FontId, pixels_per_point: f32) {
        if self.font.as_ref() != Some(&(font.clone(), pixels_per_point)) {
            self.galleys.clear();
            self.font = Some((font, pixels_per_point));
        }
    }

    fn layout(&mut self, character: char, painter: &Painter) -> Arc<egui::Galley> {
        if let Some(galley) = self.galleys.get(&character) {
            return galley.clone();
        }
        // Limit retained glyphs even when remote output contains many unique characters.
        if self.galleys.len() >= 2048 {
            self.galleys.clear();
        }
        let font = self.font.as_ref().expect("prepared glyph cache").0.clone();
        let galley = painter.fonts_mut(|fonts| {
            fonts.layout_no_wrap(character.to_string(), font, egui::Color32::PLACEHOLDER)
        });
        self.galleys.insert(character, galley.clone());
        galley
    }
}

#[derive(Clone, PartialEq)]
struct PaintKey {
    revision: u64,
    rect: Rect,
    pixels_per_point: f32,
    font: egui::FontId,
    theme: TerminalTheme,
    mouse: TerminalGridPoint,
}

pub struct TerminalView<'a> {
    widget_id: Id,
    has_focus: bool,
    size: Vec2,
    backend: &'a mut TerminalBackend,
    font: TerminalFont,
    theme: TerminalTheme,
    bindings_layout: BindingsLayout,
}

impl Widget for TerminalView<'_> {
    fn ui(self, ui: &mut egui::Ui) -> Response {
        let (layout, painter) = ui.allocate_painter(self.size, egui::Sense::click());

        let widget_id = self.widget_id;
        let mut state = ui.memory(|m| {
            m.data
                .get_temp::<TerminalViewState>(widget_id)
                .unwrap_or_default()
        });

        self.focus(&layout)
            .resize(&layout)
            .process_input(&layout, &mut state)
            .show(&mut state, &layout, &painter);

        ui.memory_mut(|m| m.data.insert_temp(widget_id, state));
        layout
    }
}

impl<'a> TerminalView<'a> {
    pub fn new(ui: &mut egui::Ui, backend: &'a mut TerminalBackend) -> Self {
        let widget_id =
            ui.make_persistent_id(format!("{}{}", EGUI_TERM_WIDGET_ID_PREFIX, backend.id()));

        Self {
            widget_id,
            has_focus: false,
            size: ui.available_size(),
            backend,
            font: TerminalFont::default(),
            theme: TerminalTheme::default(),
            bindings_layout: BindingsLayout::new(),
        }
    }

    #[inline]
    pub fn set_theme(mut self, theme: TerminalTheme) -> Self {
        self.theme = theme;
        self
    }

    #[inline]
    pub fn set_font(mut self, font: TerminalFont) -> Self {
        self.font = font;
        self
    }

    #[inline]
    pub fn set_focus(mut self, has_focus: bool) -> Self {
        self.has_focus = has_focus;
        self
    }

    #[inline]
    pub fn set_size(mut self, size: Vec2) -> Self {
        self.size = size;
        self
    }

    #[inline]
    pub fn add_bindings(mut self, bindings: Vec<(Binding<InputKind>, BindingAction)>) -> Self {
        self.bindings_layout.add_bindings(bindings);
        self
    }

    fn focus(self, layout: &Response) -> Self {
        if layout.enabled() && (self.has_focus || layout.clicked()) {
            layout.request_focus();
        }

        self
    }

    fn resize(self, layout: &Response) -> Self {
        self.backend.process_command(BackendCommand::Resize(
            Size::from(layout.rect.size()),
            self.font.font_measure(&layout.ctx),
        ));

        self
    }

    fn process_input(self, layout: &Response, state: &mut TerminalViewState) -> Self {
        if !layout.enabled() || !layout.has_focus() {
            return self;
        }

        let modifiers = layout.ctx.input(|i| i.modifiers);
        let events = layout.ctx.input(|i| i.events.clone());
        for event in events {
            if !layout.contains_pointer()
                && matches!(
                    event,
                    egui::Event::MouseWheel { .. }
                        | egui::Event::PointerButton { .. }
                        | egui::Event::PointerMoved(_)
                )
            {
                continue;
            }
            let mut input_actions = vec![];

            match event {
                egui::Event::Text(_)
                | egui::Event::Key { .. }
                | egui::Event::Copy
                | egui::Event::Paste(_) => input_actions.push(process_keyboard_event(
                    event,
                    self.backend,
                    &self.bindings_layout,
                    modifiers,
                )),
                egui::Event::MouseWheel {
                    unit,
                    delta,
                    modifiers,
                    ..
                } => {
                    if let Some(position) = layout.ctx.pointer_hover_pos() {
                        input_actions.push(process_mouse_wheel(
                            state,
                            self.font.font_type().size,
                            unit,
                            delta,
                            modifiers,
                            position - layout.rect.min,
                        ));
                    }
                }
                egui::Event::PointerButton {
                    button,
                    pressed,
                    modifiers,
                    pos,
                    ..
                } => input_actions.push(process_button_click(
                    state,
                    layout,
                    self.backend,
                    &self.bindings_layout,
                    button,
                    pos,
                    &modifiers,
                    pressed,
                )),
                egui::Event::PointerMoved(pos) => {
                    input_actions = process_mouse_move(state, layout, self.backend, pos, &modifiers)
                }
                _ => {}
            };

            for action in input_actions {
                match action {
                    InputAction::BackendCall(cmd) => {
                        self.backend.process_command(cmd);
                    }
                    InputAction::WriteToClipboard(data) => {
                        layout.ctx.copy_text(data);
                    }
                    InputAction::Ignore => {}
                }
            }
        }

        self
    }

    fn show(self, state: &mut TerminalViewState, layout: &Response, painter: &Painter) {
        let (content, cache) = self.backend.render_data();
        let key = PaintKey {
            revision: content.revision,
            rect: layout.rect,
            pixels_per_point: layout.ctx.pixels_per_point(),
            font: self.font.font_type(),
            theme: self.theme.clone(),
            mouse: state.current_mouse_position_on_grid,
        };
        if cache.paint_key.as_ref() == Some(&key) {
            painter.extend(cache.shapes.iter().cloned());
            return;
        }
        cache.glyphs.prepare(key.font.clone(), key.pixels_per_point);
        #[cfg(test)]
        {
            cache.builds += 1;
        }
        let layout_min = layout.rect.min;
        let layout_max = layout.rect.max;
        let cell_height = content.terminal_size.cell_height as f32;
        let cell_width = content.terminal_size.cell_width as f32;
        let global_bg = self.theme.get_color(Color::Named(NamedColor::Background));

        let mut shapes = std::mem::take(&mut cache.shapes);
        shapes.clear();
        shapes.push(Shape::Rect(RectShape::filled(
            Rect::from_min_max(layout_min, layout_max),
            CornerRadius::ZERO,
            global_bg,
        )));

        for indexed in &content.cells {
            let flags = indexed.cell.flags;
            let is_wide_char_spacer = flags.contains(cell::Flags::WIDE_CHAR_SPACER);
            if is_wide_char_spacer {
                continue;
            }

            let is_app_cursor_mode = content.terminal_mode.contains(TermMode::APP_CURSOR);
            let is_wide_char = flags.contains(cell::Flags::WIDE_CHAR);
            let is_inverse = flags.contains(cell::Flags::INVERSE);
            let is_dim = flags.intersects(cell::Flags::DIM | cell::Flags::DIM_BOLD);
            let is_selected = content
                .selectable_range
                .is_some_and(|r| r.contains(indexed.point));
            let is_hovered_hyperling = content.hovered_hyperlink.as_ref().is_some_and(|r| {
                r.contains(&indexed.point) && r.contains(&state.current_mouse_position_on_grid)
            });

            let x = layout_min.x + (cell_width * indexed.point.column.0 as f32);
            let line_num = indexed.point.line.0 + content.display_offset as i32;
            let y = layout_min.y + (cell_height * line_num as f32);

            let mut fg = self.theme.get_color(indexed.fg);
            let mut bg = self.theme.get_color(indexed.bg);
            let cell_width = if is_wide_char {
                cell_width * 2.0
            } else {
                cell_width
            };

            if is_dim {
                fg = fg.linear_multiply(0.7);
            }

            if is_inverse || is_selected {
                std::mem::swap(&mut fg, &mut bg);
            }

            if global_bg != bg {
                shapes.push(Shape::Rect(RectShape::filled(
                    Rect::from_min_size(
                        Pos2::new(x, y),
                        // + 1.0 is to fill grid border
                        Vec2::new(cell_width + 1., cell_height + 1.),
                    ),
                    CornerRadius::ZERO,
                    bg,
                )));
            }

            // Handle hovered hyperlink underline
            if is_hovered_hyperling {
                let underline_height = y + cell_height;
                shapes.push(Shape::LineSegment {
                    points: [
                        Pos2::new(x, underline_height),
                        Pos2::new(x + cell_width, underline_height),
                    ],
                    stroke: Stroke::new(cell_height * 0.15, fg),
                });
            }

            // Handle cursor rendering
            if content.cursor_point == indexed.point {
                let cursor_color = self.theme.get_color(content.cursor.fg);
                shapes.push(Shape::Rect(RectShape::filled(
                    Rect::from_min_size(Pos2::new(x, y), Vec2::new(cell_width, cell_height)),
                    CornerRadius::default(),
                    cursor_color,
                )));
            }

            // Draw text content
            if indexed.c != ' ' && indexed.c != '\t' {
                if content.cursor_point == indexed.point && is_app_cursor_mode {
                    std::mem::swap(&mut fg, &mut bg);
                }

                let galley = cache.glyphs.layout(indexed.c, painter);
                let rect = Align2::CENTER_TOP
                    .anchor_size(Pos2::new(x + cell_width / 2.0, y), galley.size());
                shapes.push(Shape::Text(egui::epaint::TextShape::new(
                    rect.min, galley, fg,
                )));
            }
        }

        cache.paint_key = Some(key);
        cache.shapes = shapes;
        painter.extend(cache.shapes.iter().cloned());
    }
}

fn process_keyboard_event(
    event: egui::Event,
    backend: &TerminalBackend,
    bindings_layout: &BindingsLayout,
    modifiers: Modifiers,
) -> InputAction {
    match event {
        egui::Event::Text(text) => process_text_event(&text, modifiers, backend, bindings_layout),
        egui::Event::Paste(text) => InputAction::BackendCall(
            #[cfg(not(any(target_os = "ios", target_os = "macos")))]
            if modifiers.contains(Modifiers::COMMAND | Modifiers::SHIFT) {
                BackendCommand::Write(paste_bytes(&text, backend.last_content().terminal_mode))
            } else {
                // Hotfix - Send ^V when there's not selection on view.
                BackendCommand::Write([0x16].to_vec())
            },
            #[cfg(any(target_os = "ios", target_os = "macos"))]
            {
                BackendCommand::Write(paste_bytes(&text, backend.last_content().terminal_mode))
            },
        ),
        egui::Event::Copy => {
            #[cfg(not(any(target_os = "ios", target_os = "macos")))]
            if modifiers.contains(Modifiers::COMMAND | Modifiers::SHIFT) {
                let content = backend.selectable_content();
                InputAction::WriteToClipboard(content)
            } else {
                // Hotfix - Send ^C when there's not selection on view.
                InputAction::BackendCall(BackendCommand::Write([0x3].to_vec()))
            }
            #[cfg(any(target_os = "ios", target_os = "macos"))]
            {
                let content = backend.selectable_content();
                InputAction::WriteToClipboard(content)
            }
        }
        egui::Event::Key {
            key,
            pressed,
            modifiers,
            ..
        } => process_keyboard_key(backend, bindings_layout, key, modifiers, pressed),
        _ => InputAction::Ignore,
    }
}

fn paste_bytes(text: &str, mode: TermMode) -> Vec<u8> {
    let text = text.replace('\x1b', "").replace("\r\n", "\n");
    if mode.contains(TermMode::BRACKETED_PASTE) {
        format!("\x1b[200~{text}\x1b[201~").into_bytes()
    } else {
        text.replace('\n', "\r").into_bytes()
    }
}

#[cfg(test)]
mod relay_paste_tests {
    use super::*;
    #[test]
    fn wraps_multiline_paste_and_removes_embedded_escape_sequences() {
        assert_eq!(
            paste_bytes("a\r\nb\x1b[201~", TermMode::BRACKETED_PASTE),
            b"\x1b[200~a\nb[201~\x1b[201~"
        );
        assert_eq!(paste_bytes("a\r\nb", TermMode::empty()), b"a\rb");
    }
}

#[cfg(test)]
mod relay_render_tests {
    use super::*;

    #[test]
    fn wheel_input_reaches_the_backend_only_inside_enabled_terminal() {
        let ctx = egui::Context::default();
        let mut backend = TerminalBackend::in_memory(1);
        backend.feed_output(b"\x1b[?1000h\x1b[?1006h");
        let render = |backend: &mut TerminalBackend, enabled, events| {
            let mut rect = Rect::NOTHING;
            let _ = ctx.run_ui(
                egui::RawInput {
                    screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(800.0, 400.0))),
                    events,
                    ..Default::default()
                },
                |ui| {
                    let view = TerminalView::new(ui, backend).set_focus(true);
                    rect = ui.add_enabled(enabled, view).rect;
                },
            );
            rect
        };
        for _ in 0..3 {
            render(&mut backend, true, vec![]);
        }
        let rect = render(&mut backend, true, vec![]);
        let size = backend.last_content().terminal_size;
        let pos = rect.min + Vec2::new(size.cell_width as f32 * 4.5, size.cell_height as f32 * 6.5);
        render(&mut backend, true, vec![egui::Event::PointerMoved(pos)]);
        let wheel = |unit, y, modifiers| egui::Event::MouseWheel {
            unit,
            delta: Vec2::new(0.0, y),
            modifiers,
            phase: egui::TouchPhase::Move,
        };
        render(
            &mut backend,
            true,
            vec![wheel(MouseWheelUnit::Line, 2.0, Modifiers::NONE)],
        );
        assert_eq!(backend.take_fixture_input(), b"\x1b[<64;5;7M\x1b[<64;5;7M");
        render(
            &mut backend,
            true,
            vec![wheel(MouseWheelUnit::Line, -1.0, Modifiers::CTRL)],
        );
        assert_eq!(backend.take_fixture_input(), b"\x1b[<81;5;7M");
        // Small macOS trackpad deltas accumulate instead of disappearing.
        let font_size = TerminalFont::default().font_type().size;
        for _ in 0..3 {
            render(
                &mut backend,
                true,
                vec![wheel(
                    MouseWheelUnit::Point,
                    font_size / 4.0,
                    Modifiers::NONE,
                )],
            );
            assert!(backend.take_fixture_input().is_empty());
        }
        render(
            &mut backend,
            true,
            vec![wheel(
                MouseWheelUnit::Point,
                font_size / 4.0,
                Modifiers::NONE,
            )],
        );
        assert_eq!(backend.take_fixture_input(), b"\x1b[<64;5;7M");
        render(
            &mut backend,
            true,
            vec![wheel(MouseWheelUnit::Point, -font_size, Modifiers::NONE)],
        );
        assert_eq!(backend.take_fixture_input(), b"\x1b[<65;5;7M");
        render(
            &mut backend,
            false,
            vec![wheel(MouseWheelUnit::Line, 1.0, Modifiers::NONE)],
        );
        assert!(backend.take_fixture_input().is_empty());
        render(
            &mut backend,
            true,
            vec![
                egui::Event::PointerMoved(Pos2::new(900.0, 500.0)),
                wheel(MouseWheelUnit::Line, 1.0, Modifiers::NONE),
            ],
        );
        assert!(backend.take_fixture_input().is_empty());
    }

    #[test]
    fn cached_glyphs_match_egui_text_meshes_and_have_a_fixed_retention_limit() {
        let ctx = egui::Context::default();
        let mut cache = GlyphCache::default();
        let mut pairs = vec![];
        let _ = ctx.run_ui(egui::RawInput::default(), |ui| {
            let font = egui::FontId::monospace(14.0);
            cache.prepare(font.clone(), ctx.pixels_per_point());
            for character in ['A', 'é', '日', '🦀'] {
                for color in [egui::Color32::RED, egui::Color32::from_rgb(12, 34, 56)] {
                    let position = Pos2::new(40.0, 20.0);
                    let reference = ui.fonts_mut(|fonts| {
                        Shape::text(
                            fonts,
                            position,
                            Align2::CENTER_TOP,
                            character,
                            font.clone(),
                            color,
                        )
                    });
                    let galley = cache.layout(character, ui.painter());
                    let rect = Align2::CENTER_TOP.anchor_size(position, galley.size());
                    let cached = Shape::Text(egui::epaint::TextShape::new(rect.min, galley, color));
                    pairs.push((reference, cached));
                }
            }
        });
        for (reference, cached) in pairs {
            let tessellate = |shape| {
                ctx.tessellate(
                    vec![egui::epaint::ClippedShape {
                        clip_rect: Rect::EVERYTHING,
                        shape,
                    }],
                    ctx.pixels_per_point(),
                )
            };
            let expected = tessellate(reference);
            let actual = tessellate(cached);
            assert_eq!(actual.len(), expected.len());
            for (a, b) in actual.iter().zip(&expected) {
                let (egui::epaint::Primitive::Mesh(a), egui::epaint::Primitive::Mesh(b)) =
                    (&a.primitive, &b.primitive)
                else {
                    panic!("Expected text meshes")
                };
                assert_eq!(a.vertices, b.vertices);
                assert_eq!(a.indices, b.indices);
                assert_eq!(a.texture_id, b.texture_id);
            }
        }
        let _ = ctx.run_ui(egui::RawInput::default(), |ui| {
            for codepoint in 0x400..0x400 + 2200 {
                cache.layout(char::from_u32(codepoint).unwrap(), ui.painter());
            }
            assert!(cache.galleys.len() <= 2048);
            cache.prepare(egui::FontId::monospace(20.0), ctx.pixels_per_point());
            assert!(cache.galleys.is_empty());
        });
    }

    #[test]
    fn terminal_cache_invalidates_for_output_selection_layout_font_theme_and_dpi() {
        let ctx = egui::Context::default();
        let mut backend = TerminalBackend::in_memory(1);
        let theme = TerminalTheme::default();
        let render = |backend: &mut TerminalBackend, width, font, theme: TerminalTheme, events| {
            ctx.run_ui(
                egui::RawInput {
                    screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(width, 400.0))),
                    events,
                    ..Default::default()
                },
                |ui| {
                    let view = TerminalView::new(ui, backend)
                        .set_theme(theme.clone())
                        .set_font(TerminalFont::new(crate::FontSettings {
                            font_type: egui::FontId::monospace(font),
                        }))
                        .set_focus(true);
                    ui.add(view);
                },
            )
        };
        backend.feed_output("\x1b[31mHello\x1b[0m café 日本語\r\n".as_bytes());
        for _ in 0..3 {
            render(&mut backend, 800.0, 14.0, theme.clone(), vec![]);
        }
        let builds = backend.render_data().1.builds;
        backend.set_visible(false);
        assert_eq!(backend.last_content().cells.capacity(), 0);
        // render_data rebuilds the cell snapshot, but not the drawing cache.
        let cache = backend.render_data().1;
        assert_eq!(cache.shapes.capacity(), 0);
        assert_eq!(cache.glyphs.galleys.capacity(), 0);
        assert!(cache.paint_key.is_none());
        backend.set_visible(true);
        render(&mut backend, 800.0, 14.0, theme.clone(), vec![]);
        assert_eq!(backend.render_data().1.builds, builds);
        for _ in 0..10 {
            render(&mut backend, 800.0, 14.0, theme.clone(), vec![]);
        }
        assert_eq!(backend.render_data().1.builds, builds);
        backend.feed_output(b"new");
        render(&mut backend, 800.0, 14.0, theme.clone(), vec![]);
        assert_eq!(backend.render_data().1.builds, builds + 1);
        backend.process_command(BackendCommand::SelectStart(SelectionType::Simple, 0.0, 0.0));
        backend.process_command(BackendCommand::SelectUpdate(40.0, 0.0));
        render(&mut backend, 800.0, 14.0, theme.clone(), vec![]);
        assert_eq!(backend.render_data().1.builds, builds + 2);
        render(&mut backend, 700.0, 14.0, theme.clone(), vec![]);
        assert_eq!(backend.render_data().1.builds, builds + 3);
        render(&mut backend, 700.0, 16.0, theme.clone(), vec![]);
        assert_eq!(backend.render_data().1.builds, builds + 4);
        let changed = TerminalTheme::new(Box::new(crate::ColorPalette {
            foreground: "#123456".into(),
            ..Default::default()
        }));
        render(&mut backend, 700.0, 16.0, changed.clone(), vec![]);
        assert_eq!(backend.render_data().1.builds, builds + 5);
        ctx.set_pixels_per_point(2.0);
        render(&mut backend, 700.0, 16.0, changed.clone(), vec![]);
        assert_eq!(backend.render_data().1.builds, builds + 6);
        render(
            &mut backend,
            700.0,
            16.0,
            changed.clone(),
            vec![egui::Event::Text("z".into())],
        );
        assert_eq!(backend.take_fixture_input(), b"z");
        backend.feed_output(b"\x1b[?2004h");
        render(&mut backend, 700.0, 16.0, changed.clone(), vec![]);
        render(
            &mut backend,
            700.0,
            16.0,
            changed,
            vec![egui::Event::Paste("a\nb".into())],
        );
        assert_eq!(backend.take_fixture_input(), b"\x1b[200~a\nb\x1b[201~");
    }
}

fn process_text_event(
    text: &str,
    modifiers: Modifiers,
    backend: &TerminalBackend,
    bindings_layout: &BindingsLayout,
) -> InputAction {
    if let Some(key) = Key::from_name(text) {
        if bindings_layout.get_action(
            InputKind::KeyCode(key),
            modifiers,
            backend.last_content().terminal_mode,
        ) == BindingAction::Ignore
        {
            InputAction::BackendCall(BackendCommand::Write(text.as_bytes().to_vec()))
        } else {
            InputAction::Ignore
        }
    } else {
        InputAction::BackendCall(BackendCommand::Write(text.as_bytes().to_vec()))
    }
}

fn process_keyboard_key(
    backend: &TerminalBackend,
    bindings_layout: &BindingsLayout,
    key: Key,
    modifiers: Modifiers,
    pressed: bool,
) -> InputAction {
    if !pressed {
        return InputAction::Ignore;
    }

    let terminal_mode = backend.last_content().terminal_mode;
    let binding_action =
        bindings_layout.get_action(InputKind::KeyCode(key), modifiers, terminal_mode);

    match binding_action {
        BindingAction::Char(c) => {
            let mut buf = [0, 0, 0, 0];
            let str = c.encode_utf8(&mut buf);
            InputAction::BackendCall(BackendCommand::Write(str.as_bytes().to_vec()))
        }
        BindingAction::Esc(seq) => {
            InputAction::BackendCall(BackendCommand::Write(seq.as_bytes().to_vec()))
        }
        _ => InputAction::Ignore,
    }
}

fn process_mouse_wheel(
    state: &mut TerminalViewState,
    font_size: f32,
    unit: MouseWheelUnit,
    delta: Vec2,
    modifiers: Modifiers,
    position: Vec2,
) -> InputAction {
    let lines = match unit {
        MouseWheelUnit::Line => {
            let lines = delta.y.signum() * delta.y.abs().ceil();
            lines as i32
        }
        MouseWheelUnit::Point => {
            state.scroll_pixels -= delta.y;
            let lines = (state.scroll_pixels / font_size).trunc();
            state.scroll_pixels %= font_size;
            -lines as i32
        }
        MouseWheelUnit::Page => 0,
    };
    if lines == 0 {
        InputAction::Ignore
    } else {
        InputAction::BackendCall(BackendCommand::MouseWheel {
            lines,
            modifiers,
            position,
        })
    }
}

fn process_button_click(
    state: &mut TerminalViewState,
    layout: &Response,
    backend: &TerminalBackend,
    bindings_layout: &BindingsLayout,
    button: PointerButton,
    position: Pos2,
    modifiers: &Modifiers,
    pressed: bool,
) -> InputAction {
    match button {
        PointerButton::Primary => process_left_button(
            state,
            layout,
            backend,
            bindings_layout,
            position,
            modifiers,
            pressed,
        ),
        _ => InputAction::Ignore,
    }
}

fn process_left_button(
    state: &mut TerminalViewState,
    layout: &Response,
    backend: &TerminalBackend,
    bindings_layout: &BindingsLayout,
    position: Pos2,
    modifiers: &Modifiers,
    pressed: bool,
) -> InputAction {
    let terminal_mode = backend.last_content().terminal_mode;
    if terminal_mode.intersects(TermMode::MOUSE_MODE) {
        InputAction::BackendCall(BackendCommand::MouseReport(
            MouseButton::LeftButton,
            *modifiers,
            state.current_mouse_position_on_grid,
            pressed,
        ))
    } else if pressed {
        process_left_button_pressed(state, layout, position)
    } else {
        process_left_button_released(state, layout, backend, bindings_layout, position, modifiers)
    }
}

fn process_left_button_pressed(
    state: &mut TerminalViewState,
    layout: &Response,
    position: Pos2,
) -> InputAction {
    state.is_dragged = true;
    InputAction::BackendCall(build_start_select_command(layout, position))
}

fn process_left_button_released(
    state: &mut TerminalViewState,
    layout: &Response,
    backend: &TerminalBackend,
    bindings_layout: &BindingsLayout,
    position: Pos2,
    modifiers: &Modifiers,
) -> InputAction {
    state.is_dragged = false;
    if layout.double_clicked() || layout.triple_clicked() {
        InputAction::BackendCall(build_start_select_command(layout, position))
    } else {
        let terminal_content = backend.last_content();
        let binding_action = bindings_layout.get_action(
            InputKind::Mouse(PointerButton::Primary),
            *modifiers,
            terminal_content.terminal_mode,
        );

        if binding_action == BindingAction::LinkOpen {
            InputAction::BackendCall(BackendCommand::ProcessLink(
                LinkAction::Open,
                state.current_mouse_position_on_grid,
            ))
        } else {
            InputAction::Ignore
        }
    }
}

fn build_start_select_command(layout: &Response, cursor_position: Pos2) -> BackendCommand {
    let selection_type = if layout.double_clicked() {
        SelectionType::Semantic
    } else if layout.triple_clicked() {
        SelectionType::Lines
    } else {
        SelectionType::Simple
    };

    BackendCommand::SelectStart(
        selection_type,
        cursor_position.x - layout.rect.min.x,
        cursor_position.y - layout.rect.min.y,
    )
}

fn process_mouse_move(
    state: &mut TerminalViewState,
    layout: &Response,
    backend: &TerminalBackend,
    position: Pos2,
    modifiers: &Modifiers,
) -> Vec<InputAction> {
    let terminal_content = backend.last_content();
    let cursor_x = position.x - layout.rect.min.x;
    let cursor_y = position.y - layout.rect.min.y;
    state.current_mouse_position_on_grid = TerminalBackend::selection_point(
        cursor_x,
        cursor_y,
        &terminal_content.terminal_size,
        terminal_content.display_offset,
    );

    let mut actions = vec![];
    // Handle command or selection update based on terminal mode and modifiers
    if state.is_dragged {
        let terminal_mode = terminal_content.terminal_mode;
        let cmd = if terminal_mode.contains(TermMode::MOUSE_MOTION) && modifiers.is_none() {
            InputAction::BackendCall(BackendCommand::MouseReport(
                MouseButton::LeftMove,
                *modifiers,
                state.current_mouse_position_on_grid,
                true,
            ))
        } else {
            InputAction::BackendCall(BackendCommand::SelectUpdate(cursor_x, cursor_y))
        };

        actions.push(cmd);
    }

    // Handle link hover if applicable
    if modifiers.command_only() {
        actions.push(InputAction::BackendCall(BackendCommand::ProcessLink(
            LinkAction::Hover,
            state.current_mouse_position_on_grid,
        )));
    }

    actions
}
