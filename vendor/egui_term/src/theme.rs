use alacritty_terminal::vte::ansi::{self, NamedColor};
use egui::Color32;
use std::sync::{Arc, OnceLock};

#[derive(Debug, Clone)]
pub struct ColorPalette {
    pub foreground: String,
    pub background: String,
    pub black: String,
    pub red: String,
    pub green: String,
    pub yellow: String,
    pub blue: String,
    pub magenta: String,
    pub cyan: String,
    pub white: String,
    pub bright_black: String,
    pub bright_red: String,
    pub bright_green: String,
    pub bright_yellow: String,
    pub bright_blue: String,
    pub bright_magenta: String,
    pub bright_cyan: String,
    pub bright_white: String,
    pub bright_foreground: Option<String>,
    pub dim_foreground: String,
    pub dim_black: String,
    pub dim_red: String,
    pub dim_green: String,
    pub dim_yellow: String,
    pub dim_blue: String,
    pub dim_magenta: String,
    pub dim_cyan: String,
    pub dim_white: String,
}

impl Default for ColorPalette {
    fn default() -> Self {
        Self {
            foreground: String::from("#d8d8d8"),
            background: String::from("#181818"),
            black: String::from("#181818"),
            red: String::from("#ac4242"),
            green: String::from("#90a959"),
            yellow: String::from("#f4bf75"),
            blue: String::from("#6a9fb5"),
            magenta: String::from("#aa759f"),
            cyan: String::from("#75b5aa"),
            white: String::from("#d8d8d8"),
            bright_black: String::from("#6b6b6b"),
            bright_red: String::from("#c55555"),
            bright_green: String::from("#aac474"),
            bright_yellow: String::from("#feca88"),
            bright_blue: String::from("#82b8c8"),
            bright_magenta: String::from("#c28cb8"),
            bright_cyan: String::from("#93d3c3"),
            bright_white: String::from("#f8f8f8"),
            bright_foreground: None,
            dim_foreground: String::from("#828482"),
            dim_black: String::from("#0f0f0f"),
            dim_red: String::from("#712b2b"),
            dim_green: String::from("#5f6f3a"),
            dim_yellow: String::from("#a17e4d"),
            dim_blue: String::from("#456877"),
            dim_magenta: String::from("#704d68"),
            dim_cyan: String::from("#4d7770"),
            dim_white: String::from("#8e8e8e"),
        }
    }
}

#[derive(Debug, Clone)]
pub struct TerminalTheme {
    colors: Arc<[Color32; NamedColor::DimForeground as usize + 1]>,
}

impl PartialEq for TerminalTheme {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.colors, &other.colors)
    }
}

impl Default for TerminalTheme {
    fn default() -> Self {
        static DEFAULT: OnceLock<TerminalTheme> = OnceLock::new();
        DEFAULT.get_or_init(|| Self::new(Box::default())).clone()
    }
}

impl TerminalTheme {
    pub fn new(palette: Box<ColorPalette>) -> Self {
        let parse =
            |color: &str| hex_to_color(color).unwrap_or_else(|_| panic!("invalid color {color}"));
        let mut colors = [parse(&palette.background); NamedColor::DimForeground as usize + 1];
        let normal = [
            &palette.black,
            &palette.red,
            &palette.green,
            &palette.yellow,
            &palette.blue,
            &palette.magenta,
            &palette.cyan,
            &palette.white,
            &palette.bright_black,
            &palette.bright_red,
            &palette.bright_green,
            &palette.bright_yellow,
            &palette.bright_blue,
            &palette.bright_magenta,
            &palette.bright_cyan,
            &palette.bright_white,
        ];
        for (index, color) in normal.into_iter().enumerate() {
            colors[index] = parse(color);
        }
        for r in 0..6u8 {
            for g in 0..6u8 {
                for b in 0..6u8 {
                    colors[(16 + r * 36 + g * 6 + b) as usize] = Color32::from_rgb(
                        if r == 0 { 0 } else { r * 40 + 55 },
                        if g == 0 { 0 } else { g * 40 + 55 },
                        if b == 0 { 0 } else { b * 40 + 55 },
                    );
                }
            }
        }
        for i in 0..24u8 {
            colors[232 + i as usize] = Color32::from_gray(i * 10 + 8);
        }
        colors[NamedColor::Foreground as usize] = parse(&palette.foreground);
        colors[NamedColor::BrightForeground as usize] = parse(
            palette
                .bright_foreground
                .as_deref()
                .unwrap_or(&palette.foreground),
        );
        colors[NamedColor::DimForeground as usize] = parse(&palette.dim_foreground);
        let dim = [
            &palette.dim_black,
            &palette.dim_red,
            &palette.dim_green,
            &palette.dim_yellow,
            &palette.dim_blue,
            &palette.dim_magenta,
            &palette.dim_cyan,
            &palette.dim_white,
        ];
        for (index, color) in dim.into_iter().enumerate() {
            colors[NamedColor::DimBlack as usize + index] = parse(color);
        }
        Self {
            colors: Arc::new(colors),
        }
    }

    pub fn get_color(&self, color: ansi::Color) -> Color32 {
        match color {
            ansi::Color::Spec(rgb) => Color32::from_rgb(rgb.r, rgb.g, rgb.b),
            ansi::Color::Indexed(index) => self.colors[index as usize],
            ansi::Color::Named(named) => self.colors[named as usize],
        }
    }
}

fn hex_to_color(hex: &str) -> anyhow::Result<Color32> {
    if hex.len() != 7 {
        return Err(anyhow::format_err!("input string is in non valid format"));
    }

    let r = u8::from_str_radix(&hex[1..3], 16)?;
    let g = u8::from_str_radix(&hex[3..5], 16)?;
    let b = u8::from_str_radix(&hex[5..7], 16)?;

    Ok(Color32::from_rgb(r, g, b))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn resolved_palette_preserves_named_indexed_and_true_colors() {
        let theme = TerminalTheme::new(Box::new(ColorPalette {
            foreground: "#112233".into(),
            background: "#445566".into(),
            bright_foreground: None,
            ..Default::default()
        }));
        assert_eq!(
            theme.get_color(ansi::Color::Named(NamedColor::Foreground)),
            Color32::from_rgb(17, 34, 51)
        );
        assert_eq!(
            theme.get_color(ansi::Color::Named(NamedColor::BrightForeground)),
            Color32::from_rgb(17, 34, 51)
        );
        assert_eq!(
            theme.get_color(ansi::Color::Named(NamedColor::Cursor)),
            Color32::from_rgb(68, 85, 102)
        );
        assert_eq!(theme.get_color(ansi::Color::Indexed(16)), Color32::BLACK);
        assert_eq!(theme.get_color(ansi::Color::Indexed(21)), Color32::BLUE);
        assert_eq!(theme.get_color(ansi::Color::Indexed(231)), Color32::WHITE);
        assert_eq!(
            theme.get_color(ansi::Color::Indexed(232)),
            Color32::from_gray(8)
        );
        assert_eq!(
            theme.get_color(ansi::Color::Indexed(255)),
            Color32::from_gray(238)
        );
        assert_eq!(
            theme.get_color(ansi::Color::Spec(ansi::Rgb {
                r: 12,
                g: 34,
                b: 56
            })),
            Color32::from_rgb(12, 34, 56)
        );
        assert!(Arc::ptr_eq(
            &TerminalTheme::default().colors,
            &TerminalTheme::default().colors
        ));
        assert!(Arc::ptr_eq(&theme.colors, &theme.clone().colors));
    }
}
