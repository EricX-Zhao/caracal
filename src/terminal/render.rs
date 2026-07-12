//! Self-written GPUI canvas renderer: alacritty grid -> glyphs.
//!
//! Owns cell metrics, the size computation (cols/rows from the measured bounds),
//! and the resize sync to both the local grid and the backend (CLAUDE.md §2:
//! resize is the view's responsibility, computed from cell metrics). Phase 1
//! paints the visible screen, backgrounds, and the cursor. Selection highlight
//! and scrollback come in Phase 3.
//!
//! Glyph fallback (Nerd Font icons / powerline) is handled by gpui's font
//! fallback chain: the run font carries `fallbacks` (see `view::terminal_font`),
//! and gpui_wgpu's cosmic-text layer coverage-checks each char against the
//! primary font then the fallback chain — so we do *not* route codepoints to a
//! symbol font by hand (that fought cosmic's substitution and lost).

use std::sync::Arc;

use alacritty_terminal::vte::ansi::CursorShape;

use gpui::{
    App, Bounds, Entity, FocusHandle, Font, Hsla, Pixels, Point, Styled, TextAlign, TextRun,
    Window, canvas, fill, point, px,
};

use crate::terminal::backend::PtyBackend;
use crate::terminal::batching::{batch_bg_rects, batch_text_runs};
use crate::terminal::grid_snapshot::snapshot_content;
use crate::terminal::model::{SharedTerm, TermDimensions, default_bg_hsla};
use crate::terminal::view::TerminalView;

/// Metrics passed from prepaint to paint. Also used by `TerminalView::render`
/// (`pub(crate)`) to size the padding around the canvas in cell units.
#[derive(Clone, Copy)]
pub(crate) struct CellMetrics {
    pub(crate) width: Pixels,
    pub(crate) height: Pixels,
}

pub(crate) fn cell_metrics(window: &Window, font: &Font, font_size: Pixels) -> CellMetrics {
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
///
/// The `view` entity is given so the paint callback can stash the
/// freshly-measured cell metrics and current column/row count back
/// onto the [`TerminalView`] — the mouse handlers need those to convert
/// pixel coordinates into grid points (Phase 3 selection support).
/// We do this on every paint because the metrics depend on the
/// measured bounds, which can change at any frame (window resize,
/// font change, …).
pub fn terminal_canvas(
    view: Entity<TerminalView>,
    term: SharedTerm,
    backend: Arc<dyn PtyBackend>,
    font: Font,
    font_size: Pixels,
    focus: FocusHandle,
) -> impl gpui::IntoElement {
    let prepaint_term = term.clone();
    let prepaint_font = font.clone();
    let prepaint_view = view.clone();

    canvas(
        // PREPAINT: measure -> compute cols/rows -> resize term + backend.
        move |bounds: Bounds<Pixels>, window, cx| {
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

            // Stash the metrics on the view for the mouse handlers. We
            // do this unconditionally — even on a frame where the grid
            // size didn't change, the view's cached metrics may have
            // drifted (e.g. the user resized the window by a sub-cell
            // amount), and a slightly stale cache means selections
            // snap to the wrong cell. cx.update lets us mutate
            // the entity from inside a prepaint callback.
            let _ = prepaint_view.update(cx, |v, _cx| {
                v.remember_cell_metrics(
                    f32::from(metrics.width),
                    f32::from(metrics.height),
                    cols,
                    rows,
                    f32::from(bounds.origin.x),
                    f32::from(bounds.origin.y),
                );
            });

            metrics
        },
        // PAINT: grid -> glyphs.
        move |bounds: Bounds<Pixels>, metrics: CellMetrics, window, cx| {
            paint_grid(&term, &font, font_size, &focus, bounds, metrics, window, cx);
        },
    )
    .size_full()
}

#[allow(clippy::too_many_arguments)]
fn paint_grid(
    term: &SharedTerm,
    font: &Font,
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

    // The only lock acquisition in the paint path — dropped before we do
    // any shaping/painting, so the feeder thread is never blocked by a
    // slow frame (see grid_snapshot::snapshot_content's doc comment).
    let snapshot = snapshot_content(term, focused);

    // Background rects: one paint_quad per merged run instead of per cell.
    for row in 0..snapshot.rows {
        for span in batch_bg_rects(snapshot.row(row)) {
            let cell_bounds = Bounds {
                origin: point(origin.x + cw * span.start_col as f32, origin.y + ch * row as f32),
                size: gpui::size(cw * span.num_cells as f32, ch),
            };
            window.paint_quad(fill(cell_bounds, span.color));
        }
    }

    // Non-block cursor overlays (block cursors are already baked into the
    // swapped fg/bg colors inside the snapshot).
    if let Some(cursor) = &snapshot.cursor
        && cursor.shape != CursorShape::Block
    {
        let p = point(
            origin.x + cw * cursor.col as f32,
            origin.y + ch * cursor.row as f32,
        );
        paint_cursor_overlay(window, cursor.shape, p, cw, ch, cursor.color);
    }

    // Text runs: one shape_line + paint per merged run instead of per glyph.
    let bold_font = font.clone().bold();
    for row in 0..snapshot.rows {
        for span in batch_text_runs(snapshot.row(row)) {
            let run_font = if span.style.bold { &bold_font } else { font };
            let (text, run) = text_run(span.text, run_font, span.style.fg);
            let shaped = ts.shape_line(text, font_size, &[run], Some(cw));
            let x = origin.x + cw * span.start_col as f32;
            let y = origin.y + ch * row as f32;
            let _ = shaped.paint(point(x, y), ch, TextAlign::Left, None, window, cx);
        }
    }
}

/// Build a `(text, TextRun)` pair for a whole batched span (one or more
/// characters sharing the same style).
fn text_run(text: String, font: &Font, color: Hsla) -> (gpui::SharedString, TextRun) {
    let text: gpui::SharedString = text.into();
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
            // Unfocused-style outline (approximate with a thin top bar).
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
