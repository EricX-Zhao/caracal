//! Activity bars — the VSCode-style vertical icon strips on the far left and far
//! right. Each bar is a single column of icon buttons; clicking one toggles
//! which panel occupies that side's single slot. This module owns the
//! *descriptor data* (which panels exist, on which side, with which icon/label)
//! and the *stateless button styling*. The strip assembly + click wiring lives
//! in `workspace.rs` (it needs `cx.listener`).
//!
//! Follow-up (not implemented here): drag-reorder of items across sides, plus
//! persisting the layout.

use gpui::{
    App, Div, InteractiveElement, ParentElement, SharedString, Stateful,
    StatefulInteractiveElement, Styled, div, prelude::FluentBuilder, px,
};
use gpui_component::tooltip::Tooltip;
use gpui_component::{ActiveTheme, Sizable};

use crate::panels::icons::{AppIcon, icon};

/// Which edge bar a panel belongs to.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Side {
    Left,
    Right,
}

/// Every panel that can be selected from an activity bar. `Sftp` and
/// `Sessions` are backed by real panels; the rest are placeholder
/// [`crate::panels::stub::StubPanel`]s for now.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub enum PanelId {
    // Left bar
    Sftp,
    Network,
    Security,
    // Right bar
    Sessions,
    History,
    Monitor,
}

impl PanelId {
    /// Stable string used for element ids.
    pub fn key(self) -> &'static str {
        match self {
            PanelId::Sftp => "sftp",
            PanelId::Network => "network",
            PanelId::Security => "security",
            PanelId::Sessions => "sessions",
            PanelId::History => "history",
            PanelId::Monitor => "monitor",
        }
    }

    pub fn side(self) -> Side {
        match self {
            PanelId::Sftp | PanelId::Network | PanelId::Security => Side::Left,
            PanelId::Sessions | PanelId::History | PanelId::Monitor => Side::Right,
        }
    }

    pub fn app_icon(self) -> AppIcon {
        match self {
            PanelId::Sftp => AppIcon::FileExplorer,
            PanelId::Network => AppIcon::Network,
            PanelId::Security => AppIcon::SecurityAuth,
            PanelId::Sessions => AppIcon::Sessions,
            PanelId::History => AppIcon::CommandHistory,
            PanelId::Monitor => AppIcon::ResourceMonitor,
        }
    }

    /// Short label used for tooltip + stub-panel title.
    pub fn label(self) -> &'static str {
        match self {
            PanelId::Sftp => "文件浏览器",
            PanelId::Network => "网络",
            PanelId::Security => "安全 / 认证",
            PanelId::Sessions => "会话",
            PanelId::History => "命令历史",
            PanelId::Monitor => "资源监控",
        }
    }
}

/// The ordered panels shown in one side's activity bar.
pub fn side_items(side: Side) -> &'static [PanelId] {
    match side {
        Side::Left => &[PanelId::Sftp, PanelId::Network, PanelId::Security],
        Side::Right => &[PanelId::Sessions, PanelId::History, PanelId::Monitor],
    }
}

/// A single activity-bar button (icon + active indicator + tooltip). Returns a
/// `Stateful<Div>` so the caller can chain `.on_click(cx.listener(..))`.
pub fn activity_button(pid: PanelId, active: bool, side: Side, cx: &App) -> Stateful<Div> {
    let text_color = if active {
        cx.theme().foreground
    } else {
        cx.theme().muted_foreground
    };
    let label = pid.label();

    div()
        .id(SharedString::from(format!("activity-{}", pid.key())))
        .relative()
        .flex()
        .items_center()
        .justify_center()
        .w_full()
        .h(px(40.0))
        .text_color(text_color)
        .when(active, |this| this.bg(cx.theme().list_active))
        .hover(|s| s.bg(cx.theme().accent).text_color(cx.theme().foreground))
        // Active indicator bar on the outer edge (matches nyaterm). The button
        // spans the full 44px strip so this sits clear of the centered icon.
        .when(active, |this| {
            this.child(
                div()
                    .absolute()
                    .top_1()
                    .bottom_1()
                    .w(px(2.0))
                    .rounded_full()
                    .bg(cx.theme().primary)
                    .when(matches!(side, Side::Left), |d| d.left_0())
                    .when(matches!(side, Side::Right), |d| d.right_0()),
            )
        })
        .child(icon(pid.app_icon()).large())
        .tooltip(move |window, cx| Tooltip::new(label).build(window, cx))
}

/// The quick-commands drawer toggle, pinned to the bottom of the right
/// activity bar. Same visual treatment as [`activity_button`], but not
/// backed by a `PanelId` — it toggles a bottom drawer
/// (`Workspace::show_quick_commands`), not a side-panel slot, so it doesn't
/// participate in `side_items`/`toggle_panel`.
pub fn quick_commands_button(active: bool, cx: &App) -> Stateful<Div> {
    let text_color = if active {
        cx.theme().foreground
    } else {
        cx.theme().muted_foreground
    };

    div()
        .id("activity-quick-commands")
        .relative()
        .flex()
        .items_center()
        .justify_center()
        .w_full()
        .h(px(40.0))
        .text_color(text_color)
        .when(active, |this| this.bg(cx.theme().list_active))
        .hover(|s| s.bg(cx.theme().accent).text_color(cx.theme().foreground))
        .when(active, |this| {
            this.child(
                div()
                    .absolute()
                    .top_1()
                    .bottom_1()
                    .w(px(2.0))
                    .rounded_full()
                    .bg(cx.theme().primary)
                    .right_0(),
            )
        })
        .child(icon(AppIcon::QuickCmd).large())
        .tooltip(move |window, cx| Tooltip::new("快捷命令").build(window, cx))
}

/// The Settings shortcut, pinned to the bottom of the left activity bar
/// (now the only way to reach Settings — the header's File menu that used
/// to hold it is gone, see `panels::header`). Opens the standalone Settings
/// window (`Workspace::open_settings`); not backed by a `PanelId` since it's
/// a separate window, not a side-panel slot.
pub fn settings_button(cx: &App) -> Stateful<Div> {
    div()
        .id("activity-settings")
        .flex()
        .items_center()
        .justify_center()
        .w_full()
        .h(px(40.0))
        .text_color(cx.theme().muted_foreground)
        .hover(|s| s.bg(cx.theme().accent).text_color(cx.theme().foreground))
        .child(icon(AppIcon::Settings).large())
        .tooltip(move |window, cx| Tooltip::new("设置").build(window, cx))
}
