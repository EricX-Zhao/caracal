//! `SettingsWindow`: a standalone second window (File → "设置...") for
//! application-level settings. Follows nyaterm's draft + Apply/Confirm/Cancel
//! model: [`SettingsWindow`] clones the committed [`crate::settings::AppSettings`]
//! into a local draft on open; nothing is written to `settings.toml` or applied
//! live until Apply or Confirm.

/// Parse the Terminal tab's font-size text field. Rejects non-finite,
/// non-positive, and unreasonably large values (a typo like "1400" should not
/// silently make every tab's text 100x too big).
fn parse_font_size(text: &str) -> Option<f32> {
    let value: f32 = text.trim().parse().ok()?;
    if value.is_finite() && value >= 6.0 && value <= 96.0 {
        Some(value)
    } else {
        None
    }
}

/// Parse the Terminal tab's monitor-poll-interval field. Rejects
/// non-positive and unreasonably small/large values (a 0-second interval
/// would spin the poll loop; a typo like "99999" would poll once every 27
/// hours, effectively never).
fn parse_monitor_interval(text: &str) -> Option<u32> {
    let value: u32 = text.trim().parse().ok()?;
    if (1..=3600).contains(&value) {
        Some(value)
    } else {
        None
    }
}

/// Parse the Terminal tab's scrollback-lines field. Rejects non-integer and
/// out-of-range values — 1,000 keeps a minimally useful history, 50,000 caps
/// memory use for a single tab's grid.
fn parse_scrollback_lines(text: &str) -> Option<u32> {
    let value: u32 = text.trim().parse().ok()?;
    if (1_000..=50_000).contains(&value) {
        Some(value)
    } else {
        None
    }
}

use gpui::{
    App, AppContext, ClickEvent, Context, Entity, InteractiveElement, IntoElement, ParentElement,
    Render, SharedString, StatefulInteractiveElement, Styled, WeakEntity, Window,
    div, prelude::FluentBuilder, px, red, transparent_black,
};
use gpui_component::button::{Button, DropdownButton};
use gpui_component::input::{Input, InputState};
use gpui_component::menu::PopupMenuItem;
use gpui_component::switch::Switch;
use gpui_component::{ActiveTheme, Sizable, Theme, ThemeMode, ThemeRegistry};

use crate::settings::{self, AppSettings};
use crate::terminal::view::{CJK_FALLBACK, DEFAULT_FONT_FAMILY, SYMBOL_FALLBACK};
use crate::workspace::Workspace;

/// The 4 choices every font-family dropdown offers. `""` means "detect a
/// system font at apply/seed time" — see `TerminalView::set_font_family`/
/// `set_font_fallbacks` and `Workspace::apply_appearance_font_settings`
/// (neither dropdown resolves it here; the picker only ever stores one of
/// these 4 raw values).
const FONT_CHOICES: &[(&str, &str)] = &[
    ("", "系统默认"),
    (DEFAULT_FONT_FAMILY, "JetBrains Mono"),
    (CJK_FALLBACK, "Sarasa Mono SC"),
    (SYMBOL_FALLBACK, "Symbols Nerd Font"),
];

/// Identifies which font-setting field a `SettingsWindow::font_picker`
/// dropdown reads/writes.
#[derive(Clone, Copy)]
enum FontSlot {
    TerminalPrimary,
    TerminalFallback1,
    TerminalFallback2,
    AppearancePrimary,
    AppearanceFallback,
}

