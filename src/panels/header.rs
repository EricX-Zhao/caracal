//! Top header bar — an in-app title bar (~40px) with a small brand mark, a
//! centered active-tab title (also the window's draggable region), and — on
//! Linux/Windows — a minimize/maximize/close button cluster (macOS keeps its
//! native traffic-light buttons instead; see `main.rs`'s `TitlebarOptions`).
//! The window itself requests client-side decorations in `main.rs`, which is
//! what makes this row the *only* title bar the user sees.
//!
//! No menu bar: File/Terminal's only real content was Settings and "新建本地
//! 终端" — Settings now has its own icon button at the bottom of the left
//! activity bar (`activity_bar.rs`'s `settings_button`), and "新建本地终端"
//! has no dedicated entry point anymore (open a local session from the
//! 会话 panel instead).

use gpui::{
    App, InteractiveElement, IntoElement, MouseButton, MouseDownEvent, ParentElement,
    SharedString, StatefulInteractiveElement, Styled, Window, WindowButton, WindowButtonLayout,
    div, prelude::FluentBuilder, px,
};
use gpui_component::ActiveTheme;

use crate::panels::icons::{AppIcon, icon};

/// Icon for the maximize/restore button: swaps to the "restore" glyph once
/// the window is already maximized, so the button always reads as "the
/// thing this click will do next".
fn maximize_icon(is_maximized: bool) -> AppIcon {
    if is_maximized {
        AppIcon::WindowRestore
    } else {
        AppIcon::WindowMaximize
    }
}

/// Fallback button layout for platforms/desktops that don't report one via
/// `cx.button_layout()` (Windows always; a Linux DE without
/// `gtk-decoration-layout`): minimize/maximize/close, right-aligned. Doesn't
/// reuse `WindowButtonLayout::linux_default()` because that constructor is
/// `#[cfg(any(target_os = "linux", target_os = "freebsd"))]`-gated in gpui —
/// this needs to compile on Windows too.
fn or_default_layout(layout: Option<WindowButtonLayout>) -> WindowButtonLayout {
    layout.unwrap_or(WindowButtonLayout {
        left: [None, None, None],
        right: [
            Some(WindowButton::Minimize),
            Some(WindowButton::Maximize),
            Some(WindowButton::Close),
        ],
    })
}

/// Mouse-down handler for the title bar's draggable region (the centered
/// title, and the empty space around it): a single click starts an
/// interactive window move; a double-click toggles maximize instead.
/// `titlebar_double_click` only does anything on macOS — it respects
/// whatever the user's actual System Settings double-click action is there
/// (confirmed: only `gpui_macos` overrides the shared no-op default) — so
/// Linux/Windows get an explicit `zoom_window()` call instead.
fn drag_or_maximize(ev: &MouseDownEvent, window: &mut Window, _cx: &mut App) {
    if ev.click_count == 2 {
        if cfg!(target_os = "macos") {
            window.titlebar_double_click();
        } else {
            window.zoom_window();
        }
    } else {
        window.start_window_move();
    }
}

/// One minimize/maximize/close button. macOS never renders this (native
/// traffic-light buttons replace it — see `render_header`'s `is_macos`
/// branch), so this only needs to look right for Linux/Windows conventions.
fn window_control_button(button: WindowButton, is_maximized: bool, cx: &App) -> impl IntoElement {
    let (icon_name, is_close) = match button {
        WindowButton::Minimize => (AppIcon::WindowMinimize, false),
        WindowButton::Maximize => (maximize_icon(is_maximized), false),
        WindowButton::Close => (AppIcon::WindowClose, true),
    };

    div()
        .id(button.id())
        .flex()
        .items_center()
        .justify_center()
        .w(px(46.0))
        .h_full()
        .text_color(cx.theme().muted_foreground)
        .when(is_close, |d| {
            d.hover(|s| s.bg(cx.theme().danger).text_color(cx.theme().danger_foreground))
        })
        .when(!is_close, |d| {
            d.hover(|s| s.bg(cx.theme().accent).text_color(cx.theme().foreground))
        })
        .child(icon(icon_name))
        .on_click(move |_ev, window, _cx| match button {
            WindowButton::Minimize => window.minimize_window(),
            WindowButton::Maximize => window.zoom_window(),
            WindowButton::Close => window.remove_window(),
        })
}

