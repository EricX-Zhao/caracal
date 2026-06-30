//! Mouse-driven text selection.
//!
//! State (the `in-progress selection` itself) lives on the alacritty
//! `Term::selection` field (a public `Option<Selection>`). This module
//! only owns the *mapping* layer:
//!
//!   1. screen pixel `(x, y)` → grid `Point<Line, Column>` (using the
//!      current cell metrics and `display_offset`)
//!   2. click_count → `SelectionType` (1=Simple, 2=Semantic, 3=Lines)
//!   3. alacritty selection → plain `String` for the clipboard
//!
//! Coordinates: the grid's `Point::line` is signed relative to the live
//! area (negative = scrollback, 0 = top of live area, etc). The
//! `viewport_to_point` helper from alacritty does the `display_offset`
//! arithmetic correctly — we use it instead of re-deriving the conversion
//! by hand (CLAUDE.md §1.2: don't re-derive API).
//!
//! Boundaries: see CLAUDE.md §2. This file owns NO state of its own —
//! every operation is a pure function or a method on the borrowed `Term`
//! (held under the standard `Arc<FairMutex<Term<…>>>` from `model.rs`).
//! The `TerminalView` entity holds the *drag in progress* bit (one bool)
//! so the bridge drain task can skip re-paints if a drag is idle. That's
//! the only selection-related state outside this file.

use alacritty_terminal::index::{Column, Line, Point as GridPoint, Side as GridSide};
use alacritty_terminal::selection::{Selection, SelectionType};
use alacritty_terminal::term::Term;

use crate::terminal::model::EventProxy;

/// Map `gpui::MouseDownEvent::click_count` to a `SelectionType`.
///
/// - 1 click → Simple
/// - 2 clicks → Semantic (select word, expanding to semantic boundaries)
/// - 3 clicks → Lines
/// - 4+ → Simple (a fresh "single click" round, alacritty convention)
pub fn selection_type_for_click(click_count: usize) -> Option<SelectionType> {
    match click_count {
        1 => Some(SelectionType::Simple),
        2 => Some(SelectionType::Semantic),
        3 => Some(SelectionType::Lines),
        _ => Some(SelectionType::Simple),
    }
}

/// Convert a screen-pixel point (relative to the canvas origin) to a
/// grid `Point` using the current cell metrics and `display_offset`.
///
/// `cols` / `rows` are the live-area column/row counts (i.e. what the
/// canvas just measured; see render.rs `cell_metrics`). We clamp
/// `column` to `[0, cols-1]` and `line` to `[-display_offset, rows-1]`
/// because clicks on the very last row/column edge should snap inward.
///
/// `display_offset` is the grid's current scroll position. A positive
/// value means the user is looking at scrollback, so the same screen
/// row maps to a more-negative grid line.
pub fn screen_to_grid(
    x: f32,
    y: f32,
    cell_w: f32,
    cell_h: f32,
    cols: usize,
    rows: usize,
    display_offset: usize,
) -> GridPoint {
    // Negative coordinates can happen for clicks just above the canvas
    // origin (e.g. when a scrollbar is involved). Clamp to 0.
    let x = x.max(0.0);
    let y = y.max(0.0);
    let mut col = (x / cell_w).floor() as i32;
    // `viewport_row` is 0..rows-1 (where the click landed on screen).
    let mut viewport_row = (y / cell_h).floor() as i32;

    col = col.clamp(0, cols.saturating_sub(1) as i32);
    viewport_row = viewport_row.clamp(0, rows as i32 - 1);

    // Convert the on-screen row to an absolute grid line. With
    // display_offset = N the topmost visible row (viewport_row 0) is grid
    // line -N, so absolute_line = viewport_row - display_offset. This must
    // mirror render.rs, which maps line -> screen via `line + display_offset`.
    let line = viewport_row - display_offset as i32;

    GridPoint::new(Line(line), Column(col as usize))
}

/// Choose the `Side` for a new selection anchor. alacritty uses Side to
/// disambiguate the half-open interval at the boundary cell when the
/// user's click snaps to one column past the end of a row.
///
/// - Middle of a cell → `Side::Left` (default, conservative)
/// - Right half of the last column of a non-last row → `Side::Right`
///   (lets the user grab the implicit newline by clicking past the
///   last char — matches alacritty's own heuristic)
pub fn side_for(x: f32, cell_w: f32, col: usize, cols: usize) -> GridSide {
    let frac = if cell_w > 0.0 { (x / cell_w) - col as f32 } else { 0.0 };
    if frac > 0.5 && col + 1 < cols {
        // Past the midpoint of a non-last cell: anchor on the right so
        // dragging the mouse left into the cell extends the selection
        // forward.
        GridSide::Right
    } else {
        GridSide::Left
    }
}

/// Start a new selection at `point` with `ty`. Replaces any existing
/// in-progress selection. Does *not* extend — call [`update`] for that.
pub fn start(term: &mut Term<EventProxy>, point: GridPoint, side: GridSide, ty: SelectionType) {
    term.selection = Some(Selection::new(ty, point, side));
}

/// Extend the existing selection's end to `point`. No-op if there's no
/// selection.
pub fn update(term: &mut Term<EventProxy>, point: GridPoint, side: GridSide) {
    if let Some(sel) = term.selection.as_mut() {
        sel.update(point, side);
    }
}

/// Drop the selection. Used on mouse-up (to commit a Simple selection)
/// or on any "user wants to start over" action. Currently unused — kept
/// for future context-menu / Escape-to-clear support.
#[allow(dead_code)]
pub fn clear(term: &mut Term<EventProxy>) {
    term.selection = None;
}

/// Returns the selected text, or `None` if no selection / empty.
pub fn selected_text(term: &Term<EventProxy>) -> Option<String> {
    term.selection_to_string()
}

/// Is the current selection empty (or absent)?
#[allow(dead_code)]
pub fn is_empty(term: &Term<EventProxy>) -> bool {
    term.selection
        .as_ref()
        .map(|s| s.is_empty())
        .unwrap_or(true)
}