impl FontSlot {
    /// Stable ASCII id suffix for the dropdown's `ElementId` — independent
    /// of the (Chinese, and non-unique across tabs — "首选字体" appears for
    /// both Terminal and Appearance) display label.
    fn id_suffix(self) -> &'static str {
        match self {
            FontSlot::TerminalPrimary => "terminal-primary",
            FontSlot::TerminalFallback1 => "terminal-fallback1",
            FontSlot::TerminalFallback2 => "terminal-fallback2",
            FontSlot::AppearancePrimary => "appearance-primary",
            FontSlot::AppearanceFallback => "appearance-fallback",
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum SettingsTab {
    General,
    Appearance,
    Terminal,
}

impl SettingsTab {
    fn label(self) -> &'static str {
        match self {
            SettingsTab::General => "General",
            SettingsTab::Appearance => "Appearance",
            SettingsTab::Terminal => "Terminal",
        }
    }
}

pub struct SettingsWindow {
    workspace: WeakEntity<Workspace>,
    committed: AppSettings,
    draft: AppSettings,
    active_tab: SettingsTab,
    font_size_input: Entity<InputState>,
    monitor_interval_input: Entity<InputState>,
    scrollback_input: Entity<InputState>,
    error: Option<SharedString>,
}

impl SettingsWindow {
    pub fn new(workspace: WeakEntity<Workspace>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let committed = settings::load();
        let font_size_input = cx.new(|cx| {
            InputState::new(window, cx).default_value(committed.terminal.font_size.to_string())
        });
        let monitor_interval_input = cx.new(|cx| {
            InputState::new(window, cx)
                .default_value(committed.terminal.monitor_basic_interval_secs.to_string())
        });
        let scrollback_input = cx.new(|cx| {
            InputState::new(window, cx)
                .default_value(committed.terminal.scrollback_lines.to_string())
        });
        Self {
            workspace,
            draft: committed.clone(),
            committed,
            active_tab: SettingsTab::Appearance,
            font_size_input,
            monitor_interval_input,
            scrollback_input,
            error: None,
        }
    }

    /// Read both inputs into the draft, validating font size. Returns `false`
    /// (and sets `self.error`) without mutating the draft further if the
    /// font-size field doesn't parse.
    fn sync_inputs_to_draft(&mut self, cx: &App) -> bool {
        let size_text = self.font_size_input.read(cx).value();
        let Some(size) = parse_font_size(&size_text) else {
            self.error = Some("字号必须是 6-96 之间的数字".into());
            return false;
        };
        self.draft.terminal.font_size = size;

        let interval_text = self.monitor_interval_input.read(cx).value();
        let Some(interval) = parse_monitor_interval(&interval_text) else {
            self.error = Some("轮询间隔必须是 1-3600 之间的整数(秒)".into());
            return false;
        };
        self.draft.terminal.monitor_basic_interval_secs = interval;

        let scrollback_text = self.scrollback_input.read(cx).value();
        let Some(scrollback_lines) = parse_scrollback_lines(&scrollback_text) else {
            self.error = Some("回滚行数必须是 1000-50000 之间的整数".into());
            return false;
        };
        self.draft.terminal.scrollback_lines = scrollback_lines;

        self.error = None;
        true
    }

    /// Persist the draft, apply it live (theme immediately; font broadcast to
    /// every open tab via `Workspace`), and update `committed`. Returns
    /// `false` (leaving the window open with `self.error` set) on validation
    /// or save failure.
    fn apply(&mut self, cx: &mut Context<Self>) -> bool {
        if !self.sync_inputs_to_draft(cx) {
            cx.notify();
            return false;
        }
        if let Err(e) = settings::save(&self.draft) {
            log::error!("failed to save settings: {e}");
            self.error = Some("保存设置失败".into());
            cx.notify();
            return false;
        }

        // Look up the exact theme by name (not just a light/dark mode) — see
        // `theme_dropdown`. Falls back to the built-in Default Dark theme if
        // the name isn't registered (shouldn't normally happen: the dropdown
        // only ever offers names straight from `ThemeRegistry`).
        let theme_name = SharedString::from(self.draft.appearance.theme_name.clone());
        match ThemeRegistry::global(cx).themes().get(&theme_name).cloned() {
            Some(theme) => Theme::global_mut(cx).apply_config(&theme),
            None => Theme::change(ThemeMode::Dark, None, cx),
        }
        // `Theme::change`/`apply_config` only refresh the window they're
        // given (none, here), so force every open window to repaint or
        // panels that don't redraw for other reasons keep showing stale colors.
        cx.refresh_windows();

        let font_family = self.draft.terminal.font_family.clone();
        let font_size = px(self.draft.terminal.font_size);
        let font_fallback1 = self.draft.terminal.font_fallback1.clone();
        let font_fallback2 = self.draft.terminal.font_fallback2.clone();
        let appearance_font_family = self.draft.appearance.font_family.clone();
        let appearance_font_fallback = self.draft.appearance.font_fallback.clone();
        let _ = self.workspace.update(cx, |workspace, cx| {
            workspace.apply_font_settings(font_family, font_size, font_fallback1, font_fallback2, cx);
            workspace.apply_appearance_font_settings(
                appearance_font_family,
                appearance_font_fallback,
                cx,
            );
        });

        self.committed = self.draft.clone();
        cx.notify();
        true
    }

