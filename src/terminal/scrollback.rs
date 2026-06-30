//! Scrollback / display-offset handling.
//!
//! The model is the `display_offset` field on alacritty's `Term::grid` — we
//! never keep a parallel copy in this file. The wrapper helpers here just
//! translate user intent (wheel ticks, PageUp/PageDown) into the right
//! [`alacritty_terminal::grid::Scroll`] variant.
//!
//! Boundaries: see CLAUDE.md §2. The `display_offset` *is* the source of
//! truth; render.rs reads it on every paint and view.rs reads it to decide
//! whether the user has scrolled away from the live area (which suppresses
//! the auto-snap-to-bottom).

use alacritty_terminal::grid::Scroll;

use crate::terminal::model::EventProxy;

/// Apply a scroll intent to the term.
///
/// `apply` is concrete on the `EventProxy` listener because that's the
/// only listener Caracal instantiates. It does *not* fire any session
/// events — scrolling is a read on the grid, so the listener is unused
/// here.
pub fn apply(term: &mut alacritty_terminal::Term<EventProxy>, scroll: Scroll) {
    term.scroll_display(scroll);
}

/// Translate a `gpui::ScrollDelta::Pixels(Point<Pixels>)` Y component into a
/// `Scroll::Delta(n)` of lines.
///
/// Sign matches Zed's terminal: a positive `delta.y` scrolls *up* into history,
/// and `Scroll::Delta(+n)` increases `display_offset` (older content). So the
/// mapping is direct — no inversion. `cell_h` is the measured line height in px.
pub fn delta_from_pixels(delta_y_px: f32, cell_h: f32) -> Scroll {
    let h = if cell_h > 0.0 { cell_h } else { 16.0 };
    Scroll::Delta((delta_y_px / h).round() as i32)
}

/// Translate a `gpui::ScrollDelta::Lines(Point<f32>)` Y component into a
/// `Scroll::Delta(n)`. Classic mouse wheels report whole lines per notch; we
/// scroll 3 lines per notch (common terminal default). Positive = up.
pub fn delta_from_lines(delta_y_lines: f32) -> Scroll {
    Scroll::Delta((delta_y_lines * 3.0).round() as i32)
}
