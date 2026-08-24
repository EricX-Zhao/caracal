//! `TerminalPanel`: adapter that embeds a `terminal::TerminalView` in the
//! workspace center. It does three things only (CLAUDE.md §1): embed the inner
//! entity, show a scrollbar + copy/paste menu, and **delegate focus to the
//! inner view**. No other business logic.

use std::cell::Cell;
use std::rc::Rc;

use alacritty_terminal::grid::{Dimensions, Scroll};
use gpui::{
    App, Context, Entity, FocusHandle, Focusable, IntoElement, ParentElement, Pixels, Point, Render,
    Size, Styled, WeakEntity, Window, div, point, px, size,
};
use gpui_component::menu::{ContextMenuExt, PopupMenuItem};
use gpui_component::scroll::{Scrollbar, ScrollbarHandle, ScrollbarShow};

use crate::terminal::model::SharedTerm;
use crate::terminal::scrollback;
use crate::terminal::view::TerminalView;
use crate::workspace::Workspace;

/// Adapts the terminal's alacritty grid (line-based scrollback) to
/// gpui-component's pixel-based `ScrollbarHandle` contract. Lives here, not
/// under `terminal/`, because `gpui_component` may only be imported from
/// `panels/` (see the design spec) — `terminal/view.rs` only exposes the
/// plain-`gpui` accessors (`shared_term`, `last_cell_height`) this needs.
#[derive(Clone)]
struct TerminalScrollbarHandle {
    term: SharedTerm,
    /// Cached each render from `TerminalView::last_cell_height()`.
    /// `ScrollbarHandle` methods take `&self` with no `cx`, so they can't
    /// read the live entity value directly — this is refreshed by
    /// `TerminalPanel::render` before the `Scrollbar` element is built.
    cell_h: Rc<Cell<f32>>,
}

impl TerminalScrollbarHandle {
    fn new(term: SharedTerm) -> Self {
        Self {
            term,
            cell_h: Rc::new(Cell::new(0.0)),
        }
    }
}

impl ScrollbarHandle for TerminalScrollbarHandle {
    /// gpui's scrollbar sign convention: `0` at the top of content,
    /// increasingly negative toward the bottom.
    fn offset(&self) -> Point<Pixels> {
        let cell_h = self.cell_h.get();
        if cell_h <= 0.0 {
            return point(px(0.0), px(0.0));
        }
        let term = self.term.lock();
        let hidden_above = term
            .total_lines()
            .saturating_sub(term.screen_lines())
            .saturating_sub(term.grid().display_offset());
        point(px(0.0), px(-(hidden_above as f32 * cell_h)))
    }

    fn set_offset(&self, offset: Point<Pixels>) {
        let cell_h = self.cell_h.get();
        if cell_h <= 0.0 {
            return;
        }
        let mut term = self.term.lock();
        let total = term.total_lines();
        let screen = term.screen_lines();
        let current_display_offset = term.grid().display_offset();
        let hidden_above = ((-f32::from(offset.y)) / cell_h).round().max(0.0) as usize;
        let target_display_offset = total.saturating_sub(screen).saturating_sub(hidden_above);
        let delta = target_display_offset as i32 - current_display_offset as i32;
        if delta != 0 {
            scrollback::apply(&mut term, Scroll::Delta(delta));
        }
    }

    fn content_size(&self) -> Size<Pixels> {
        let cell_h = self.cell_h.get();
        if cell_h <= 0.0 {
            return size(px(0.0), px(0.0));
        }
        let term = self.term.lock();
        // Width is unused: gpui-component's Scrollbar only reads `.height`/
        // `offset().y` for a vertical-only axis (scrollbar.rs:541-559).
        size(px(0.0), px(term.total_lines() as f32 * cell_h))
    }
}