/// Renders one side (`layout.left` or `layout.right`) of the button layout —
/// most desktops only populate one side, but this handles both since
/// `cx.button_layout()` can report either.
fn window_control_side(
    slots: [Option<WindowButton>; gpui::MAX_BUTTONS_PER_SIDE],
    is_maximized: bool,
    cx: &App,
) -> impl IntoElement {
    div()
        .flex()
        .flex_row()
        .items_center()
        .h_full()
        .children(
            slots
                .into_iter()
                .flatten()
                .map(|button| window_control_button(button, is_maximized, cx)),
        )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maximize_icon_switches_on_maximized_state() {
        assert_eq!(maximize_icon(false), AppIcon::WindowMaximize);
        assert_eq!(maximize_icon(true), AppIcon::WindowRestore);
    }

    #[test]
    fn or_default_layout_passes_through_a_reported_layout() {
        let reported = WindowButtonLayout {
            left: [Some(WindowButton::Close), None, None],
            right: [Some(WindowButton::Minimize), Some(WindowButton::Maximize), None],
        };
        assert_eq!(or_default_layout(Some(reported)), reported);
    }

    #[test]
    fn or_default_layout_falls_back_to_right_aligned_min_max_close() {
        let layout = or_default_layout(None);
        assert_eq!(layout.left, [None, None, None]);
        assert_eq!(
            layout.right,
            [
                Some(WindowButton::Minimize),
                Some(WindowButton::Maximize),
                Some(WindowButton::Close),
            ]
        );
    }
}

/// Build the header row. `active_title` is the focused terminal's title (or
/// "Caracal" when nothing is focused).
pub fn render_header(
    active_title: SharedString,
    window: &Window,
    cx: &App,
) -> impl IntoElement + use<> {
    // macOS keeps its native traffic-light buttons (positioned via
    // `TitlebarOptions.traffic_light_position` in `main.rs`) instead of a
    // hand-drawn cluster — that's the platform convention, and gpui_macos
    // already implements the OS-level plumbing for it.
    let is_macos = cfg!(target_os = "macos");
    let window_controls = if is_macos {
        None
    } else {
        let layout = or_default_layout(cx.button_layout());
        let is_maximized = window.is_maximized();
        Some((
            window_control_side(layout.left, is_maximized, cx).into_any_element(),
            window_control_side(layout.right, is_maximized, cx).into_any_element(),
        ))
    };
    let (left_controls, right_controls) = match window_controls {
        Some((l, r)) => (Some(l), Some(r)),
        None => (None, None),
    };

    div()
        .h(px(40.0))
        .flex()
        .flex_row()
        .items_center()
        .gap_2()
        .px_2()
        .bg(cx.theme().muted)
        .border_b_1()
        .border_color(cx.theme().border)
        // Reserve space for macOS's native traffic-light buttons (rendered
        // outside this element tree entirely — see `main.rs`'s
        // `traffic_light_position`), so the brand icon doesn't sit under them.
        .when(is_macos, |d| d.pl(px(78.0)))
        .children(left_controls)
        // Brand mark (no menu — see the module doc comment)
        .child(
            div()
                .flex_shrink_0()
                .text_color(cx.theme().foreground)
                .child(icon(AppIcon::Terminal)),
        )
        // Centered active title — also the draggable region: this flex_1
        // area is mostly empty space around the centered text, so a
        // mouse-down anywhere in it reads as "drag the window" (or
        // "double-click to maximize"), matching Zed/VS Code convention. The
        // brand mark above (and the window-controls cluster added
        // separately) are deliberately NOT covered by this handler, so their
        // own clicks are unaffected.
        .child(
            div()
                .id("header-drag-region")
                .flex()
                .flex_1()
                .min_w(px(0.0))
                .items_center()
                .justify_center()
                .text_xs()
                .text_color(cx.theme().muted_foreground)
                .on_mouse_down(MouseButton::Left, drag_or_maximize)
                .child(active_title),
        )
        .children(right_controls)
}
