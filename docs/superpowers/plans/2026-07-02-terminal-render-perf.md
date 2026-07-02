# Terminal Render Performance Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Cut the CPU cost and stutter of `src/terminal/render.rs`'s per-frame grid paint by replacing its per-cell `shape_line`/`paint_quad` calls with per-run batched calls, and by releasing the `Term` lock before painting instead of holding it for the whole draw.

**Architecture:** Split the current monolithic `paint_grid` into three layers: (1) `grid_snapshot::snapshot_content` takes the alacritty `Term` lock just long enough to copy the visible grid into an owned, gpui-independent `GridSnapshot`, then drops the lock; (2) `batching::batch_text_runs` / `batching::batch_bg_rects` are pure functions that group contiguous same-style cells in a snapshot row into runs/rects (no gpui `Window`/`App` needed, fully unit-testable); (3) `render::paint_grid` calls the snapshot, batches each row, and issues one `shape_line`/`paint_quad` call per run/rect instead of one per cell. This mirrors the pattern Zed's `terminal_element.rs` (`BatchedTextRun` + `merge_background_regions`) and Ghostty's renderer (row-granular dirty tracking, documented as fixing a measured 96%-of-frame-time shaping bottleneck) both use, adapted to gpui's immediate-mode `canvas` element.

**Tech Stack:** Rust, gpui (`shape_line`/`ShapedLine::paint`/`paint_quad`), alacritty_terminal 0.26 (`Term`, `FairMutex`, `renderable_content`).

## Global Constraints

- No new crates. Everything is built from `gpui`, `alacritty_terminal`, and `std`, exactly as today's `Cargo.toml` already provides.
- Stay inside gpui's `canvas`/paint model — no raw GPU/wgpu access. This plan is a CPU-side batching change, not a renderer rewrite.
- `src/terminal/` must stay free of `gpui_component` imports (CLAUDE.md §1 boundary; self-check via `grep -r gpui_component src/terminal/`). New files must not violate this.
- Visual output must not regress: same colors, same selection highlight, same wide/CJK character handling, same cursor shapes (block/underline/beam/hollow), same behavior when unfocused.
- Follow the existing inline `#[cfg(test)] mod tests { #[test] fn ... }` convention (see `src/terminal/keymap.rs:189-210`) — no separate `tests/` directory.
- Every new `pub mod` goes into `src/terminal/mod.rs` alongside the existing module list.

---

## File Structure

- Create: `src/terminal/grid_snapshot.rs` — `SnapCell`, `GridSnapshot`, `CursorPaint`, `snapshot_content()`. Owns the *only* place that locks the `Term` for painting purposes.
- Create: `src/terminal/batching.rs` — `CellStyle`, `TextSpan`, `BgSpan`, `batch_text_runs()`, `batch_bg_rects()`. Pure, no gpui `Window`/`App`, no alacritty `Term`.
- Modify: `src/terminal/render.rs` — `paint_grid` rewritten to call the two modules above instead of its current per-cell loop (`render.rs:118-230` today).
- Modify: `src/terminal/mod.rs` — add `pub mod grid_snapshot;` and `pub mod batching;`.

---

### Task 1: `GridSnapshot` — copy the grid out from under the lock

**Files:**
- Create: `src/terminal/grid_snapshot.rs`
- Modify: `src/terminal/mod.rs` (add `pub mod grid_snapshot;`)

**Interfaces:**
- Produces: `pub struct SnapCell { pub c: char, pub fg: Hsla, pub bg: Hsla, pub bold: bool, pub paint_bg: bool, pub wide: bool }` (all fields `pub`, `Clone + Copy`)
- Produces: `pub struct CursorPaint { pub row: usize, pub col: usize, pub shape: CursorShape, pub color: Hsla }`
- Produces: `pub struct GridSnapshot { pub cols: usize, pub rows: usize, pub cursor: Option<CursorPaint>, cells: Vec<SnapCell> }` with `pub fn row(&self, r: usize) -> &[SnapCell]`
- Produces: `pub fn snapshot_content(term: &SharedTerm, focused: bool) -> GridSnapshot`

