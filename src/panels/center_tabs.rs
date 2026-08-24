//! Center terminal tab strip: the list math and the GPUI strip.
//! `Workspace` is the only strong owner of `TerminalPanel`s; this module
//! does not hold entities.

use gpui::{
    Div, InteractiveElement, ParentElement, Stateful, Styled, div, prelude::FluentBuilder, px,
};
use gpui_component::{ActiveTheme, StyledExt};

/// After closing the tab at `closed` in a list of `len` tabs whose current
/// active index is `active`, the new active index — or `None` if the list
/// is now empty or the close is invalid.
pub fn active_index_after_close(closed: usize, active: usize, len: usize) -> Option<usize> {
    if len == 0 || closed >= len {
        return None;
    }
    let new_len = len - 1;
    if new_len == 0 {
        return None;
    }
    let mut a = active.min(len - 1);
    if closed < a {
        a -= 1;
    } else if closed == a && a >= new_len {
        a = new_len - 1;
    }
    Some(a)
}

/// Permutation of `0..len` after moving the item at `from` to index `to`
/// (the index in the list *after* removal). Identity if either index is
/// out of range or `from == to`.
pub fn reorder_indices(from: usize, to: usize, len: usize) -> Vec<usize> {
    let mut v: Vec<usize> = (0..len).collect();
    if from >= len || to >= len || from == to {
        return v;
    }
    let item = v.remove(from);
    let insert_at = to.min(v.len());
    v.insert(insert_at, item);
    v
}

#[derive(Clone)]
pub struct DragTab {
    pub ix: usize,
}

impl gpui::Render for DragTab {
    fn render(
        &mut self,
        _window: &mut gpui::Window,
        cx: &mut gpui::Context<Self>,
    ) -> impl gpui::IntoElement {
        gpui::div()
            .px_3()
            .py_1()
            .bg(cx.theme().tab_active)
            .text_color(cx.theme().tab_active_foreground)
            .text_sm()
            .font_semibold()
            .border_r_1()
            .border_color(cx.theme().border)
            .child(format!("{}", self.ix + 1))
    }
}

pub fn tab_label(tab_number: u32, title: &str) -> String {
    format!("{tab_number}-{title}")
}

/// Horizontal chip strip. `on_select` / `on_close` / drop handling are
/// wired by the caller via the returned element's listeners — this helper
/// only builds one chip so Workspace can attach `cx.listener`s.
pub fn render_tab_chip(
    ix: usize,
    label: String,
    is_active: bool,
    cx: &mut gpui::Context<crate::workspace::Workspace>,
) -> Stateful<Div> {
    let fg = if is_active {
        cx.theme().tab_active_foreground
    } else {
        cx.theme().muted_foreground
    };
    div()
        .id(("center-tab", ix))
        .relative()
        .flex()
        .flex_row()
        .items_center()
        .gap_1()
        .px_3()
        .py_1()
        .text_sm()
        .text_color(fg)
        .border_r_1()
        .border_color(cx.theme().border)
        .when(is_active, |d| {
            d.bg(cx.theme().tab_active)
                .font_semibold()
                .child(
                    div()
                        .absolute()
                        .left_0()
                        .right_0()
                        .bottom_0()
                        .h(px(2.0))
                        .bg(cx.theme().primary),
                )
        })
        .when(!is_active, |d| {
            d.hover(|s| s.bg(cx.theme().list_hover).text_color(cx.theme().foreground))
        })
        .child(div().child(label))
}

pub fn render_empty_center(cx: &gpui::App) -> Div {
    div()
        .size_full()
        .flex()
        .items_center()
        .justify_center()
        .text_color(cx.theme().muted_foreground)
        .child(rust_i18n::t!("Terminal.empty_tabs").to_string())
}

#[cfg(test)]
mod tests {
    use super::{active_index_after_close, reorder_indices, tab_label};

    #[test]
    fn close_left_of_active_decrements() {
        assert_eq!(active_index_after_close(0, 2, 3), Some(1));
    }

    #[test]
    fn close_active_middle_keeps_slot() {
        // tabs [0,1,2], active 1, close 1 → new list [0,2], active stays index 1
        assert_eq!(active_index_after_close(1, 1, 3), Some(1));
    }

    #[test]
    fn close_last_while_active_lands_on_new_last() {
        assert_eq!(active_index_after_close(2, 2, 3), Some(1));
    }

    #[test]
    fn close_right_of_active_leaves_active() {
        assert_eq!(active_index_after_close(2, 0, 3), Some(0));
    }

    #[test]
    fn close_only_tab_yields_none() {
        assert_eq!(active_index_after_close(0, 0, 1), None);
    }

    #[test]
    fn close_out_of_range_or_empty_yields_none() {
        assert_eq!(active_index_after_close(0, 0, 0), None);
        assert_eq!(active_index_after_close(5, 0, 2), None);
    }

    #[test]
    fn reorder_move_right() {
        assert_eq!(reorder_indices(0, 2, 3), vec![1, 2, 0]);
    }

    #[test]
    fn reorder_move_left() {
        assert_eq!(reorder_indices(2, 0, 3), vec![2, 0, 1]);
    }

    #[test]
    fn reorder_no_op_same_index() {
        assert_eq!(reorder_indices(1, 1, 3), vec![0, 1, 2]);
    }

    #[test]
    fn reorder_out_of_range_is_identity() {
        assert_eq!(reorder_indices(9, 0, 3), vec![0, 1, 2]);
        assert_eq!(reorder_indices(0, 9, 3), vec![0, 1, 2]);
    }

    #[test]
    fn tab_label_prefixes_live_number() {
        assert_eq!(tab_label(1, "本地终端"), "1-本地终端");
        assert_eq!(tab_label(3, "prod:2"), "3-prod:2");
    }
}
