//! Self-written GPUI canvas renderer: alacritty grid -> glyphs.
//!
//! Owns cell metrics, the size computation (cols/rows from the measured bounds),
//! and the resize sync to both the local grid and the backend (CLAUDE.md §2:
//! resize is the view's responsibility, computed from cell metrics). Phase 1
//! paints the visible screen, backgrounds, and the cursor. Selection highlight
//! and scrollback come in Phase 3.

use std::sync::Arc;

use alacritty_terminal::term::TermMode;
use alacritty_terminal::term::cell::Flags;
use alacritty_terminal::vte::ansi::{Color, CursorShape, NamedColor};

use gpui::{
    App, Bounds, FocusHandle, Font, Hsla, Pixels, Point, Styled, TextRun, Window, canvas, fill,
    point, px,
};

use crate::terminal::backend::PtyBackend;
use crate::terminal::model::{SharedTerm, TermDimensions, default_bg_hsla, resolve_color};

/// Metrics passed from prepaint to paint.
#[derive(Clone, Copy)]
struct CellMetrics {
    width: Pixels,
    height: Pixels,
}

fn cell_metrics(window: &Window, font: &Font, font_size: Pixels) -> CellMetrics {
    let ts = window.text_system();
    let font_id = ts.resolve_font(font);
    let width = ts
        .em_advance(font_id, font_size)
        .unwrap_or_else(|_| font_size * 0.6);
    // Cell height = font's glyph extent plus a little leading, with a sane floor.
    let glyph_extent = ts.ascent(font_id, font_size) + ts.descent(font_id, font_size);
    let height = glyph_extent.max(font_size * 1.2).ceil();
    CellMetrics { width, height }
}

/// Build the terminal grid canvas element.
pub fn terminal_canvas(
    term: SharedTerm,
    backend: Arc<dyn PtyBackend>,
    font: Font,
    symbol_font: Font,
    font_size: Pixels,
    focus: FocusHandle,
) -> impl gpui::IntoElement {
    let prepaint_term = term.clone();
    let prepaint_font = font.clone();

    canvas(
        // PREPAINT: measure -> compute cols/rows -> resize term + backend.
        move |bounds: Bounds<Pixels>, window, _cx| {
            let metrics = cell_metrics(window, &prepaint_font, font_size);
            let cols = (f32::from(bounds.size.width) / f32::from(metrics.width)).floor() as usize;
            let rows = (f32::from(bounds.size.height) / f32::from(metrics.height)).floor() as usize;
            let cols = cols.max(2);
            let rows = rows.max(1);

            {
                use alacritty_terminal::grid::Dimensions;
                let mut t = prepaint_term.lock();
                if t.columns() != cols || t.screen_lines() != rows {
                    t.resize(TermDimensions::new(cols, rows));
                    backend.resize(cols as u16, rows as u16);
                }
            }
            metrics
        },
        // PAINT: grid -> glyphs.
        move |bounds: Bounds<Pixels>, metrics: CellMetrics, window, cx| {
            paint_grid(
                &term,
                &font,
                &symbol_font,
                font_size,
                &focus,
                bounds,
                metrics,
                window,
                cx,
            );
        },
    )
    .size_full()
}

/// Codepoints that belong to Nerd Font icon ranges (the three Private Use Areas).
/// These never appear in an ordinary monospace font, so on Linux — where gpui
/// 0.2.2 ignores `Font.fallbacks` and cosmic-text's auto-fallback is unreliable
/// for PUA — we route them explicitly to the symbol font.
fn is_nerd_font_glyph(c: char) -> bool {
    matches!(
        c as u32,
        0xE000..=0xF8FF        // BMP Private Use Area
        | 0xF0000..=0xFFFFD    // Supplementary PUA-A
        | 0x100000..=0x10FFFD  // Supplementary PUA-B
    )
}