This function is the *only* place `render.rs` will touch the `Term` lock. It replicates the color/cursor/selection resolution logic currently inline in `render.rs:138-229`, but writes the result into an owned `Vec<SnapCell>` instead of painting — so the lock can be dropped before any gpui work happens (today's `render.rs:138` holds the `FairMutex` for the entire paint, blocking the `caracal-feeder` thread from advancing the ANSI parser for the whole draw).

- [ ] **Step 1: Write the failing tests**

```rust
// src/terminal/grid_snapshot.rs (bottom of file, after the implementation stub below)
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
            let mut parser = Processor::new();
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
        assert!(!snap.row(0)[2].paint_bg);
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
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test --lib terminal::grid_snapshot -- --nocapture`
Expected: FAIL to compile — `snapshot_content`, `SnapCell`, `GridSnapshot` don't exist yet.

- [ ] **Step 3: Write the implementation**

```rust
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
    /// Whether this cell needs its own background quad (selected, or a
    /// non-default background color). `false` for blank/default cells and
    /// for the trailing spacer slot of a wide character.
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
```

- [ ] **Step 4: Register the module**

In `src/terminal/mod.rs`, add (alphabetical, matching the existing list style):

```rust
pub mod backend;
pub mod batching;
pub mod bridge;
pub mod grid_snapshot;
pub mod keymap;
pub mod model;
pub mod render;
pub mod scrollback;
pub mod selection;
pub mod ssh;
pub mod view;
```

(`batching` doesn't exist yet — Task 2 creates it. Adding the line now means Task 2 doesn't need to touch `mod.rs` again.)

- [ ] **Step 5: Run the tests to verify they pass**

Run: `cargo test --lib terminal::grid_snapshot -- --nocapture`
Expected: PASS (3 tests: `plain_text_lands_at_correct_cells`, `sgr_background_marks_paint_bg`, `cursor_present_only_when_focused`)

- [ ] **Step 6: Run the full test suite and commit**

Run: `cargo test`
Expected: PASS, no regressions elsewhere.

```bash
git add src/terminal/grid_snapshot.rs src/terminal/mod.rs
git commit -m "terminal: add GridSnapshot to copy the grid out from under the Term lock"
```

---

### Task 2: `batch_text_runs` — merge contiguous same-style cells into text spans

**Files:**
- Create: `src/terminal/batching.rs`

**Interfaces:**
- Consumes: `crate::terminal::grid_snapshot::SnapCell` (from Task 1)
- Produces: `pub struct CellStyle { pub fg: Hsla, pub bold: bool }`, `pub struct TextSpan { pub start_col: usize, pub text: String, pub style: CellStyle }`, `pub fn batch_text_runs(row: &[SnapCell]) -> Vec<TextSpan>`

Note on wide characters: a wide (CJK) cell *can* merge into a run with a same-style cell immediately before it (they're truly column-adjacent), but nothing can merge into a run *after* a wide cell, because its trailing spacer slot (`c == ' '`) always breaks contiguity. This is intentional — it's the same emergent behavior Zed's `BatchedTextRun::can_append` produces, and it's required for gpui's `force_width` cell-quantization (`shape_line(..., Some(cw))`) to place every subsequent glyph at the correct column: each run's `start_col` is independently authoritative for painting, so a run boundary right after a wide glyph costs nothing but an extra `shape_line` call.

- [ ] **Step 1: Write the failing tests**

```rust
// src/terminal/batching.rs
use gpui::Hsla;

use crate::terminal::grid_snapshot::SnapCell;

#[derive(Clone, Copy)]
pub struct CellStyle {
    pub fg: Hsla,
    pub bold: bool,
}

impl CellStyle {
    fn matches(&self, other: &CellStyle) -> bool {
        self.bold == other.bold && hsla_eq(self.fg, other.fg)
    }
}

fn hsla_eq(a: Hsla, b: Hsla) -> bool {
    a.h == b.h && a.s == b.s && a.l == b.l && a.a == b.a
}

pub struct TextSpan {
    pub start_col: usize,
    pub text: String,
    pub style: CellStyle,
}

/// Group a row's non-space cells into runs of contiguous, same-style text.
/// Stub — Step 3 fills this in.
pub fn batch_text_runs(_row: &[SnapCell]) -> Vec<TextSpan> {
    unimplemented!()
}

#[cfg(test)]
mod text_run_tests {
    use super::*;
    use gpui::hsla;

    fn cell(c: char, fg: Hsla, bold: bool) -> SnapCell {
        SnapCell { c, fg, bg: hsla(0.0, 0.0, 0.0, 1.0), bold, paint_bg: false, wide: false }
    }

    #[test]
    fn merges_same_style_run() {
        let red = hsla(0.0, 1.0, 0.5, 1.0);
        let row = vec![cell('a', red, false), cell('b', red, false), cell('c', red, false)];
        let spans = batch_text_runs(&row);
        assert_eq!(spans.len(), 1);
        assert_eq!(spans[0].start_col, 0);
        assert_eq!(spans[0].text, "abc");
    }

    #[test]
    fn breaks_on_style_change() {
        let red = hsla(0.0, 1.0, 0.5, 1.0);
        let blue = hsla(0.6, 1.0, 0.5, 1.0);
        let row = vec![cell('a', red, false), cell('b', blue, false)];
        let spans = batch_text_runs(&row);
        assert_eq!(spans.len(), 2);
        assert_eq!(spans[1].start_col, 1);
        assert_eq!(spans[1].text, "b");
    }

    #[test]
    fn breaks_on_space_gap() {
        let red = hsla(0.0, 1.0, 0.5, 1.0);
        let mut row = vec![cell(' ', red, false); 5];
        row[0] = cell('a', red, false);
        row[3] = cell('b', red, false);
        let spans = batch_text_runs(&row);
        assert_eq!(spans.len(), 2);
        assert_eq!(spans[0].start_col, 0);
        assert_eq!(spans[1].start_col, 3);
    }

    #[test]
    fn wide_char_merges_backward_but_isolates_forward() {
        let red = hsla(0.0, 1.0, 0.5, 1.0);
        let mut row = vec![cell(' ', red, false); 4];
        row[0] = cell('a', red, false);
        row[1] = SnapCell { c: '\u{4e2d}', fg: red, bg: hsla(0.0, 0.0, 0.0, 1.0), bold: false, paint_bg: false, wide: true };
        // row[2] is the wide char's spacer slot — stays blank, never read.
        row[3] = cell('b', red, false);
        let spans = batch_text_runs(&row);
        assert_eq!(spans.len(), 2);
        assert_eq!(spans[0].start_col, 0);
        assert_eq!(spans[0].text, "a\u{4e2d}");
        assert_eq!(spans[1].start_col, 3);
        assert_eq!(spans[1].text, "b");
    }
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test --lib terminal::batching::text_run_tests -- --nocapture`
Expected: FAIL — `unimplemented!()` panics on every test.

- [ ] **Step 3: Implement `batch_text_runs`**

Replace the stub body:

```rust
pub fn batch_text_runs(row: &[SnapCell]) -> Vec<TextSpan> {
    let mut spans = Vec::new();
    let mut current: Option<TextSpan> = None;
    let mut col = 0usize;

    while col < row.len() {
        let cell = row[col];
        let step = if cell.wide { 2 } else { 1 };

        if cell.c == ' ' {
            if let Some(span) = current.take() {
                spans.push(span);
            }
            col += step;
            continue;
        }

        let style = CellStyle { fg: cell.fg, bold: cell.bold };
        let expected_col = current.as_ref().map(|s| s.start_col + s.text.chars().count());

        match &mut current {
            Some(span) if span.style.matches(&style) && expected_col == Some(col) => {
                span.text.push(cell.c);
            }
            _ => {
                if let Some(span) = current.take() {
                    spans.push(span);
                }
                current = Some(TextSpan { start_col: col, text: cell.c.to_string(), style });
            }
        }
        col += step;
    }

    if let Some(span) = current.take() {
        spans.push(span);
    }
    spans
}
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test --lib terminal::batching::text_run_tests -- --nocapture`
Expected: PASS (4 tests)

- [ ] **Step 5: Commit**

```bash
git add src/terminal/batching.rs
git commit -m "terminal: add batch_text_runs to merge same-style cells into text spans"
```

---

### Task 3: `batch_bg_rects` — merge contiguous same-color cells into background spans

**Files:**
- Modify: `src/terminal/batching.rs` (same file as Task 2, additive)

**Interfaces:**
- Consumes: `SnapCell` (Task 1), `hsla_eq` (private helper already in `batching.rs` from Task 2)
- Produces: `pub struct BgSpan { pub start_col: usize, pub num_cells: usize, pub color: Hsla }`, `pub fn batch_bg_rects(row: &[SnapCell]) -> Vec<BgSpan>`

Unlike text runs, a background rect legitimately spans the *entire* wide-character cell (2 columns) in one step, and a same-color rect can continue seamlessly across a wide character's spacer slot into the next real cell (the spacer is simply skipped, never visited, via the same `step`-based column walk used in Task 2).

- [ ] **Step 1: Write the failing tests**

```rust
// Append to src/terminal/batching.rs, above the existing #[cfg(test)] module or as a second one.

pub struct BgSpan {
    pub start_col: usize,
    pub num_cells: usize,
    pub color: Hsla,
}

/// Group a row's non-default-background cells into merged rects.
/// Stub — Step 3 fills this in.
pub fn batch_bg_rects(_row: &[SnapCell]) -> Vec<BgSpan> {
    unimplemented!()
}

#[cfg(test)]
mod bg_rect_tests {
    use super::*;
    use gpui::hsla;

    fn bg_cell(bg: Hsla) -> SnapCell {
        SnapCell { c: ' ', fg: hsla(0.0, 0.0, 0.0, 1.0), bg, bold: false, paint_bg: true, wide: false }
    }

    fn default_cell() -> SnapCell {
        SnapCell { c: ' ', fg: hsla(0.0, 0.0, 0.0, 1.0), bg: hsla(0.0, 0.0, 0.0, 1.0), bold: false, paint_bg: false, wide: false }
    }

    #[test]
    fn merges_contiguous_same_color() {
        let red = hsla(0.0, 1.0, 0.5, 1.0);
        let row = vec![bg_cell(red), bg_cell(red), bg_cell(red)];
        let spans = batch_bg_rects(&row);
        assert_eq!(spans.len(), 1);
        assert_eq!(spans[0].start_col, 0);
        assert_eq!(spans[0].num_cells, 3);
    }

    #[test]
    fn breaks_on_color_change() {
        let red = hsla(0.0, 1.0, 0.5, 1.0);
        let blue = hsla(0.6, 1.0, 0.5, 1.0);
        let row = vec![bg_cell(red), bg_cell(blue)];
        let spans = batch_bg_rects(&row);
        assert_eq!(spans.len(), 2);
    }

    #[test]
    fn skips_default_background_cells() {
        let red = hsla(0.0, 1.0, 0.5, 1.0);
        let row = vec![default_cell(), bg_cell(red)];
        let spans = batch_bg_rects(&row);
        assert_eq!(spans.len(), 1);
        assert_eq!(spans[0].start_col, 1);
    }

    #[test]
    fn wide_cell_claims_two_columns_and_merges_across_its_spacer() {
        let red = hsla(0.0, 1.0, 0.5, 1.0);
        let mut row = vec![bg_cell(red), bg_cell(red), bg_cell(red)];
        row[0].wide = true; // occupies columns 0-1; row[1] is its spacer slot
        let spans = batch_bg_rects(&row);
        // The wide cell's rect (2 cols) continues seamlessly into column 2.
        assert_eq!(spans.len(), 1);
        assert_eq!(spans[0].num_cells, 3);
    }
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test --lib terminal::batching::bg_rect_tests -- --nocapture`
Expected: FAIL — `unimplemented!()` panics.

- [ ] **Step 3: Implement `batch_bg_rects`**

```rust
pub fn batch_bg_rects(row: &[SnapCell]) -> Vec<BgSpan> {
    let mut spans = Vec::new();
    let mut current: Option<BgSpan> = None;
    let mut col = 0usize;

    while col < row.len() {
        let cell = row[col];
        let step = if cell.wide { 2 } else { 1 };

        if !cell.paint_bg {
            if let Some(span) = current.take() {
                spans.push(span);
            }
            col += step;
            continue;
        }

        match &mut current {
            Some(span) if hsla_eq(span.color, cell.bg) && span.start_col + span.num_cells == col => {
                span.num_cells += step;
            }
            _ => {
                if let Some(span) = current.take() {
                    spans.push(span);
                }
                current = Some(BgSpan { start_col: col, num_cells: step, color: cell.bg });
            }
        }
        col += step;
    }

    if let Some(span) = current.take() {
        spans.push(span);
    }
    spans
}
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test --lib terminal::batching -- --nocapture`
Expected: PASS (8 tests total: 4 from Task 2 + 4 from this task)

- [ ] **Step 5: Commit**

```bash
git add src/terminal/batching.rs
git commit -m "terminal: add batch_bg_rects to merge same-color cells into background spans"
```

---

### Task 4: Rewrite `paint_grid` to consume the snapshot and batched spans

**Files:**
- Modify: `src/terminal/render.rs:118-244` (the `paint_grid` function and its `text_run` helper)

**Interfaces:**
- Consumes: `grid_snapshot::snapshot_content` (Task 1), `batching::batch_text_runs` / `batching::batch_bg_rects` (Tasks 2-3)
- Produces: same public signature for `terminal_canvas` — this task is purely internal to `paint_grid`, no callers change.

This removes the per-cell loop entirely, including the lock (`term.lock()` at the old `render.rs:138`, held through the whole function). Painting order is preserved: whole-area default background, then per-row background rects, then the cursor overlay (non-block shapes only — block cursors are already baked into the swapped colors in the snapshot), then per-row text runs — matching the old per-cell order of bg → overlay → glyph.

- [ ] **Step 1: Replace the top-of-file imports**

In `src/terminal/render.rs`, replace:

```rust
use alacritty_terminal::term::TermMode;
use alacritty_terminal::term::cell::Flags;
use alacritty_terminal::vte::ansi::{Color, CursorShape, NamedColor};

use gpui::{
    App, Bounds, Entity, FocusHandle, Font, Hsla, Pixels, Point, Styled, TextAlign, TextRun,
    Window, canvas, fill, point, px,
};

use crate::terminal::backend::PtyBackend;
use crate::terminal::model::{
    SharedTerm, TermDimensions, default_bg_hsla, resolve_color, selection_bg_hsla,
};
use crate::terminal::view::TerminalView;
```

with:

```rust
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
```

- [ ] **Step 2: Replace `paint_grid` and `text_run`**

Replace the entire block from `fn paint_grid(` through the end of `fn text_run(...)` (originally `render.rs:118-244`) with:

```rust
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
            let (text, run) = text_run(&span.text, run_font, span.style.fg);
            let shaped = ts.shape_line(text, font_size, &[run], Some(cw));
            let x = origin.x + cw * span.start_col as f32;
            let y = origin.y + ch * row as f32;
            let _ = shaped.paint(point(x, y), ch, TextAlign::Left, None, window, cx);
        }
    }
}

/// Build a `(text, TextRun)` pair for a whole batched span (one or more
/// characters sharing the same style).
fn text_run(text: &str, font: &Font, color: Hsla) -> (gpui::SharedString, TextRun) {
    let text: gpui::SharedString = text.to_string().into();
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
```

`paint_cursor_overlay` (the `match shape { ... }` function below `text_run` in the original file) is unchanged — leave it exactly as-is.

- [ ] **Step 3: Build**

Run: `cargo build`
Expected: compiles cleanly. If `clippy::too_many_arguments` or an unused-import warning appears, fix it — everything above is meant to compile as-is against the `gpui`/`alacritty_terminal` versions pinned in `Cargo.toml`.

- [ ] **Step 4: Run the full test suite**

Run: `cargo test`
Expected: PASS, including the `grid_snapshot` and `batching` tests from Tasks 1-3.

- [ ] **Step 5: Commit**

```bash
git add src/terminal/render.rs
git commit -m "terminal: paint from batched runs/rects instead of per-cell shape_line/paint_quad"
```

---

### Task 5: Manual verification in the running app

**Files:** none (verification only)

This is the step that a test suite can't cover — gpui painting needs a real window. Use the project's `run` skill (or `cargo run` directly) and check every visual path the rewrite touches.

- [ ] **Step 1: Launch the app**

Use the `run` skill to start caracal, or:

```bash
cargo run
```

- [ ] **Step 2: Plain text and scrolling**

Run `ls -la /usr/bin` and scroll up/down through the output. Confirm text renders at the correct columns, no glyphs are missing or shifted.

- [ ] **Step 3: Colors and backgrounds**

Run `ls --color=always /usr/bin | head -50` (or `grep --color=always -r foo .`) — confirm foreground colors and highlighted-match backgrounds look identical to before the change, with no seams or gaps at run boundaries.

- [ ] **Step 4: Wide/CJK characters**

Run `echo "abc中文测试def"` and, if available, open something with a status bar containing icons (`htop`, a Nerd-Font-using prompt). Confirm wide characters render at the correct width and don't shift trailing text.

- [ ] **Step 5: Selection**

Click-drag to select a few lines of colored output, confirm the selection highlight still covers exactly the selected cells (including wide characters), and `Ctrl+Shift+C` still copies the right text.

- [ ] **Step 6: Cursor shapes and focus**

Confirm the cursor renders correctly focused (block, swapped colors) and unfocused (no cursor drawn, per `cursor_present_only_when_focused` from Task 1). If the shell/config exercises underline or beam cursors (`\e[3 q` / `\e[5 q`), confirm those overlays still show.

- [ ] **Step 7: Subjective smoothness check**

Run a heavy-output command, e.g. `yes | head -200000` or `find / 2>/dev/null | head -20000`, and confirm scrolling/output feels smoother than before the change (no gating on hard numbers — this is a sanity check, not a benchmark). Optionally compare CPU usage in `top`/`htop` for the same command before and after this branch.

- [ ] **Step 8: If everything checks out, this phase is done — no commit needed for this task** (verification only, nothing to add to git).

---

## Phase 2 (optional follow-up, measure Phase 1 first)

### Task 6: Row-level content cache to skip re-batching unchanged rows

**Only do this if, after Task 5, profiling still shows `paint_grid` as a hot path** (e.g. `perf top` while catting a large file still shows meaningful time in `batch_text_runs`/`batch_bg_rects`/`shape_line`). Phase 1 already gets most of the win Ghostty's dirty-tracking targets, because gpui's own `LineLayoutCache` (`text_system/line_layout.rs`) transparently caches `shape_line` results keyed by `(text, font, force_width)` — once a row's batched text is unchanged frame-to-frame, the shaping itself is already close to free. What Phase 2 buys on top of that is skipping the O(cols) *batching* loop (the `batch_text_runs`/`batch_bg_rects` scan itself) for rows whose content didn't change since the last frame — worthwhile mainly on very wide/tall terminals with lots of unchanging chrome (e.g. `vim`/`htop` status lines) where that scan itself becomes non-trivial.

**Files:**
- Create: `src/terminal/row_cache.rs` — `RowCache`, `RowPlan`, `RowCache::diff_and_get(&mut self, row_index: usize, cells: &[SnapCell]) -> &RowPlan`
- Modify: `src/terminal/view.rs` — add `row_cache: Rc<RefCell<row_cache::RowCache>>` field to `TerminalView`, constructed once in `with_backend` alongside `term`/`backend`
- Modify: `src/terminal/render.rs` — `terminal_canvas`/`paint_grid` thread the cache through and consult it per row instead of unconditionally calling `batch_text_runs`/`batch_bg_rects`

Design: `RowCache` holds, per viewport row index, a hash of that row's `SnapCell` content (`char`, `fg`, `bg`, `bold`, `paint_bg`, `wide` — everything that affects the batched output) plus the previously-computed `Vec<TextSpan>`/`Vec<BgSpan>`. On each paint, hash the row's current cells; if it matches the cached hash *and* the row doesn't contain the cursor (the cursor row must always be treated as dirty, since focus/blink state isn't part of the cell hash), reuse the cached spans instead of recomputing them. Otherwise recompute and overwrite the cache entry.

This task is intentionally left at this design-level of detail rather than full TDD steps: implement it the same way as Tasks 1-3 (pure `RowCache` struct + hashing logic unit-tested with hand-built `SnapCell` fixtures, no gpui dependency), wire it into `render.rs` the same way Task 4 wired the batching functions, then repeat Task 5's manual verification pass — paying particular attention to: resizing (cache must be invalidated/resized when `cols`/`rows` change), scrolling (every row's content changes when `display_offset` changes, so the cache should naturally miss on scroll — verify it does), and cursor blink (must never go stale).