    fn on_click_cancel(&mut self, _ev: &ClickEvent, window: &mut Window, cx: &mut Context<Self>) {
        self.draft = self.committed.clone();
        self.error = None;
        window.remove_window();
        cx.notify();
    }

    fn on_click_apply(&mut self, _ev: &ClickEvent, _window: &mut Window, cx: &mut Context<Self>) {
        self.apply(cx);
    }

    fn on_click_confirm(&mut self, _ev: &ClickEvent, window: &mut Window, cx: &mut Context<Self>) {
        if self.apply(cx) {
            window.remove_window();
        }
    }

    fn set_theme_name(&mut self, name: &str, cx: &mut Context<Self>) {
        self.draft.appearance.theme_name = name.to_string();
        cx.notify();
    }

    fn toggle_monitor_enabled(&mut self, cx: &mut Context<Self>) {
        self.draft.terminal.monitor_basic_enabled = !self.draft.terminal.monitor_basic_enabled;
        cx.notify();
    }

    fn font_slot_value(&self, slot: FontSlot) -> &str {
        match slot {
            FontSlot::TerminalPrimary => &self.draft.terminal.font_family,
            FontSlot::TerminalFallback1 => &self.draft.terminal.font_fallback1,
            FontSlot::TerminalFallback2 => &self.draft.terminal.font_fallback2,
            FontSlot::AppearancePrimary => &self.draft.appearance.font_family,
            FontSlot::AppearanceFallback => &self.draft.appearance.font_fallback,
        }
    }

    fn set_font_slot(&mut self, slot: FontSlot, value: &str, cx: &mut Context<Self>) {
        let field = match slot {
            FontSlot::TerminalPrimary => &mut self.draft.terminal.font_family,
            FontSlot::TerminalFallback1 => &mut self.draft.terminal.font_fallback1,
            FontSlot::TerminalFallback2 => &mut self.draft.terminal.font_fallback2,
            FontSlot::AppearancePrimary => &mut self.draft.appearance.font_family,
            FontSlot::AppearanceFallback => &mut self.draft.appearance.font_fallback,
        };
        *field = value.to_string();
        cx.notify();
    }

    /// One "首选/备选" font dropdown: a text label above, a `DropdownButton`
    /// below showing the current choice's display name and offering all of
    /// `FONT_CHOICES`. Shared by all 5 font fields across both tabs — see
    /// the design spec for why a fixed dropdown (not free text) is used
    /// here, including for what used to be a free-text field.
    fn font_picker(&self, label: &'static str, slot: FontSlot, cx: &Context<Self>) -> impl IntoElement {
        let current = self.font_slot_value(slot).to_string();
        let display = FONT_CHOICES
            .iter()
            .find(|(value, _)| *value == current)
            .map(|(_, label)| *label)
            .unwrap_or("系统默认");
        let weak = cx.entity().downgrade();
        div()
            .flex()
            .flex_col()
            .gap_0p5()
            .child(
                div()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(label),
            )
            .child(
                DropdownButton::new(SharedString::from(format!(
                    "font-picker-{}",
                    slot.id_suffix()
                )))
                .xsmall()
                .button(
                    Button::new(SharedString::from(format!(
                        "font-picker-btn-{}",
                        slot.id_suffix()
                    )))
                    .xsmall()
                    .label(display.to_string()),
                )
                .dropdown_menu(move |menu, _window, _cx| {
                    let mut menu = menu;
                    for &(value, choice_label) in FONT_CHOICES {
                        let value = value.to_string();
                        let weak = weak.clone();
                        menu = menu.item(PopupMenuItem::new(choice_label).on_click(
                            move |_ev, _window, cx| {
                                let value = value.clone();
                                let _ = weak.update(cx, |this, cx| {
                                    this.set_font_slot(slot, &value, cx);
                                });
                            },
                        ));
                    }
                    menu
                }),
            )
    }

