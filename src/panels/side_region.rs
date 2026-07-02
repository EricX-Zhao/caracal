//! Side region — the panel container that sits between an activity bar and the
//! center. Each side shows exactly one panel (the active activity-bar item).
//!
//! The panel view is embedded the same way `gpui_component`'s `DockArea`
//! `TabPanel` does it: a `relative flex_1` container whose child is the view
//! `.cached(StyleRefinement::default().absolute().size_full())`. This gives the
//! panel *definite pixel bounds*, which virtualized children like the SFTP
//! `DataTable` need in order to render rows (without it the list stays blank).
//!
//! The region's *width* is controlled by the enclosing `h_resizable` group in
//! `workspace.rs`; this function only produces the region's content.

use gpui::{
    AnyElement, AnyView, Hsla, IntoElement, ParentElement, StyleRefinement, Styled, div,
    prelude::FluentBuilder, px,
};

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
                .child(view.cached(StyleRefinement::default().absolute().size_full())),
        )
        .into_any_element()
}
