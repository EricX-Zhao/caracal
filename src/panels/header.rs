//! Top header bar — an in-app title bar (~40px) with a small brand mark, a
//! menu bar (File / View / Terminal / Help) built from gpui-component dropdown
//! menus, and a centered active-tab title. The OS window decorations are kept
//! for now; a frameless window + custom min/max/close controls (gpui-component
//! `TitleBar`) are a follow-up phase.
//!
//! Menu actions call back into the [`Workspace`] via a `WeakEntity`, so this
//! module stays a thin presentational adapter (CLAUDE.md §1).

use gpui::{
    App, IntoElement, ParentElement, SharedString, Styled, WeakEntity, div, px,
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
