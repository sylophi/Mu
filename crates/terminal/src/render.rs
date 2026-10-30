//! Laying out and painting the grid: merged background rects, then one shaped
//! line per run of cells that meet on a row, in a run of text for each style,
//! snapped to the cell grid.

use std::sync::{Arc, atomic::Ordering};

use alacritty_terminal::{
    grid::Dimensions as _,
    term::{
        RenderableContent,
        cell::{Cell, Flags},
        color::Colors,
    },
    vte::ansi::{Color as AnsiColor, CursorShape, NamedColor, Rgb},
};
use gpui_kit::component::ActiveTheme as _;
use gpui_kit::*;

use crate::{Geometry, TerminalView, powerline};

pub fn terminal_canvas(view: Entity<TerminalView>) -> impl IntoElement {
    let layout_view = view.clone();
    canvas(
        move |bounds, window, cx| layout_view.update(cx, |view, cx| view.layout(bounds, window, cx)),
        move |bounds, layout, window, cx| {
            window.handle_input(&layout.focus_handle, ElementInputHandler::new(bounds, view), cx);
            layout.paint(window, cx);
        },
    )
    .size_full()
}

pub struct Palette {
    ansi: [Hsla; 16],
    foreground: Hsla,
    background: Hsla,
    cursor: Hsla,
}

/// The 16 ANSI colors for each appearance, which the app sets as a global before it shows a
/// terminal.
pub struct AnsiColors {
    pub dark: [Hsla; 16],
    pub light: [Hsla; 16],
}

impl Global for AnsiColors {}

impl Palette {
    pub fn new(cx: &App) -> Self {
        let theme = cx.theme();
        let colors = cx.global::<AnsiColors>();
        let ansi = if theme.is_dark() { colors.dark } else { colors.light };
        Self { ansi, foreground: theme.foreground, background: theme.background, cursor: theme.caret }
    }

    fn resolve(&self, color: AnsiColor, overrides: &Colors) -> Hsla {
        match color {
            AnsiColor::Spec(rgb) => to_hsla(rgb),
            AnsiColor::Indexed(index) => self.indexed(index as usize, overrides),
            AnsiColor::Named(name) => self.indexed(name as usize, overrides),
        }
    }

    fn indexed(&self, index: usize, overrides: &Colors) -> Hsla {
        if let Some(rgb) = overrides[index] {
            return to_hsla(rgb);
        }
        const CUBE: [u8; 6] = [0, 95, 135, 175, 215, 255];
        match index {
            0..16 => self.ansi[index],
            16..232 => {
                let i = index - 16;
                to_hsla(Rgb { r: CUBE[i / 36], g: CUBE[i / 6 % 6], b: CUBE[i % 6] })
            }
            232..256 => {
                let v = 8 + 10 * (index - 232) as u8;
                to_hsla(Rgb { r: v, g: v, b: v })
            }
            _ => match index {
                i if i == NamedColor::Foreground as usize => self.foreground,
                i if i == NamedColor::Background as usize => self.background,
                i if i == NamedColor::Cursor as usize => self.cursor,
                i if i == NamedColor::BrightForeground as usize => self.foreground,
                i if i == NamedColor::DimForeground as usize => self.foreground.opacity(0.7),
                // DimBlack..DimWhite follow Cursor.
                i => self.ansi[(i - NamedColor::DimBlack as usize) % 8].opacity(0.7),
            },
        }
    }

    /// A color's current value, answering an app's OSC color query.
    pub fn request(&self, index: usize, overrides: &Colors) -> Rgb {
        let rgba = Rgba::from(self.indexed(index, overrides));
        let byte = |v: f32| (v * 255.).round() as u8;
        Rgb { r: byte(rgba.r), g: byte(rgba.g), b: byte(rgba.b) }
    }
}

fn to_hsla(rgb: Rgb) -> Hsla {
    Rgba { r: rgb.r as f32 / 255., g: rgb.g as f32 / 255., b: rgb.b as f32 / 255., a: 1. }.into()
}

#[derive(Clone, PartialEq)]
struct Style {
    color: Hsla,
    bold: bool,
    italic: bool,
    underline: bool,
    strikethrough: bool,
}

impl Style {
    fn plain(color: Hsla) -> Self {
        Self { color, bold: false, italic: false, underline: false, strikethrough: false }
    }

