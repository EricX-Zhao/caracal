//! `TerminalPanel`: the adapter that lets a `terminal::TerminalView` live inside
//! a `gpui_component` dock. It does three things only (CLAUDE.md §1): embed the
//! inner entity, show a title (with a close button), and **delegate focus to the
//! inner view**. No other business logic.

use std::cell::Cell;
use std::rc::Rc;
use std::sync::Arc;

use alacritty_terminal::grid::{Dimensions, Scroll};
use gpui::{
    App, ClickEvent, Context, Entity, FocusHandle, Focusable, InteractiveElement, IntoElement,
    ParentElement, Pixels, Point, Render, Size, StatefulInteractiveElement, Styled, WeakEntity,
    Window, div, point, px, size,
};
use gpui_component::dock::{Panel, PanelControl, PanelEvent, PanelView, TabPanel};
use gpui_component::menu::{ContextMenuExt, PopupMenuItem};
use gpui_component::scroll::{Scrollbar, ScrollbarHandle, ScrollbarShow};
use gpui_component::ActiveTheme;

use crate::panels::icons::{AppIcon, icon};
use crate::terminal::model::SharedTerm;
use crate::terminal::scrollback;
use crate::terminal::view::TerminalView;

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
    /// The `TabPanel` this panel currently lives in, handed to us via
    /// `on_added_to`. Needed so the close button (embedded in `title()`, since
    /// this gpui-component revision's tab strip has no built-in per-tab close
    /// icon) can remove *this specific* panel regardless of which tab is active.
    tab_panel: Option<WeakEntity<TabPanel>>,
    /// The 1-indexed sequence number rendered as this tab's `"N-"` title
    /// prefix. Kept in sync by `Workspace::renumber_tabs` (via
    /// `set_tab_number` below) every time the open-tab set changes — not
    /// a value this panel manages itself. Only goes stale after a manual
    /// drag-reorder, which `Workspace` has no way to observe (see
    /// docs/superpowers/specs/2026-07-22-tab-sequence-numbers-design.md).
    tab_number: u32,
    /// Whether this panel is currently attached to a `TabPanel`. Flipped to
    /// `false` by `on_removed`, back to `true` by `on_added_to` — the signal
    /// `on_removed` uses to tell a genuine close apart from gpui-component's
    /// internal detach-then-reinsert dance for a drag-reorder (both call
    /// `on_removed` identically; only a real close never calls `on_added_to`
    /// again afterward). See
    /// docs/superpowers/specs/2026-07-22-drag-reorder-false-close-design.md.
    attached: bool,
    /// Lazily built on first render (`TerminalPanel::new` takes no `cx`, and
    /// the handle needs `self.terminal.read(cx)` to get the shared `Term`).
    scrollbar_handle: Option<TerminalScrollbarHandle>,
}

impl TerminalPanel {
    pub fn new(terminal: Entity<TerminalView>, tab_number: u32) -> Self {
        Self {
            terminal,
            tab_panel: None,
            tab_number,
            attached: true,
            scrollbar_handle: None,
        }
    }

    /// Overwrite this tab's displayed `"N-"` prefix and repaint. Called by
    /// `Workspace::renumber_tabs` whenever the open-tab set changes.
    pub(crate) fn set_tab_number(&mut self, n: u32, cx: &mut Context<Self>) {
        self.tab_number = n;
        cx.notify();
    }

    fn close(&self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(tab_panel) = self.tab_panel.as_ref().and_then(|w| w.upgrade()) else {
            return;
        };
        let this: Arc<dyn PanelView> = Arc::new(cx.entity());
        // Deferred: this runs inside a `cx.listener` on *this* panel, which
        // GPUI already wraps in an `Entity<TerminalPanel>::update(..)` call.
        // Calling `remove_panel` synchronously would trigger our own
        // `on_removed` hook, re-entering that same update — GPUI panics on
        // nested self-updates ("cannot update X while it is already being
        // updated"). Deferring runs it after the current update finishes,
        // the same way gpui-component's own `TabPanel::on_action_close_panel`
        // defers its cleanup.
        window.defer(cx, move |window, cx| {
            tab_panel.update(cx, |tab_panel, cx| {
                tab_panel.remove_panel(this, window, cx);
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

impl gpui::EventEmitter<PanelEvent> for TerminalPanel {}

/// Emitted when the dock actually removes this panel
/// (`Panel::on_removed`, below) — a generic, backend-agnostic "this tab
/// is gone" signal. `TerminalPanel` doesn't know or care why (matches
/// its "adapter only" mandate, file header above); `Workspace` is the
/// one that knows what, if anything, needs cleaning up for a given
/// backend kind (see `open_ssh` in `workspace.rs`, the only current
/// subscriber — local/Telnet/Serial tabs emit this too, just unobserved,
/// since they share no session to clean up).
#[derive(Clone, Debug)]
pub enum TerminalPanelEvent {
    Closed,
}

impl gpui::EventEmitter<TerminalPanelEvent> for TerminalPanel {}

impl Panel for TerminalPanel {
    fn panel_name(&self) -> &'static str {
        "TerminalPanel"
    }

    /// Hide the "Expand / Zoom" icon in the tab strip. gpui-component's
    /// `TabPanel::render_toolbar` only renders that icon when `zoomable`
    /// returns `Some(PanelControl::Both | Toolbar)`; returning `None`
    /// makes the entire zoom branch fall through to `None` and no icon is
    /// emitted. The tab strip's "..." menu still offers Zoom In/Out as
    /// menu actions, so the feature isn't lost — just demoted to a less
    /// visible spot. We don't use TabPanel's tab-strip close button either
    /// (this gpui-component revision doesn't ship one), hence our own `X`
    /// in `title()`.
    fn zoomable(&self, _cx: &App) -> Option<PanelControl> {
        None
    }

    fn title(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let title = format!("{}-{}", self.tab_number, self.terminal.read(cx).title());
        div()
            .flex()
            .flex_row()
            .items_center()
            .gap_1()
            .child(title)
            .child(
                div()
                    .id(("close-terminal", cx.entity_id()))
                    .rounded_sm()
                    .text_color(cx.theme().foreground)
                    .hover(|s| s.bg(cx.theme().danger).text_color(cx.theme().danger_foreground))
                    .child(icon(AppIcon::Delete))
                    .on_click(cx.listener(|this, _ev: &ClickEvent, window, cx| {
                        // Don't also let the tab strip's own click handler fire
                        // (it would select this tab right before we remove it).
                        cx.stop_propagation();
                        this.close(window, cx);
                    })),
            )
    }

    fn on_added_to(
        &mut self,
        tab_panel: WeakEntity<TabPanel>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) {
        self.tab_panel = Some(tab_panel);
        self.attached = true;
    }

    /// gpui-component's `TabPanel::detach_panel` calls this identically for
    /// a genuine close AND for every drag-driven reposition (detach, then
    /// immediately reinsert elsewhere) — it has no way to tell us which.
    /// Deferring the actual "is this really gone" check by one update cycle
    /// and rechecking `attached` lets a drag's synchronous reinsert (which
    /// flips `attached` back to `true` via `on_added_to`, before this
    /// deferred closure runs) self-heal away a false close. See
    /// docs/superpowers/specs/2026-07-22-drag-reorder-false-close-design.md.
    fn on_removed(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.attached = false;
        let weak = cx.entity().downgrade();
        window.defer(cx, move |_window, cx| {
            let _ = weak.update(cx, |this, cx| {
                if !this.attached {
                    cx.emit(TerminalPanelEvent::Closed);
                }
            });
        });
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
