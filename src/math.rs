use gpui::{
    App, Bounds, Hsla, IntoElement, PathBuilder, Pixels, Point, Rgba, TextRun, Window, canvas,
    fill, font, point, prelude::*, px, size,
};
use ratex_types::{
    color::Color,
    display_item::{DisplayItem, DisplayList},
    math_style::MathStyle,
    path_command::PathCommand,
};
use std::{
    borrow::Cow,
    collections::HashMap,
    sync::{Arc, OnceLock},
};

pub const FONT_SIZE: f32 = 16.0;
// A private inherited-color marker; explicit \color{black} remains black.
const INHERIT: Color = Color::new(-1.0, -1.0, -1.0, 1.0);
const FONTS: &[&str] = &[
    "AMS-Regular",
    "Caligraphic-Regular",
    "Fraktur-Regular",
    "Fraktur-Bold",
    "Main-Bold",
    "Main-BoldItalic",
    "Main-Italic",
    "Main-Regular",
    "Math-BoldItalic",
    "Math-Italic",
    "SansSerif-Bold",
    "SansSerif-Italic",
    "SansSerif-Regular",
    "Script-Regular",
    "Size1-Regular",
    "Size2-Regular",
    "Size3-Regular",
    "Size4-Regular",
    "Typewriter-Regular",
];

fn fonts() -> Result<&'static HashMap<&'static str, ttf_parser::Face<'static>>, String> {
    static DATA: OnceLock<Vec<Cow<'static, [u8]>>> = OnceLock::new();
    static FACES: OnceLock<Result<HashMap<&'static str, ttf_parser::Face<'static>>, String>> =
        OnceLock::new();
    FACES
        .get_or_init(|| {
            let data = FONTS
                .iter()
                .map(|name| {
                    ratex_katex_fonts::ttf_bytes(&format!("KaTeX_{name}.ttf"))
                        .ok_or_else(|| format!("缺少数学字体：{name}"))
                })
                .collect::<Result<Vec<_>, _>>()?;
            let data = DATA.get_or_init(|| data);
            FONTS
                .iter()
                .zip(data)
                .map(|(name, data)| {
                    ttf_parser::Face::parse(data, 0)
                        .map(|face| (*name, face))
                        .map_err(|e| e.to_string())
                })
                .collect()
        })
        .as_ref()
        .map_err(Clone::clone)
}

// GPUI rejects fonts without 'm', including KaTeX_Size*. Read their actual outlines
// so delimiters cannot silently fall back to a normal text font.
fn outline_glyphs(display: &mut DisplayList) -> Result<(), String> {
    let fonts = fonts()?;
    for item in &mut display.items {
        let DisplayItem::GlyphPath {
            x,
            y,
            scale,
            font,
            char_code,
            color,
        } = item
        else {
            continue;
        };
        let Some(face) = fonts.get(font.as_str()) else {
            continue;
        }; // CJK/emoji use GPUI fallback.
        let id = ratex_font::FontId::parse(font).ok_or("未知数学字体")?;
        let ch = ratex_font::katex_ttf_glyph_char(id, *char_code);
        let glyph = face
            .glyph_index(ch)
            .ok_or_else(|| format!("数学字体缺少字形：{ch}"))?;
        let mut outline = Outline {
            commands: Vec::new(),
            scale: *scale / f64::from(face.units_per_em()),
        };
        face.outline_glyph(glyph, &mut outline);
        *item = DisplayItem::Path {
            x: *x,
            y: *y,
            commands: outline.commands,
            fill: true,
            color: *color,
        };
    }
    Ok(())
}