#[allow(clippy::too_many_arguments)]
fn paint_grid(
    term: &SharedTerm,
    font: &Font,
    symbol_font: &Font,
    font_size: Pixels,
    focus: &FocusHandle,
    bounds: Bounds<Pixels>,
    metrics: CellMetrics,
    window: &mut Window,
    cx: &mut App,
) {
    let ts = window.text_system().clone();
    let origin = bounds.origin;
    let cw = metrics.width;
    let ch = metrics.height;
    let focused = focus.is_focused(window);

    // Whole-area default background.
    window.paint_quad(fill(bounds, default_bg_hsla()));

    let term = term.lock();
    let content = term.renderable_content();
    let colors = content.colors;
    let display_offset = content.display_offset as i32;
    let show_cursor = focused
        && content.mode.contains(TermMode::SHOW_CURSOR)
        && content.cursor.shape != CursorShape::Hidden;
    let cur_row = content.cursor.point.line.0 + display_offset;
    let cur_col = content.cursor.point.column.0 as i32;

    let bold_font = font.clone().bold();

    for cell in content.display_iter {
        let flags = cell.flags;
        // Skip the dummy cell that trails a wide (CJK) glyph.
        if flags.intersects(Flags::WIDE_CHAR_SPACER | Flags::LEADING_WIDE_CHAR_SPACER) {
            continue;
        }
        if flags.contains(Flags::HIDDEN) {
            continue;
        }

        let row = cell.point.line.0 + display_offset;
        let col = cell.point.column.0 as i32;
        if row < 0 || col < 0 {
            continue;
        }

        let x = origin.x + cw * col as f32;
        let y = origin.y + ch * row as f32;

        let is_cursor_block = show_cursor
            && row == cur_row
            && col == cur_col
            && content.cursor.shape == CursorShape::Block;

        // Resolve colors, applying INVERSE and a block cursor as a swap.
        let mut fg = resolve_color(cell.fg, colors);
        let mut bg = resolve_color(cell.bg, colors);
        let swap = flags.contains(Flags::INVERSE) ^ is_cursor_block;
        if swap {
            std::mem::swap(&mut fg, &mut bg);
        }

        let width_cells = if flags.contains(Flags::WIDE_CHAR) { 2.0 } else { 1.0 };

        // Background quad when the cell isn't the plain default.
        let is_default_bg =
            !swap && matches!(cell.bg, Color::Named(NamedColor::Background));
        if !is_default_bg {
            let cell_bounds = Bounds {
                origin: point(x, y),
                size: gpui::size(cw * width_cells, ch),
            };
            window.paint_quad(fill(cell_bounds, bg));
        }

        // Non-block cursors: thin overlay.
        if show_cursor && row == cur_row && col == cur_col && !is_cursor_block {
            paint_cursor_overlay(window, content.cursor.shape, point(x, y), cw, ch, fg);
        }

        let c = if cell.c == '\0' { ' ' } else { cell.c };
        if c == ' ' {
            continue;
        }

        if is_nerd_font_glyph(c) {
            // Proportional Symbols Nerd Font: shrink-to-fit + center, so wide
            // icons don't overflow into the next cell.
            paint_symbol(
                &ts,
                symbol_font,
                c,
                fg,
                font_size,
                point(x, y),
                cw * width_cells,
                ch,
                window,
                cx,
            );
        } else {
            let run_font = if flags.contains(Flags::BOLD) {
                &bold_font
            } else {
                font
            };
            let (text, run) = text_run(c, run_font, fg);
            let shaped = ts.shape_line(text, font_size, &[run], None);
            let _ = shaped.paint(point(x, y), ch, window, cx);
        }
    }
    drop(term);
}

/// Build a single-glyph `(text, TextRun)` pair.
fn text_run(c: char, font: &Font, color: Hsla) -> (gpui::SharedString, TextRun) {
    let text: gpui::SharedString = c.to_string().into();
    let run = TextRun {
        len: text.len(),
        font: font.clone(),
        color,
        background_color: None,
        underline: None,
        strikethrough: None,
    };
    (text, run)
}

/// Paint a Nerd Font symbol glyph, shrinking it to fit `allotted` width and
/// centering it horizontally within the cell.
#[allow(clippy::too_many_arguments)]
fn paint_symbol(
    ts: &gpui::WindowTextSystem,
    font: &Font,
    c: char,
    color: Hsla,
    font_size: Pixels,
    origin: Point<Pixels>,
    allotted: Pixels,
    ch: Pixels,
    window: &mut Window,
    cx: &mut App,
) {
    let (text, run) = text_run(c, font, color);
    let mut shaped = ts.shape_line(text, font_size, &[run], None);

    // Shrink oversized icons (the proportional font has many double-width glyphs).
    if shaped.width > allotted && shaped.width > px(0.0) {
        let scale = f32::from(allotted) / f32::from(shaped.width);
        let (text2, run2) = text_run(c, font, color);
        shaped = ts.shape_line(text2, font_size * scale, &[run2], None);
    }

    let x_off = ((f32::from(allotted) - f32::from(shaped.width)) / 2.0).max(0.0);
    let _ = shaped.paint(point(origin.x + px(x_off), origin.y), ch, window, cx);
}

fn paint_cursor_overlay(
    window: &mut Window,
    shape: CursorShape,
    origin: Point<Pixels>,
    cw: Pixels,
    ch: Pixels,
    color: Hsla,
) {
    match shape {
        CursorShape::Underline => {
            let h = px(2.0);
            window.paint_quad(fill(
                Bounds {
                    origin: point(origin.x, origin.y + ch - h),
                    size: gpui::size(cw, h),
                },
                color,
            ));
        }
        CursorShape::Beam => {
            window.paint_quad(fill(
                Bounds {
                    origin,
                    size: gpui::size(px(2.0), ch),
                },
                color,
            ));
        }
        CursorShape::HollowBlock => {
            // Unfocused-style outline (approximate with a thin top+bottom bar).
            window.paint_quad(fill(
                Bounds {
                    origin,
                    size: gpui::size(cw, px(1.0)),
                },
                color,
            ));
        }
        _ => {}
    }
}
