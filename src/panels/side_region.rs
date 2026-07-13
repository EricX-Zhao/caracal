//! Side region — the panel container that sits between an activity bar and the
//! center. Each side shows exactly one panel (the active activity-bar item).
//!
//! The panel view is given *definite pixel bounds* — a `relative flex_1`
//! container whose child is a plain `absolute size_full` div wrapping the
//! view — which virtualized children like the SFTP `DataTable` need in order
//! to render rows (without it the list stays blank). This used to be done via
//! `AnyView::cached(...)`, which bundles that same style onto the view's own
//! layout node — but caching also means the view's *paint* (not just layout)
//! gets replayed from a previous frame whenever `Context::notify` wasn't
//! called on that exact entity, independent of `cx.refresh_windows()`
//! (confirmed: switching the app theme left this region showing whatever
//! theme was active last time this exact entity repainted, e.g. a light-mode
//! header bar surviving into an otherwise-dark frame). A plain wrapping div
//! gives the same bounds without opting into that caching, so the view's
//! `render()` — and therefore `cx.theme()` — is always read fresh.
//!
//! The region's *width* is controlled by the enclosing `h_resizable` group in
//! `workspace.rs`; this function only produces the region's content.

use gpui::{AnyElement, AnyView, Hsla, IntoElement, ParentElement, Styled, div, prelude::FluentBuilder, px};

/// Build a side region's content for `view`.
///
/// - `border` — theme border color for the inner edge.
/// - `left_side` — border on the right edge when true, left edge when false.
pub fn side_region_content(view: AnyView, border: Hsla, left_side: bool) -> AnyElement {
    div()
        .flex()
        .flex_col()
        .size_full()
        .min_h(px(0.0))
        .when(left_side, |d| d.border_r_1().border_color(border))
        .when(!left_side, |d| d.border_l_1().border_color(border))
        .child(
            div()
                .relative()
                .flex_1()
                .min_h(px(0.0))
                .child(div().absolute().size_full().child(view)),
        )
        .into_any_element()
}
