//! Owns the *only* lock acquisition used for painting the terminal grid.
//!
//! `snapshot_content` copies the visible grid out of the alacritty `Term`
//! into an owned, gpui-independent `GridSnapshot` and drops the lock before
//! returning. This exists so `render::paint_grid` never holds the `Term`'s
//! `FairMutex` while doing the (much slower) text-shaping/painting work —
//! holding it there blocks the `caracal-feeder` thread from advancing the
//! ANSI parser for the whole frame, which is a real source of stutter under
//! sustained output.

use alacritty_terminal::term::TermMode;
use alacritty_terminal::term::cell::Flags;
use alacritty_terminal::vte::ansi::{Color, CursorShape, NamedColor};

use gpui::Hsla;

use crate::terminal::model::{SharedTerm, resolve_color, selection_bg_hsla};

/// One grid cell's paint-relevant data, fully resolved (colors, selection,
/// cursor swap already applied) so painting never needs to touch the `Term`.
#[derive(Clone, Copy)]
pub struct SnapCell {
    pub c: char,
    pub fg: Hsla,
    pub bg: Hsla,
    pub bold: bool,
    /// Whether this cell needs its own background quad (selected, a
    /// non-default background color, or a block cursor's swapped fill). `false`
    /// for blank/default cells and for the trailing spacer slot of a wide character.
    pub paint_bg: bool,
    /// Whether this is the *leading* cell of a wide (CJK) character — its
    /// background/overlay should claim two grid columns.
    pub wide: bool,
}

impl SnapCell {
    fn blank() -> Self {
        Self {
            c: ' ',
            fg: Hsla::default(),
            bg: Hsla::default(),
            bold: false,
            paint_bg: false,
            wide: false,
        }
    }
}

/// Cursor paint info, already resolved to a screen-space row/col.
pub struct CursorPaint {
    pub row: usize,
    pub col: usize,
    pub shape: CursorShape,
    pub color: Hsla,
}

/// An owned copy of the visible grid, safe to read after the `Term` lock
/// has been dropped.
pub struct GridSnapshot {
    pub cols: usize,
    pub rows: usize,
    pub cursor: Option<CursorPaint>,
    cells: Vec<SnapCell>,
}

impl GridSnapshot {
    pub fn row(&self, r: usize) -> &[SnapCell] {
        &self.cells[r * self.cols..(r + 1) * self.cols]
    }
}