pub struct TerminalPanel {
    terminal: Entity<TerminalView>,
    /// The 1-indexed sequence number rendered as this tab's `"N-"` title
    /// prefix. Kept in sync by `Workspace::renumber_tabs` (via
    /// `set_tab_number` below) every time the open-tab set changes — not
    /// a value this panel manages itself.
    tab_number: u32,
    /// Back-reference so `close` can ask `Workspace` to drop this tab.
    workspace: WeakEntity<Workspace>,
    /// Lazily built on first render (`TerminalPanel::new` takes no `cx`, and
    /// the handle needs `self.terminal.read(cx)` to get the shared `Term`).
    scrollbar_handle: Option<TerminalScrollbarHandle>,
}

impl TerminalPanel {
    pub fn new(terminal: Entity<TerminalView>, tab_number: u32, workspace: WeakEntity<Workspace>) -> Self {
        Self {
            terminal,
            tab_number,
            workspace,
            scrollbar_handle: None,
        }
    }

    /// Overwrite this tab's displayed `"N-"` prefix and repaint. Called by
    /// `Workspace::renumber_tabs` whenever the open-tab set changes.
    pub(crate) fn set_tab_number(&mut self, n: u32, cx: &mut Context<Self>) {
        self.tab_number = n;
        cx.notify();
    }

    pub(crate) fn tab_number(&self) -> u32 {
        self.tab_number
    }

    pub(crate) fn terminal(&self) -> Entity<TerminalView> {
        self.terminal.clone()
    }

    #[allow(dead_code)] // nested-update path if a panel-local listener closes itself
    fn close(&self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(workspace) = self.workspace.upgrade() else {
            return;
        };
        let panel = cx.entity();
        // Deferred: this runs inside a `cx.listener` on *this* panel, which
        // GPUI already wraps in an `Entity<TerminalPanel>::update(..)` call.
        // Calling `workspace.close_tab` synchronously would re-enter that
        // same update if Workspace then notifies this panel — GPUI panics
        // on nested self-updates. Deferring runs it after the current
        // update finishes.
        window.defer(cx, move |window, cx| {
            workspace.update(cx, |workspace, cx| {
                workspace.close_tab(panel, window, cx);
            });
        });
    }
}

impl Render for TerminalPanel {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if self.scrollbar_handle.is_none() {
            let term = self.terminal.read(cx).shared_term();
            self.scrollbar_handle = Some(TerminalScrollbarHandle::new(term));
        }
        let handle = self.scrollbar_handle.as_ref().expect("initialized above");
        handle.cell_h.set(self.terminal.read(cx).last_cell_height());

        let has_selection = self.terminal.read(cx).has_selection();
        let weak_terminal = self.terminal.downgrade();

        // Embed the inner terminal entity (it renders/handles input itself),
        // plus a right-side scrollbar overlay driven by `handle`. The
        // right-click context menu lives here rather than in
        // `terminal::view` (CLAUDE.md §1 boundary: that file stays
        // `gpui_component`-free) — copy/paste themselves are delegated back
        // to the inner view's `pub(crate)` methods so this adapter doesn't
        // duplicate clipboard/selection logic.
        div()
            .relative()
            .size_full()
            .child(
                div()
                    .size_full()
                    .child(self.terminal.clone())
                    .context_menu(move |menu, _window, _cx| {
                        // `context_menu`'s builder is an `Fn` (invoked on every
                        // render of the menu), so it must not move its captures —
                        // clone into locals per invocation, same reasoning as
                        // `SftpPanel`'s transfer-row context menu.
                        let weak_copy = weak_terminal.clone();
                        let weak_paste = weak_terminal.clone();
                        menu.item(
                            PopupMenuItem::new(rust_i18n::t!("Terminal.copy"))
                                .disabled(!has_selection)
                                .on_click(move |_ev, _window, cx| {
                                    let _ = weak_copy.update(cx, |view, cx| {
                                        view.copy_selection_to_clipboard(cx);
                                    });
                                }),
                        )
                        .item(PopupMenuItem::new(rust_i18n::t!("Terminal.paste")).on_click(
                            move |_ev, _window, cx| {
                                let _ = weak_paste.update(cx, |view, cx| {
                                    view.paste_from_clipboard(cx);
                                });
                            },
                        ))
                    }),
            )
            .child(
                div().absolute().inset_0().child(
                    Scrollbar::vertical(handle)
                        .id("terminal-scrollbar")
                        .scrollbar_show(ScrollbarShow::Scrolling),
                ),
            )
    }
}