struct Outline {
    commands: Vec<PathCommand>,
    scale: f64,
}
impl ttf_parser::OutlineBuilder for Outline {
    fn move_to(&mut self, x: f32, y: f32) {
        self.commands.push(PathCommand::MoveTo {
            x: f64::from(x) * self.scale,
            y: -f64::from(y) * self.scale,
        });
    }
    fn line_to(&mut self, x: f32, y: f32) {
        self.commands.push(PathCommand::LineTo {
            x: f64::from(x) * self.scale,
            y: -f64::from(y) * self.scale,
        });
    }
    fn quad_to(&mut self, x1: f32, y1: f32, x: f32, y: f32) {
        self.commands.push(PathCommand::QuadTo {
            x1: f64::from(x1) * self.scale,
            y1: -f64::from(y1) * self.scale,
            x: f64::from(x) * self.scale,
            y: -f64::from(y) * self.scale,
        });
    }
    fn curve_to(&mut self, x1: f32, y1: f32, x2: f32, y2: f32, x: f32, y: f32) {
        self.commands.push(PathCommand::CubicTo {
            x1: f64::from(x1) * self.scale,
            y1: -f64::from(y1) * self.scale,
            x2: f64::from(x2) * self.scale,
            y2: -f64::from(y2) * self.scale,
            x: f64::from(x) * self.scale,
            y: -f64::from(y) * self.scale,
        });
    }
    fn close(&mut self) {
        self.commands.push(PathCommand::Close);
    }
}

#[derive(Debug)]
pub struct Formula {
    display: DisplayList,
}

impl Formula {
    pub fn parse(source: &str, display: bool) -> Result<Self, String> {
        if source.len() > 16 * 1024 {
            return Err("公式超过 16 KiB 限制".into());
        }
        let nodes = ratex_parser::parse(source).map_err(|error| error.to_string())?;
        let options = ratex_layout::LayoutOptions {
            style: if display {
                MathStyle::Display
            } else {
                MathStyle::Text
            },
            color: INHERIT,
            ..Default::default()
        };
        let mut display = ratex_layout::to_display_list(&ratex_layout::layout(&nodes, &options));
        // Bound geometry before either GPU drawing or clipboard bitmap allocation.
        if display.items.len() > 8192
            || [display.width, display.height, display.depth]
                .iter()
                .any(|value| !value.is_finite() || !(0.0..=512.0).contains(value))
            || display.items.iter().any(|item| !valid_item(item))
        {
            return Err("公式尺寸或复杂度超过显示限制".into());
        }
        outline_glyphs(&mut display)?;
        let commands: usize = display
            .items
            .iter()
            .map(|item| match item {
                DisplayItem::Path { commands, .. } => commands.len(),
                _ => 1,
            })
            .sum();
        if commands > 65_536 || display.items.iter().any(|item| !valid_item(item)) {
            return Err("公式轮廓超过显示限制".into());
        }
        Ok(Self { display })
    }

    pub fn ascent(&self, em: f32) -> f32 {
        self.display.height as f32 * em
    }
    pub fn descent(&self, em: f32) -> f32 {
        self.display.depth as f32 * em
    }

    pub fn element(self: &Arc<Self>, em: f32) -> impl IntoElement {
        let formula = self.clone();
        canvas(
            |_, _, _| {},
            move |bounds, _, window, cx| {
                let foreground = window.text_style().color;
                for item in &formula.display.items {
                    paint_item(
                        item,
                        bounds.origin + point(px(1.0), px(1.0)),
                        em,
                        foreground,
                        window,
                        cx,
                    );
                }
            },
        )
        .w(px(self.display.width as f32 * em + 2.0))
        .h(px(self.ascent(em) + self.descent(em) + 2.0))
        .flex_none()
    }

    pub fn clipboard_png(&self) -> Result<Vec<u8>, String> {
        let mut display = self.display.clone();
        for item in &mut display.items {
            let color = match item {
                DisplayItem::GlyphPath { color, .. }
                | DisplayItem::Line { color, .. }
                | DisplayItem::Rect { color, .. }
                | DisplayItem::Path { color, .. } => color,
            };
            if *color == INHERIT {
                *color = Color::BLACK;
            }
        }
        let pixels =
            (display.width * 32.0 + 8.0).ceil() * (display.total_height() * 32.0 + 8.0).ceil();
        if pixels > 16_000_000.0 {
            return Err("公式过大，无法复制为图片".into());
        }
        ratex_render::render_to_png(
            &display,
            &ratex_render::RenderOptions {
                font_size: FONT_SIZE,
                padding: 2.0,
                device_pixel_ratio: 2.0,
                background_color: Color::new(0.0, 0.0, 0.0, 0.0),
                ..Default::default()
            },
        )
    }
}