    /// One sidebar tab button.
    fn tab_button(&self, tab: SettingsTab, cx: &Context<Self>) -> impl IntoElement {
        let active = self.active_tab == tab;
        div()
            .id(SharedString::from(format!("settings-tab-{}", tab.label())))
            .px_3()
            .py_1()
            .rounded_sm()
            .bg(if active {
                cx.theme().list_active
            } else {
                transparent_black()
            })
            .text_color(if active {
                cx.theme().foreground
            } else {
                cx.theme().muted_foreground
            })
            .hover(|s| s.bg(cx.theme().accent))
            .child(tab.label())
            .on_click(cx.listener(move |this, _ev: &ClickEvent, _window, cx| {
                this.active_tab = tab;
                cx.notify();
            }))
    }

    /// Theme dropdown: lists every theme in `ThemeRegistry` (the built-in
    /// Default Light/Dark pair plus the bundled gpui-component theme
    /// collection — see `main.rs`'s `BUNDLED_THEMES`); selecting one applies
    /// it immediately by exact name, not just a light/dark mode.
    fn theme_dropdown(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let current = self.draft.appearance.theme_name.clone();
        let weak = cx.entity().downgrade();
        let names: Vec<SharedString> = ThemeRegistry::global(cx)
            .sorted_themes()
            .into_iter()
            .map(|theme| theme.name.clone())
            .collect();

        DropdownButton::new("settings-theme-picker")
            .xsmall()
            .button(
                Button::new("settings-theme-picker-btn")
                    .xsmall()
                    .label(current),
            )
            .dropdown_menu(move |menu, _window, _cx| {
                let mut menu = menu;
                for name in &names {
                    let name = name.clone();
                    let weak = weak.clone();
                    menu = menu.item(PopupMenuItem::new(name.clone()).on_click(
                        move |_ev, _window, cx| {
                            let name = name.clone();
                            let _ = weak.update(cx, |this, cx| {
                                this.set_theme_name(&name, cx);
                            });
                        },
                    ));
                }
                menu
            })
    }

    fn render_placeholder_tab(&self, title: &str, cx: &Context<Self>) -> impl IntoElement {
        div()
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .gap_2()
            .size_full()
            .text_sm()
            .text_color(cx.theme().muted_foreground)
            .child(div().text_color(cx.theme().foreground).child(title.to_string()))
            .child("此设置尚未实现")
    }

    /// UI-level appearance only (theme). The terminal's own font lives on the
    /// Terminal tab (`render_terminal_tab`) — it affects terminal content,
    /// not the application chrome this tab controls.
    fn render_appearance_tab(&self, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .flex()
            .flex_col()
            .gap_3()
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_0p5()
                    .child(
                        div()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child("主题"),
                    )
                    .child(self.theme_dropdown(cx)),
            )
            .child(self.font_picker("首选字体", FontSlot::AppearancePrimary, cx))
            .child(self.font_picker("备选字体", FontSlot::AppearanceFallback, cx))
    }

    /// Terminal-content settings: currently just font family/size, which only
    /// affect `TerminalView` rendering (see `Workspace::apply_font_settings`),
    /// not the application chrome.
    fn render_terminal_tab(&self, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .flex()
            .flex_col()
            .gap_3()
            .child(self.font_picker("首选字体", FontSlot::TerminalPrimary, cx))
            .child(self.font_picker("备选字体 1", FontSlot::TerminalFallback1, cx))
            .child(self.font_picker("备选字体 2", FontSlot::TerminalFallback2, cx))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_0p5()
                    .child(
                        div()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child("字号 (px)"),
                    )
                    .child(Input::new(&self.font_size_input)),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_0p5()
                    .child(
                        div()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child("资源监控 (基础)"),
                    )
                    .child(self.monitor_enabled_switch(cx)),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_0p5()
                    .child(
                        div()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child("轮询间隔 (秒)"),
                    )
                    .child(Input::new(&self.monitor_interval_input)),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_0p5()
                    .child(
                        div()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child("回滚行数"),
                    )
                    .child(Input::new(&self.scrollback_input)),
            )
    }

    /// Standard boolean-toggle style for this settings window: a pill switch
    /// (`gpui_component::switch::Switch`), not a text pill button — see the
    /// "UI conventions" note in `docs/superpowers/specs/2026-07-07-settings-page-design.md`.
    fn monitor_enabled_switch(&self, cx: &Context<Self>) -> impl IntoElement {
        Switch::new("settings-monitor-enabled")
            .checked(self.draft.terminal.monitor_basic_enabled)
            .on_click(cx.listener(|this, _checked: &bool, _window, cx| {
                this.toggle_monitor_enabled(cx);
            }))
    }
}

