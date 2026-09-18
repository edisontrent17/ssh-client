use eframe::egui::{self, Color32, FontFamily, FontId, RichText, Stroke};
use egui_term::{ColorPalette, TerminalTheme};

pub const TEXT: Color32 = Color32::from_rgb(29, 29, 31);
pub const MUTED: Color32 = Color32::from_rgb(100, 100, 107);
pub const SIDEBAR: Color32 = Color32::from_rgb(245, 245, 247);
pub const BORDER: Color32 = Color32::from_rgb(215, 215, 221);
pub const BLUE: Color32 = Color32::from_rgb(0, 112, 235);
pub const SELECTION: Color32 = Color32::from_rgb(218, 233, 252);

#[cfg(target_os = "macos")]
fn system_fonts() -> egui::FontDefinitions {
    use std::sync::OnceLock;
    static UI_FONT: OnceLock<Option<Vec<u8>>> = OnceLock::new();
    static MONO_FONT: OnceLock<Option<Vec<u8>>> = OnceLock::new();
    let mut fonts = egui::FontDefinitions::default();
    for (name, path, family, storage) in [
        (
            "system-ui",
            "/System/Library/Fonts/SFNS.ttf",
            FontFamily::Proportional,
            &UI_FONT,
        ),
        (
            "system-mono",
            "/System/Library/Fonts/SFNSMono.ttf",
            FontFamily::Monospace,
            &MONO_FONT,
        ),
    ] {
        // egui clones owned FontData when creating a font face. Keep one immutable
        // copy per process and lend it to every face/context instead. Only these
        // two fixed system fonts are retained, for the lifetime of the application.
        if let Some(data) = storage.get_or_init(|| std::fs::read(path).ok()).as_deref() {
            fonts
                .font_data
                .insert(name.into(), egui::FontData::from_static(data).into());
            fonts
                .families
                .entry(family)
                .or_default()
                .insert(0, name.into());
        }
    }
    fonts
}

pub fn configure(ctx: &egui::Context) {
    ctx.set_theme(egui::Theme::Light);
    ctx.set_visuals(egui::Visuals::light());
    ctx.send_viewport_cmd(egui::ViewportCommand::SetTheme(egui::SystemTheme::Light));
    ctx.style_mut_of(egui::Theme::Light, |style| {
        style
            .text_styles
            .insert(egui::TextStyle::Body, FontId::proportional(16.0));
        style
            .text_styles
            .insert(egui::TextStyle::Button, FontId::proportional(16.0));
        style
            .text_styles
            .insert(egui::TextStyle::Small, FontId::proportional(12.0));
        style
            .text_styles
            .insert(egui::TextStyle::Heading, FontId::proportional(22.0));
        style.spacing.item_spacing = egui::vec2(8.0, 10.0);
        style.spacing.button_padding = egui::vec2(12.0, 7.0);
        style.spacing.interact_size.y = 32.0;
        let v = &mut style.visuals;
        v.panel_fill = SIDEBAR;
        v.window_fill = SIDEBAR;
        v.extreme_bg_color = Color32::WHITE;
        v.text_edit_bg_color = Some(Color32::WHITE);
        v.weak_text_color = Some(MUTED);
        v.widgets.noninteractive.fg_stroke.color = TEXT;
        v.widgets.noninteractive.bg_stroke = Stroke::new(1.0, BORDER);
        v.selection.bg_fill = SELECTION;
        v.selection.stroke = Stroke::new(1.0, BLUE);
        v.window_corner_radius = egui::CornerRadius::same(12);
        v.window_stroke = Stroke::new(1.0, BORDER);
        v.window_shadow = egui::epaint::Shadow {
            offset: [0, 6],
            blur: 28,
            spread: 0,
            color: Color32::from_black_alpha(28),
        };
        v.disabled_alpha = 0.55;
        for (widget, fill) in [
            (&mut v.widgets.inactive, Color32::WHITE),
            (&mut v.widgets.hovered, Color32::from_rgb(237, 243, 253)),
            (&mut v.widgets.active, SELECTION),
            (&mut v.widgets.open, SELECTION),
        ] {
            widget.bg_fill = fill;
            widget.weak_bg_fill = fill;
            widget.bg_stroke = Stroke::new(1.0, BORDER);
            widget.fg_stroke = Stroke::new(1.0, TEXT);
            widget.corner_radius = egui::CornerRadius::same(5);
        }
        v.widgets.hovered.bg_stroke.color = BLUE;
        v.widgets.active.bg_stroke.color = BLUE;
    });
    // Use the fonts already installed on the Mac, without redistributing them.
    #[cfg(target_os = "macos")]
    ctx.set_fonts(system_fonts());
}

pub fn primary(label: &str) -> egui::Button<'_> {
    egui::Button::new(RichText::new(label).color(Color32::WHITE))
        .fill(BLUE)
        .stroke(Stroke::NONE)
        .min_size(egui::vec2(150.0, 40.0))
}