fn valid_item(item: &DisplayItem) -> bool {
    let valid = |values: &[f64]| values.iter().all(|v| v.is_finite() && v.abs() <= 1024.0);
    match item {
        DisplayItem::GlyphPath {
            x,
            y,
            scale,
            char_code,
            ..
        } => valid(&[*x, *y, *scale]) && *scale > 0.0 && char::from_u32(*char_code).is_some(),
        DisplayItem::Line {
            x,
            y,
            width,
            thickness,
            ..
        } => valid(&[*x, *y, *width, *thickness]) && *width >= 0.0 && *thickness >= 0.0,
        DisplayItem::Rect {
            x,
            y,
            width,
            height,
            ..
        } => valid(&[*x, *y, *width, *height]) && *width >= 0.0 && *height >= 0.0,
        DisplayItem::Path { x, y, commands, .. } => {
            valid(&[*x, *y])
                && commands.len() <= 8192
                && commands.iter().all(|command| match command {
                    PathCommand::MoveTo { x, y } | PathCommand::LineTo { x, y } => valid(&[*x, *y]),
                    PathCommand::QuadTo { x1, y1, x, y } => valid(&[*x1, *y1, *x, *y]),
                    PathCommand::CubicTo {
                        x1,
                        y1,
                        x2,
                        y2,
                        x,
                        y,
                    } => valid(&[*x1, *y1, *x2, *y2, *x, *y]),
                    PathCommand::Close => true,
                })
        }
    }
}