    /// The style `cell`'s flags give it, in `color`.
    fn of(cell: &Cell, color: Hsla) -> Self {
        Self {
            bold: cell.flags.contains(Flags::BOLD),
            italic: cell.flags.contains(Flags::ITALIC),
            underline: cell.flags.intersects(Flags::ALL_UNDERLINES),
            strikethrough: cell.flags.contains(Flags::STRIKEOUT),
            ..Self::plain(color)
        }
    }

    /// Whether it draws a line through or under spaces too.
    fn lined(&self) -> bool {
        self.underline || self.strikethrough
    }

    fn unlined(self) -> Self {
        Self { underline: false, strikethrough: false, ..self }
    }
}

/// Cells on a row with at most a space between them, shaped together, so a row in many colors is
/// laid out and painted as one line. A wider gap, like a TUI's padding, starts another line, so its
/// spaces are neither shaped nor painted.
struct Line {
    row: usize,
    column: usize,
    /// The column after the last cell.
    end: usize,
    /// Whether a space follows, which goes in only once a cell that draws comes after it.
    space: bool,
    text: String,
    /// The length of the text in each style, in order.
    runs: Vec<(usize, Style)>,
    /// A wide glyph spans two cells and gets a line of its own.
    wide: bool,
}

impl Line {
    /// A line starting with a cell `width` columns wide.
    fn new(row: usize, column: usize, width: usize, chars: impl Iterator<Item = char>, style: Style) -> Self {
        let mut line = Self {
            row,
            column,
            end: column + width,
            space: false,
            text: String::new(),
            runs: Vec::new(),
            wide: width == 2,
        };
        line.append(chars, style);
        line
    }

    fn continues_at(&self, row: usize, column: usize) -> bool {
        !self.wide && self.row == row && self.end + self.space as usize == column
    }

    /// Adds the next cell's characters, after the space before it.
    fn push(&mut self, chars: impl Iterator<Item = char>, style: Style) {
        if std::mem::take(&mut self.space) {
            // It draws nothing, so it can go in the run before it, if not under or through a line.
            let last = self.runs.last().expect("a line starts with a cell").1.clone();
            self.append(std::iter::once(' '), last.unlined());
            self.end += 1;
        }
        self.append(chars, style);
        self.end += 1;
    }

    fn append(&mut self, chars: impl Iterator<Item = char>, style: Style) {
        let start = self.text.len();
        self.text.extend(chars);
        let len = self.text.len() - start;
        match self.runs.last_mut() {
            Some((run, last)) if *last == style => *run += len,
            _ => self.runs.push((len, style)),
        }
    }
}

/// Shapes text in the terminal's font, a cell to a character.
struct Shaper {
    text_system: Arc<WindowTextSystem>,
    font: Font,
    font_size: Pixels,
    cell_width: Pixels,
}

impl Shaper {
    fn shape(&self, text: String, style: &Style) -> ShapedLine {
        let run = self.run(text.len(), style);
        self.text_system.shape_line(text.into(), self.font_size, &[run], Some(self.cell_width))
    }

    fn shape_line(&self, line: Line) -> ShapedLine {
        let runs: Vec<_> = line.runs.iter().map(|(len, style)| self.run(*len, style)).collect();
        self.text_system.shape_line(line.text.into(), self.font_size, &runs, Some(self.cell_width))
    }

    fn run(&self, len: usize, style: &Style) -> TextRun {
        TextRun {
            len,
            font: Font {
                weight: if style.bold { FontWeight::BOLD } else { FontWeight::NORMAL },
                style: if style.italic { FontStyle::Italic } else { FontStyle::Normal },
                ..self.font.clone()
            },
            color: style.color,
            background_color: None,
            underline: style.underline.then_some(UnderlineStyle {
                thickness: px(1.),
                color: Some(style.color),
                wavy: false,
            }),
            strikethrough: style
                .strikethrough
                .then_some(StrikethroughStyle { thickness: px(1.), color: Some(style.color) }),
        }
    }
}

/// What the cells on screen draw, before their text is shaped.
#[derive(Default)]
struct Cells {
    backgrounds: Vec<(Bounds<Pixels>, Hsla)>,
    /// Powerline's, in their cells, which a line's ends would otherwise stick out of.
    separators: Vec<(Bounds<Pixels>, Path<Pixels>, Hsla)>,
    lines: Vec<Line>,
}