pub fn terminal() -> TerminalTheme {
    TerminalTheme::new(Box::new(ColorPalette {
        background: "#ffffff".into(),
        foreground: "#1d1d1f".into(),
        bright_foreground: Some("#101012".into()),
        dim_foreground: "#64646b".into(),
        black: "#1d1d1f".into(),
        red: "#b42318".into(),
        green: "#247238".into(),
        yellow: "#8a5b00".into(),
        blue: "#005fc4".into(),
        magenta: "#8945a6".into(),
        cyan: "#007880".into(),
        white: "#d5d5da".into(),
        bright_black: "#64646b".into(),
        bright_red: "#ce3025".into(),
        bright_green: "#258142".into(),
        bright_yellow: "#986800".into(),
        bright_blue: "#0070df".into(),
        bright_magenta: "#9b43b4".into(),
        bright_cyan: "#00838c".into(),
        bright_white: "#ffffff".into(),
        dim_black: "#48484d".into(),
        dim_red: "#93423b".into(),
        dim_green: "#486d4a".into(),
        dim_yellow: "#756135".into(),
        dim_blue: "#456887".into(),
        dim_magenta: "#765484".into(),
        dim_cyan: "#477477".into(),
        dim_white: "#afafb5".into(),
    }))
}

pub struct Icons(std::collections::BTreeMap<&'static str, egui::TextureHandle>);

impl Icons {
    pub fn new(ctx: &egui::Context) -> Self {
        macro_rules! load {
            ($($name:literal),*) => { [$(($name, include_bytes!(concat!("assets/icons/", $name, ".rgba")).as_slice())),*] };
        }
        let icons = load!(
            "folder",
            "file",
            "server",
            "search",
            "plus",
            "terminal",
            "upload",
            "download",
            "chevron-right",
            "chevron-down",
            "x",
            "arrow-up",
            "refresh"
        );
        Self(
            icons
                .into_iter()
                .map(|(name, rgba)| {
                    (
                        name,
                        ctx.load_texture(
                            name,
                            egui::ColorImage::from_rgba_unmultiplied([48, 48], rgba),
                            egui::TextureOptions::LINEAR,
                        ),
                    )
                })
                .collect(),
        )
    }

    pub fn image(&self, name: &str, size: f32, tint: Color32) -> egui::Image<'static> {
        egui::Image::new((self.0[name].id(), egui::vec2(size, size))).tint(tint)
    }

    pub fn button<'a>(&self, name: &str, label: &'a str) -> egui::Button<'a> {
        egui::Button::image_and_text(self.image(name, 16.0, MUTED), label)
    }
}

#[cfg(all(test, target_os = "macos"))]
mod tests {
    use super::*;

    #[test]
    fn system_font_faces_and_contexts_share_the_same_bytes() {
        let first = system_fonts();
        let second = system_fonts();
        for name in ["system-ui", "system-mono"] {
            // Font availability differs between macOS versions; the bundled
            // fallback families remain usable if a system font is absent.
            if let Some(font) = first.font_data.get(name) {
                assert!(matches!(font.font, std::borrow::Cow::Borrowed(_)));
                assert_eq!(font.font.as_ptr(), second.font_data[name].font.as_ptr());
                assert_eq!(font.font.as_ptr(), font.as_ref().clone().font.as_ptr());
            }
        }
        // Compare glyph metrics and tessellated text with the previous owned
        // font representation, including the UI's variable-weight labels.
        let mut owned = first.clone();
        for name in ["system-ui", "system-mono"] {
            if let Some(font) = owned.font_data.get_mut(name) {
                let mut data = font.as_ref().clone();
                data.font = std::borrow::Cow::Owned(data.font.to_vec());
                *font = data.into();
            }
        }
        let render = |definitions| {
            let ctx = egui::Context::default();
            ctx.set_fonts(definitions);
            let output = ctx.run_ui(egui::RawInput::default(), |ui| {
                ui.label("Relay — café 0123456789");
                ui.label(RichText::new("Remote files").variation("wght", 600.0));
                ui.monospace("user@host:~$ ls -la");
            });
            ctx.tessellate(output.shapes, output.pixels_per_point)
        };
        let shared = render(first);
        let owned = render(owned);
        assert_eq!(shared.len(), owned.len());
        for (shared, owned) in shared.iter().zip(&owned) {
            assert_eq!(shared.clip_rect, owned.clip_rect);
            let (egui::epaint::Primitive::Mesh(shared), egui::epaint::Primitive::Mesh(owned)) =
                (&shared.primitive, &owned.primitive)
            else {
                panic!("expected text meshes");
            };
            assert_eq!(shared.vertices, owned.vertices);
            assert_eq!(shared.indices, owned.indices);
            assert_eq!(shared.texture_id, owned.texture_id);
        }
    }
}