fn paint_item(
    item: &DisplayItem,
    origin: Point<Pixels>,
    em: f32,
    foreground: Hsla,
    window: &mut Window,
    _cx: &mut App,
) {
    let at = |x: f64, y: f64| origin + point(px(x as f32 * em), px(y as f32 * em));
    let color = |c: &Color| -> Hsla {
        if *c == INHERIT {
            foreground
        } else {
            Rgba {
                r: c.r,
                g: c.g,
                b: c.b,
                a: c.a,
            }
            .into()
        }
    };
    match item {
        DisplayItem::GlyphPath {
            x,
            y,
            scale,
            font: name,
            char_code,
            color: c,
        } => {
            let fid = ratex_font::FontId::parse(name).unwrap_or(ratex_font::FontId::MainRegular);
            let character = ratex_font::katex_ttf_glyph_char(fid, *char_code);
            let text = character.to_string();
            let font_size = px(em * *scale as f32);
            let line = window.text_system().shape_line(
                text.clone().into(),
                font_size,
                &[TextRun {
                    len: text.len(),
                    font: font(".SystemUIFont"),
                    color: color(c),
                    ..Default::default()
                }],
                None,
            );
            for run in &line.runs {
                for glyph in &run.glyphs {
                    let position = at(*x, *y) + glyph.position;
                    let result = if glyph.is_emoji {
                        window.paint_emoji(position, run.font_id, glyph.id, font_size)
                    } else {
                        window.paint_glyph(position, run.font_id, glyph.id, font_size, color(c))
                    };
                    if let Err(error) = result {
                        eprintln!("数学字形绘制失败：{error}");
                    }
                }
            }
        }
        DisplayItem::Rect {
            x,
            y,
            width,
            height,
            color: c,
        } => {
            window.paint_quad(fill(
                Bounds::new(
                    at(*x, *y),
                    size(px(*width as f32 * em), px(*height as f32 * em)),
                ),
                color(c),
            ));
        }
        DisplayItem::Line {
            x,
            y,
            width,
            thickness,
            color: c,
            dashed,
        } => {
            let thickness = px((*thickness as f32 * em).max(1.0 / window.scale_factor()));
            let mut path = PathBuilder::stroke(thickness);
            if *dashed {
                path = path.dash_array(&[thickness * 4.0, thickness * 4.0]);
            }
            path.move_to(at(*x, *y));
            path.line_to(at(*x + *width, *y));
            if let Ok(path) = path.build() {
                window.paint_path(path, color(c));
            }
        }
        DisplayItem::Path {
            x,
            y,
            commands,
            fill,
            color: c,
        } => {
            let mut path = if *fill {
                PathBuilder::fill()
            } else {
                PathBuilder::stroke(px(1.5))
            };
            let at = |dx, dy| at(x + dx, y + dy);
            for command in commands {
                match *command {
                    PathCommand::MoveTo { x, y } => path.move_to(at(x, y)),
                    PathCommand::LineTo { x, y } => path.line_to(at(x, y)),
                    PathCommand::QuadTo { x1, y1, x, y } => path.curve_to(at(x, y), at(x1, y1)),
                    PathCommand::CubicTo {
                        x1,
                        y1,
                        x2,
                        y2,
                        x,
                        y,
                    } => path.cubic_bezier_to(at(x, y), at(x1, y1), at(x2, y2)),
                    PathCommand::Close => path.close(),
                }
            }
            if let Ok(path) = path.build() {
                window.paint_path(path, color(c));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn math_fonts_are_embedded_without_a_runtime_cargo_cache() {
        for name in FONTS {
            let bytes = ratex_katex_fonts::ttf_bytes(&format!("KaTeX_{name}.ttf"))
                .expect("bundled font must exist");
            assert!(
                matches!(bytes, Cow::Borrowed(_)),
                "{name} must be compiled in, not read from Cargo's source directory"
            );
            ttf_parser::Face::parse(&bytes, 0).expect("embedded font must be valid");
        }
    }

    #[test]
    fn lays_out_common_math_and_rejects_invalid_input() {
        let start = std::time::Instant::now();
        for source in [
            r"E=mc^2",
            r"\frac{a_1}{\sqrt{1+x^2}}",
            r"\left(\frac{1}{x}\right)",
            r"\sum_{n=1}^{\infty}\frac{1}{n^2}",
            r"\begin{pmatrix}a&b\\c&d\end{pmatrix}",
            r"\int_0^1 x^2\,dx=\frac13",
            r"\text{中文}+\alpha",
            r"\hat{x}+\vec{v}",
        ] {
            for display in [false, true] {
                let formula = Formula::parse(source, display).unwrap();
                assert!(formula.display.width > 0.0, "{source}");
                assert!(formula.ascent(16.0) > 0.0, "{source}");
                assert!(!formula.display.items.is_empty());
            }
        }
        eprintln!("16 native math layouts: {:?}", start.elapsed());
        let inline = Formula::parse(r"\frac{a}{b}", false).unwrap();
        let display = Formula::parse(r"\frac{a}{b}", true).unwrap();
        assert!(display.ascent(16.0) > inline.ascent(16.0));
        assert_eq!(inline.ascent(32.0), inline.ascent(16.0) * 2.0);
        assert!(inline.descent(16.0) > 0.0);
        for invalid in [r"\unknowncommand{x}", r"\frac{", r"\input{/etc/passwd}"] {
            assert!(Formula::parse(invalid, false).is_err(), "{invalid}");
        }
        assert!(Formula::parse(&"x".repeat(16 * 1024 + 1), false).is_err());
        assert!(
            Formula::parse(&format!("{}x{}", "{".repeat(100), "}".repeat(100)), false).is_err()
        );
        let png = inline.clipboard_png().unwrap();
        assert!(png.starts_with(b"\x89PNG"));
    }

    #[test]
    fn preserves_special_math_font_outlines_without_text_font_fallback() {
        let fonts = fonts().unwrap();
        assert!(fonts["Size2-Regular"].glyph_index('m').is_none());
        let mut display = DisplayList::new();
        display.items.push(DisplayItem::GlyphPath {
            x: 0.0,
            y: 2.0,
            scale: 1.0,
            font: "Size2-Regular".into(),
            char_code: '(' as u32,
            color: INHERIT,
        });
        outline_glyphs(&mut display).unwrap();
        let DisplayItem::Path { commands, .. } = &display.items[0] else {
            panic!()
        };
        assert!(commands.len() > 5);
        let ys: Vec<_> = commands
            .iter()
            .filter_map(|command| match command {
                PathCommand::MoveTo { y, .. }
                | PathCommand::LineTo { y, .. }
                | PathCommand::QuadTo { y, .. }
                | PathCommand::CubicTo { y, .. } => Some(*y),
                PathCommand::Close => None,
            })
            .collect();
        let top = ys.iter().copied().fold(f64::INFINITY, f64::min);
        let bottom = ys.iter().copied().fold(f64::NEG_INFINITY, f64::max);
        assert!(
            bottom - top > 1.5,
            "large delimiter must exceed regular text height"
        );
    }
}