impl Cells {
    fn new(content: RenderableContent, selection: Hsla, palette: &Palette, geometry: &Geometry) -> Self {
        let mut cells = Self::default();
        let offset = content.display_offset as i32;
        for indexed in content.display_iter {
            let cell = indexed.cell;
            let row = (indexed.point.line.0 + offset) as usize;
            let column = indexed.point.column.0;
            if cell.flags.intersects(Flags::WIDE_CHAR_SPACER | Flags::LEADING_WIDE_CHAR_SPACER) {
                continue;
            }

            let mut fg = palette.resolve(cell.fg, content.colors);
            let mut bg =
                (cell.bg != AnsiColor::Named(NamedColor::Background)).then(|| palette.resolve(cell.bg, content.colors));
            if cell.flags.contains(Flags::INVERSE) {
                let inverted_bg = fg;
                fg = bg.unwrap_or(palette.background);
                bg = Some(inverted_bg);
            }
            if cell.flags.contains(Flags::DIM) {
                fg = fg.opacity(0.7);
            }
            let width = if cell.flags.contains(Flags::WIDE_CHAR) { 2 } else { 1 };
            if let Some(bg) = bg {
                cells.background(geometry.cells(row, column, width), bg);
            }
            if content.selection.is_some_and(|range| range.contains(indexed.point)) {
                cells.background(geometry.cells(row, column, width), selection);
            }
            if cell.flags.contains(Flags::HIDDEN) {
                continue;
            }
            let bounds = geometry.cells(row, column, 1);
            if let Some(path) = powerline::separator(cell.c, bounds) {
                cells.separators.push((bounds, path, fg));
                continue;
            }

            // The cell a tab starts from keeps it, for copying, but draws as a space: shaped, it'd
            // move what's after it to the font's tab stops.
            let c = if cell.c == '\t' { ' ' } else { cell.c };
            let style = Style::of(cell, fg);
            if c == ' ' && !style.lined() {
                if let Some(line) = cells.lines.last_mut()
                    && line.continues_at(row, column)
                {
                    line.space = true;
                }
                continue;
            }
            let chars = std::iter::once(c).chain(cell.zerowidth().unwrap_or_default().iter().copied());
            match cells.lines.last_mut() {
                Some(line) if width == 1 && line.continues_at(row, column) => line.push(chars, style),
                _ => cells.lines.push(Line::new(row, column, width, chars, style)),
            }
        }
        cells
    }

    /// Fills `bounds` with `color`, as part of the background before it when that's the same color
    /// and meets it on the same line.
    fn background(&mut self, bounds: Bounds<Pixels>, color: Hsla) {
        if let Some((last, last_color)) = self.backgrounds.last_mut()
            && *last_color == color
            && last.origin.y == bounds.origin.y
            && (last.right() - bounds.left()).abs() < px(0.5)
        {
            last.size.width += bounds.size.width;
        } else {
            self.backgrounds.push((bounds, color));
        }
    }
}

/// The cursor, and for a block, the character under it drawn again in the background color.
struct Cursor {
    quad: PaintQuad,
    glyph: Option<(Point<Pixels>, ShapedLine)>,
}

impl Cursor {
    /// The cursor shaped `shape` over `cell`, which it fills at `bounds`, drawn hollow while the
    /// terminal isn't focused.
    fn new(
        shape: CursorShape,
        cell: &Cell,
        bounds: Bounds<Pixels>,
        focused: bool,
        palette: &Palette,
        shaper: &Shaper,
    ) -> Self {
        let color = palette.cursor;
        let bare = |quad| Self { quad, glyph: None };
        match shape {
            _ if !focused => bare(outline(bounds, color, BorderStyle::Solid)),
            CursorShape::HollowBlock => bare(outline(bounds, color, BorderStyle::Solid)),
            CursorShape::Beam => bare(fill(Bounds::new(bounds.origin, size(px(2.), bounds.size.height)), color)),
            CursorShape::Underline => bare(fill(
                Bounds::new(point(bounds.left(), bounds.bottom() - px(2.)), size(bounds.size.width, px(2.))),
                color,
            )),
            _ => Self {
                quad: fill(bounds, color),
                glyph: (cell.c != ' ' && !cell.flags.contains(Flags::HIDDEN)).then(|| {
                    let style = Style::of(cell, palette.background).unlined();
                    (bounds.origin, shaper.shape(cell.c.to_string(), &style))
                }),
            },
        }
    }
}

pub struct Layout {
    focus_handle: FocusHandle,
    line_height: Pixels,
    backgrounds: Vec<(Bounds<Pixels>, Hsla)>,
    separators: Vec<(Bounds<Pixels>, Path<Pixels>, Hsla)>,
    texts: Vec<(Point<Pixels>, ShapedLine)>,
    cursor: Option<Cursor>,
    marked: Option<(Bounds<Pixels>, Hsla, ShapedLine)>,
}

