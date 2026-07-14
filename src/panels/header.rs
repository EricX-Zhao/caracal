//! Top header bar — an in-app title bar (~40px) with a small brand mark, a
//! menu bar (File / View / Terminal / Help) built from gpui-component dropdown
//! menus, and a centered active-tab title. The OS window decorations are kept
//! for now; a frameless window + custom min/max/close controls (gpui-component
//! `TitleBar`) are a follow-up phase.
//!
//! Menu actions call back into the [`Workspace`] via a `WeakEntity`, so this
//! module stays a thin presentational adapter (CLAUDE.md §1).

use gpui::{
    App, InteractiveElement, IntoElement, MouseButton, MouseDownEvent, ParentElement,
    SharedString, Styled, WeakEntity, Window, WindowButton, WindowButtonLayout, div, px,
};
use gpui_component::button::{Button, ButtonVariants};
use gpui_component::menu::{DropdownMenu, PopupMenuItem};
use gpui_component::{ActiveTheme, Sizable};

use crate::panels::icons::{AppIcon, icon};
use crate::workspace::Workspace;

/// The "新建本地终端" menu item — defined once and reused by the File and
/// Terminal menus so the label + action can't drift apart.
fn new_local_item(ws: WeakEntity<Workspace>) -> PopupMenuItem {
    PopupMenuItem::new("新建本地终端").on_click(move |_ev, window, cx| {
        let _ = ws.update(cx, |w, cx| w.open_local(window, cx));
    })
}

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
    workspace: WeakEntity<Workspace>,
    active_title: SharedString,
    cx: &App,
) -> impl IntoElement + use<> {
    let ws_file = workspace.clone();
    let ws_terminal = workspace.clone();
    let ws_settings = workspace.clone();

    let file_menu = Button::new("menu-file")
        .ghost()
        .xsmall()
        .label("文件")
        .dropdown_menu(move |menu, _window, _cx| {
            menu.item(new_local_item(ws_file.clone())).item(
                PopupMenuItem::new("设置...").on_click({
                    let ws_settings = ws_settings.clone();
                    move |_ev, window, cx| {
                        let _ = ws_settings.update(cx, |w, cx| w.open_settings(window, cx));
                    }
                }),
            )
        });

    let view_menu = Button::new("menu-view")
        .ghost()
        .xsmall()
        .label("视图")
        .dropdown_menu(move |menu, _window, _cx| {
            menu.item(
                // Shares `crate::toggle_theme` with Ctrl+K (bound in
                // `main.rs`) so both paths persist to `settings.toml` and
                // always agree with what Settings → Appearance shows.
                PopupMenuItem::new("切换主题")
                    .on_click(move |_ev, _window, cx| crate::toggle_theme(cx)),
            )
        });

    let terminal_menu = Button::new("menu-terminal")
        .ghost()
        .xsmall()
        .label("终端")
        .dropdown_menu(move |menu, _window, _cx| menu.item(new_local_item(ws_terminal.clone())));

    let help_menu = Button::new("menu-help")
        .ghost()
        .xsmall()
        .label("帮助")
        .dropdown_menu(move |menu, _window, _cx| {
            menu.item(PopupMenuItem::link(
                "项目主页",
                "https://github.com/Kilo-Org/kilocode",
            ))
        });

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
        // Brand + menu bar
        .child(
            div()
                .flex()
                .flex_row()
                .items_center()
                .gap_1()
                .flex_shrink_0()
                .child(
                    div()
                        .text_color(cx.theme().foreground)
                        .child(icon(AppIcon::Terminal)),
                )
                .child(file_menu)
                .child(view_menu)
                .child(terminal_menu)
                .child(help_menu),
        )
        // Centered active title
        .child(
            div()
                .flex()
                .flex_1()
                .min_w(px(0.0))
                .items_center()
                .justify_center()
                .text_xs()
                .text_color(cx.theme().muted_foreground)
                .child(active_title),
        )
}