impl Render for SettingsWindow {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let border = cx.theme().border;
        let content = match self.active_tab {
            SettingsTab::General => self.render_placeholder_tab("General", cx).into_any_element(),
            SettingsTab::Appearance => self.render_appearance_tab(cx).into_any_element(),
            SettingsTab::Terminal => self.render_terminal_tab(cx).into_any_element(),
        };

        div()
            .flex()
            .flex_col()
            .size_full()
            .child(
                div()
                    .flex()
                    .flex_row()
                    .flex_1()
                    .min_h(px(0.0))
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap_1()
                            .w(px(140.0))
                            .p_2()
                            .border_r_1()
                            .border_color(border)
                            .child(self.tab_button(SettingsTab::General, cx))
                            .child(self.tab_button(SettingsTab::Appearance, cx))
                            .child(self.tab_button(SettingsTab::Terminal, cx)),
                    )
                    .child(div().flex_1().p_4().child(content)),
            )
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .justify_end()
                    .gap_2()
                    .p_2()
                    .border_t_1()
                    .border_color(border)
                    .when_some(self.error.clone(), |el, err| {
                        el.child(
                            div()
                                .flex_1()
                                .text_xs()
                                .text_color(red())
                                .child(err),
                        )
                    })
                    .child(
                        div()
                            .id("settings-cancel")
                            .px_2()
                            .py_0p5()
                            .rounded_sm()
                            .hover(|s| s.bg(cx.theme().accent))
                            .child("取消")
                            .on_click(cx.listener(Self::on_click_cancel)),
                    )
                    .child(
                        div()
                            .id("settings-apply")
                            .px_2()
                            .py_0p5()
                            .rounded_sm()
                            .hover(|s| s.bg(cx.theme().accent))
                            .child("应用")
                            .on_click(cx.listener(Self::on_click_apply)),
                    )
                    .child(
                        div()
                            .id("settings-confirm")
                            .px_2()
                            .py_0p5()
                            .rounded_sm()
                            .bg(cx.theme().primary)
                            .text_color(cx.theme().primary_foreground)
                            .child("确定")
                            .on_click(cx.listener(Self::on_click_confirm)),
                    ),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_valid_size() {
        assert_eq!(parse_font_size("14"), Some(14.0));
        assert_eq!(parse_font_size(" 16.5 "), Some(16.5));
    }

    #[test]
    fn rejects_non_numeric() {
        assert_eq!(parse_font_size("abc"), None);
        assert_eq!(parse_font_size(""), None);
    }

    #[test]
    fn rejects_out_of_range() {
        assert_eq!(parse_font_size("0"), None);
        assert_eq!(parse_font_size("-5"), None);
        assert_eq!(parse_font_size("500"), None);
    }

    #[test]
    fn parses_valid_scrollback_lines() {
        assert_eq!(parse_scrollback_lines("10000"), Some(10_000));
        assert_eq!(parse_scrollback_lines(" 1000 "), Some(1_000));
        assert_eq!(parse_scrollback_lines("50000"), Some(50_000));
    }

    #[test]
    fn rejects_out_of_range_scrollback_lines() {
        assert_eq!(parse_scrollback_lines("999"), None);
        assert_eq!(parse_scrollback_lines("50001"), None);
    }

    #[test]
    fn rejects_non_numeric_scrollback_lines() {
        assert_eq!(parse_scrollback_lines("abc"), None);
        assert_eq!(parse_scrollback_lines(""), None);
    }
}
