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
pub fn batch_text_runs(row: &[SnapCell]) -> Vec<TextSpan> {
    let mut spans = Vec::new();
    let mut current: Option<TextSpan> = None;
    let mut end_col: usize = 0;
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

        match &mut current {
            Some(span) if span.style.matches(&style) && end_col == col => {
                span.text.push(cell.c);
                // Only increment end_col for narrow cells; wide chars are always last in their span
                if !cell.wide {
                    end_col += 1;
                }
            }
            _ => {
                if let Some(span) = current.take() {
                    spans.push(span);
                }
                current = Some(TextSpan { start_col: col, text: cell.c.to_string(), style });
                end_col = col + 1;
            }
        }
        col += step;
    }

    if let Some(span) = current.take() {
        spans.push(span);
    }
    spans
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

pub struct BgSpan {
    pub start_col: usize,
    pub num_cells: usize,
    pub color: Hsla,
}

/// Group a row's non-default-background cells into merged rects.
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