impl Focusable for TerminalPanel {
    /// Delegate to the inner view's handle — returning our own would swallow all
    /// keyboard input (CLAUDE.md §2: focus 委托).
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.terminal.read(cx).focus_handle(cx)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alacritty_terminal::vte::ansi::Processor;
    use crate::terminal::model::new_term;

    /// Builds a term with a known scrollback depth: `rows`-row screen,
    /// `scrollback_lines`-line history cap, `total_output_lines` lines of
    /// output fed through the VTE parser (enough to overflow the cap so the
    /// resulting `total_lines()` is deterministic).
    fn term_with_history(rows: usize, scrollback_lines: usize, total_output_lines: usize) -> SharedTerm {
        let (tx, _rx) = flume::unbounded();
        let term = new_term(10, rows, scrollback_lines, tx);
        {
            let mut t = term.lock();
            let mut parser: Processor = Processor::new();
            let bytes: Vec<u8> = (0..total_output_lines)
                .flat_map(|i| format!("line {i}\r\n").into_bytes())
                .collect();
            parser.advance(&mut *t, &bytes);
        }
        term
    }

    #[test]
    fn offset_and_content_size_reflect_scrollback_depth() {
        // 30 lines into a 3-row screen with a 20-line history cap: 27 lines
        // scroll into history, capped at 20 -> total_lines == 23.
        let term = term_with_history(3, 20, 30);
        let handle = TerminalScrollbarHandle::new(term.clone());
        handle.cell_h.set(10.0);

        assert_eq!(term.lock().total_lines(), 23);
        assert_eq!(term.lock().grid().display_offset(), 0);

        assert_eq!(handle.content_size(), size(px(0.0), px(230.0)));
        // hidden_above = 23 total - 3 screen - 0 display_offset = 20 lines
        // hidden above the current (live, bottom) viewport.
        assert_eq!(handle.offset(), point(px(0.0), px(-200.0)));
    }

    #[test]
    fn set_offset_to_top_scrolls_all_the_way_back() {
        let term = term_with_history(3, 20, 30);
        let handle = TerminalScrollbarHandle::new(term.clone());
        handle.cell_h.set(10.0);

        handle.set_offset(point(px(0.0), px(0.0)));
        assert_eq!(term.lock().grid().display_offset(), 20);
    }

    #[test]
    fn set_offset_to_bottom_returns_to_live_area() {
        let term = term_with_history(3, 20, 30);
        let handle = TerminalScrollbarHandle::new(term.clone());
        handle.cell_h.set(10.0);

        handle.set_offset(point(px(0.0), px(0.0)));
        assert_eq!(term.lock().grid().display_offset(), 20);

        handle.set_offset(point(px(0.0), px(-200.0)));
        assert_eq!(term.lock().grid().display_offset(), 0);
    }

    #[test]
    fn zero_cell_height_is_a_safe_no_op() {
        let term = term_with_history(3, 20, 30);
        let handle = TerminalScrollbarHandle::new(term.clone());
        // cell_h left at its default 0.0 (pre-first-paint state).

        assert_eq!(handle.offset(), point(px(0.0), px(0.0)));
        assert_eq!(handle.content_size(), size(px(0.0), px(0.0)));

        handle.set_offset(point(px(0.0), px(-999.0)));
        assert_eq!(term.lock().grid().display_offset(), 0);
    }
}