---

## Self-Review

**Spec coverage:**
- Per-cell `shape_line`/`paint()` → per-run batching: Task 4 (via Task 2's `batch_text_runs`). ✓
- Per-cell `paint_quad` → merged background rects: Task 4 (via Task 3's `batch_bg_rects`). ✓
- Term lock held through the whole paint → dropped before painting: Task 1's `snapshot_content`, consumed by Task 4. ✓
- Row-level dirty tracking (Ghostty-inspired, flagged as the smaller/optional win): Task 6, explicitly scoped as an optional Phase 2. ✓
- No visual regressions (colors, selection, wide chars, cursor, focus): covered by Task 1's tests, Task 2/3's tests, and Task 5's manual pass. ✓

**Placeholder scan:** no TODOs, no "add appropriate handling" — every step has complete, concrete code or an exact command with expected output. Task 6 is deliberately lighter-detail (it's an optional follow-up gated on profiling data that doesn't exist yet), but still names exact files, exact struct/function signatures, and a concrete cache-invalidation rule rather than "handle caching appropriately."

**Type consistency:** `SnapCell` (Task 1) is consumed identically by `batch_text_runs`/`batch_bg_rects` (Tasks 2-3) and by `render.rs` (Task 4) via `GridSnapshot::row(&self, r: usize) -> &[SnapCell]`. `TextSpan`/`BgSpan`/`CellStyle` field names match between their Task 2/3 definitions and their Task 4 usage (`span.start_col`, `span.text`, `span.style.bold`, `span.style.fg`, `span.num_cells`, `span.color`). `snapshot_content(term: &SharedTerm, focused: bool) -> GridSnapshot` signature matches its Task 1 definition and Task 4 call site.