/// Lock the `Term`, copy the visible grid + cursor into an owned snapshot,
/// and drop the lock before returning. `focused` gates cursor visibility
/// exactly like the old `paint_grid` did (a block cursor never swaps colors
/// when the terminal isn't focused).
pub fn snapshot_content(term: &SharedTerm, focused: bool) -> GridSnapshot {
    use alacritty_terminal::grid::Dimensions;

    let term = term.lock();
    let cols = term.columns();
    let rows = term.screen_lines();
    let content = term.renderable_content();
    let colors = content.colors;
    let selection = content.selection;
    let display_offset = content.display_offset as i32;
    let show_cursor = focused
        && content.mode.contains(TermMode::SHOW_CURSOR)
        && content.cursor.shape != CursorShape::Hidden;
    let cur_row = content.cursor.point.line.0 + display_offset;
    let cur_col = content.cursor.point.column.0 as i32;

    let mut cells = vec![SnapCell::blank(); cols * rows];

    for cell in content.display_iter {
        let flags = cell.flags;
        if flags.intersects(Flags::WIDE_CHAR_SPACER | Flags::LEADING_WIDE_CHAR_SPACER) {
            continue;
        }
        if flags.contains(Flags::HIDDEN) {
            continue;
        }

        let row = cell.point.line.0 + display_offset;
        let col = cell.point.column.0 as i32;
        if row < 0 || col < 0 || row as usize >= rows || col as usize >= cols {
            continue;
        }

        let is_cursor_block = show_cursor
            && row == cur_row
            && col == cur_col
            && content.cursor.shape == CursorShape::Block;

        let mut fg = resolve_color(cell.fg, colors);
        let mut bg = resolve_color(cell.bg, colors);
        let swap = flags.contains(Flags::INVERSE) ^ is_cursor_block;
        if swap {
            std::mem::swap(&mut fg, &mut bg);
        }

        let selected = selection.as_ref().is_some_and(|range| range.contains(cell.point));
        let is_default_bg = !swap && matches!(cell.bg, Color::Named(NamedColor::Background));
        let (paint_bg, bg) = if selected {
            (true, selection_bg_hsla())
        } else {
            (!is_default_bg, bg)
        };

        let c = if cell.c == '\0' { ' ' } else { cell.c };
        let idx = row as usize * cols + col as usize;
        cells[idx] = SnapCell {
            c,
            fg,
            bg,
            bold: flags.contains(Flags::BOLD),
            paint_bg,
            wide: flags.contains(Flags::WIDE_CHAR),
        };
    }

    // The cursor's logical column is always a cell's leading column (never
    // a wide-char spacer slot), so `cells[idx].fg` is always the real
    // resolved foreground for the overlay color.
    let cursor = if show_cursor && cur_row >= 0 && (cur_row as usize) < rows && cur_col >= 0 && (cur_col as usize) < cols
    {
        let idx = cur_row as usize * cols + cur_col as usize;
        Some(CursorPaint {
            row: cur_row as usize,
            col: cur_col as usize,
            shape: content.cursor.shape,
            color: cells[idx].fg,
        })
    } else {
        None
    };

    GridSnapshot { cols, rows, cursor, cells }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::terminal::model::new_term;
    use alacritty_terminal::vte::ansi::Processor;

    fn term_with(bytes: &[u8], cols: usize, rows: usize) -> SharedTerm {
        let (tx, _rx) = flume::unbounded();
        let term = new_term(cols, rows, tx);
        {
            let mut t = term.lock();
            let mut parser: Processor = Processor::new();
            parser.advance(&mut *t, bytes);
        }
        term
    }

    #[test]
    fn plain_text_lands_at_correct_cells() {
        let term = term_with(b"hi", 10, 3);
        let snap = snapshot_content(&term, true);
        assert_eq!(snap.cols, 10);
        assert_eq!(snap.rows, 3);
        assert_eq!(snap.row(0)[0].c, 'h');
        assert_eq!(snap.row(0)[1].c, 'i');
        assert_eq!(snap.row(0)[2].c, ' ');
        // Column 2 is where the cursor sits after typing "hi" — its own
        // block-cursor fill is asserted separately in
        // `block_cursor_paints_its_own_background`. Check a blank cell the
        // cursor does *not* occupy instead.
        assert!(!snap.row(0)[5].paint_bg);
    }

    #[test]
    fn block_cursor_paints_its_own_background() {
        // The default cursor shape is Block, and paint_cursor_overlay in
        // render.rs is a no-op for Block — the background quad painted when
        // `paint_bg` is true (via the fg/bg swap) is the *only* thing that
        // renders a block cursor's solid fill. A default-background cell
        // under the cursor must still get paint_bg == true.
        let term = term_with(b"hi", 10, 3);
        let snap = snapshot_content(&term, true);
        assert!(snap.row(0)[2].paint_bg, "block cursor must paint its own background");
    }

    #[test]
    fn sgr_background_marks_paint_bg() {
        // SGR 41 = red background, one 'X', SGR 0 resets.
        let term = term_with(b"\x1b[41mX\x1b[0mY", 10, 3);
        let snap = snapshot_content(&term, true);
        assert!(snap.row(0)[0].paint_bg, "colored cell must be marked for bg paint");
        assert!(!snap.row(0)[1].paint_bg, "default-bg cell must not be marked");
    }

    #[test]
    fn cursor_present_only_when_focused() {
        let term = term_with(b"x", 10, 3);
        let focused = snapshot_content(&term, true);
        let unfocused = snapshot_content(&term, false);
        assert!(focused.cursor.is_some());
        assert!(unfocused.cursor.is_none());
    }
}