impl Layout {
    fn paint(self, window: &mut Window, cx: &mut App) {
        for (bounds, color) in &self.backgrounds {
            window.paint_quad(fill(*bounds, *color));
        }
        for (cell, path, color) in self.separators {
            window.with_content_mask(Some(ContentMask { bounds: cell }), |window| window.paint_path(path, color));
        }
        for (origin, line) in &self.texts {
            line.paint(*origin, self.line_height, TextAlign::Left, None, window, cx).ok();
        }
        if let Some(cursor) = self.cursor {
            window.paint_quad(cursor.quad);
            if let Some((origin, line)) = &cursor.glyph {
                line.paint(*origin, self.line_height, TextAlign::Left, None, window, cx).ok();
            }
        }
        if let Some((bounds, background, line)) = &self.marked {
            window.paint_quad(fill(*bounds, *background));
            line.paint(bounds.origin, self.line_height, TextAlign::Left, None, window, cx).ok();
        }
    }
}

impl TerminalView {
    fn layout(&mut self, bounds: Bounds<Pixels>, window: &mut Window, cx: &mut Context<Self>) -> Layout {
        let theme = cx.theme();
        let font = Font {
            // Prompts like powerlevel10k draw icons from a Nerd Font.
            fallbacks: Some(FontFallbacks::from_fonts(vec!["Symbols Nerd Font Mono".into(), "MesloLGS NF".into()])),
            ..font(theme.mono_font_family.clone())
        };
        let (font_size, selection) = (theme.mono_font_size, theme.selection);
        let text_system = window.text_system().clone();
        let font_id = text_system.resolve_font(&font);
        // Cells a whole number of pixels, as Ghostty has them, so where they meet, backgrounds and
        // glyphs don't each cover part of a pixel, which leaves a seam.
        let scale = window.scale_factor();
        let snap = |value: Pixels| px((f32::from(value) * scale).round() / scale);
        let cell_width =
            snap(text_system.advance(font_id, font_size, 'm').map(|advance| advance.width).unwrap_or(font_size * 0.6));
        let line_height = snap(font_size * 1.3);
        self.set_geometry(Geometry {
            origin: point(snap(bounds.origin.x), snap(bounds.origin.y)),
            cell_width,
            line_height,
            columns: ((bounds.size.width / cell_width) as usize).max(2),
            lines: ((bounds.size.height / line_height) as usize).max(1),
        });
        let geometry = self.geometry;
        let shaper = Shaper { text_system, font, font_size, cell_width };
        let palette = Palette::new(cx);

        // Output parsed from here on wakes the view for another frame.
        self.redraw_pending.store(false, Ordering::Release);
        let term = self.term.lock();
        let content = term.renderable_content();
        let cursor = content.cursor;
        let (cursor_row, cursor_column) = (cursor.point.line.0 + content.display_offset as i32, cursor.point.column.0);
        let cells = Cells::new(content, selection, &palette, &geometry);
        let texts = cells
            .lines
            .into_iter()
            .map(|line| (geometry.cell_origin(line.row, line.column), shaper.shape_line(line)))
            .collect();

        let shown = cursor.shape != CursorShape::Hidden && (0..term.screen_lines() as i32).contains(&cursor_row);
        let focused = self.focus_handle.is_focused(window);
        let cursor = shown.then(|| {
            let cell = &term.grid()[cursor.point];
            let width = if cell.flags.contains(Flags::WIDE_CHAR) { 2 } else { 1 };
            let bounds = geometry.cells(cursor_row as usize, cursor_column, width);
            Cursor::new(cursor.shape, cell, bounds, focused, &palette, &shaper)
        });
        drop(term);

        let marked = self.marked_text.clone().filter(|_| cursor_row >= 0).map(|text| {
            let line = shaper.shape(text, &Style { underline: true, ..Style::plain(palette.foreground) });
            let mut bounds = geometry.cells(cursor_row as usize, cursor_column, 1);
            bounds.size.width = line.width.max(cell_width);
            (bounds, palette.background, line)
        });

        Layout {
            focus_handle: self.focus_handle.clone(),
            line_height,
            backgrounds: cells.backgrounds,
            separators: cells.separators,
            texts,
            cursor,
            marked,
        }
    }
}
