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

use gpui::{
    App, AppContext, ClickEvent, Context, Entity, InteractiveElement, IntoElement, ParentElement,
    Render, SharedString, StatefulInteractiveElement, Styled, WeakEntity, Window,
    div, prelude::FluentBuilder, px, red, transparent_black,
};
use gpui_component::input::{Input, InputState};
use gpui_component::{ActiveTheme, Theme, ThemeMode};

use crate::settings::{self, AppSettings};
use crate::workspace::Workspace;

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
    font_family_input: Entity<InputState>,
    font_size_input: Entity<InputState>,
    error: Option<SharedString>,
}

impl SettingsWindow {
    pub fn new(workspace: WeakEntity<Workspace>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let committed = settings::load();
        let font_family_input = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("留空 = 内置默认字体")
                .default_value(committed.terminal.font_family.clone())
        });
        let font_size_input = cx.new(|cx| {
            InputState::new(window, cx).default_value(committed.terminal.font_size.to_string())
        });
        Self {
            workspace,
            draft: committed.clone(),
            committed,
            active_tab: SettingsTab::Appearance,
            font_family_input,
            font_size_input,
            error: None,
        }
    }

    /// Read both inputs into the draft, validating font size. Returns `false`
    /// (and sets `self.error`) without mutating the draft further if the
    /// font-size field doesn't parse.
    fn sync_inputs_to_draft(&mut self, cx: &App) -> bool {
        self.draft.terminal.font_family = self.font_family_input.read(cx).value().to_string();
        let size_text = self.font_size_input.read(cx).value();
        match parse_font_size(&size_text) {
            Some(size) => {
                self.draft.terminal.font_size = size;
                self.error = None;
                true
            }
            None => {
                self.error = Some("字号必须是 6-96 之间的数字".into());
                false
            }
        }
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

        let mode = if self.draft.appearance.theme_mode == "light" {
            ThemeMode::Light
        } else {
            ThemeMode::Dark
        };
        Theme::change(mode, None, cx);

        let font_family = self.draft.terminal.font_family.clone();
        let font_size = px(self.draft.terminal.font_size);
        let _ = self.workspace.update(cx, |workspace, cx| {
            workspace.apply_font_settings(font_family, font_size, cx);
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

    fn set_theme_mode(&mut self, mode: &str, cx: &mut Context<Self>) {
        self.draft.appearance.theme_mode = mode.to_string();
        cx.notify();
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

    /// One theme-mode pill (shared visual idiom with
    /// `saved_connections.rs`'s connection-type pills).
    fn theme_pill(&self, mode: &'static str, label: &'static str, cx: &Context<Self>) -> impl IntoElement {
        let active = self.draft.appearance.theme_mode == mode;
        div()
            .id(SharedString::from(format!("settings-theme-{mode}")))
            .px_2()
            .py_0p5()
            .rounded_sm()
            .bg(if active {
                cx.theme().primary
            } else {
                cx.theme().accent
            })
            .text_color(if active {
                cx.theme().primary_foreground
            } else {
                cx.theme().foreground
            })
            .hover(|s| s.bg(cx.theme().accent))
            .child(label)
            .on_click(cx.listener(move |this, _ev: &ClickEvent, _window, cx| {
                this.set_theme_mode(mode, cx);
            }))
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
                    .child(
                        div()
                            .flex()
                            .flex_row()
                            .gap_2()
                            .child(self.theme_pill("dark", "深色", cx))
                            .child(self.theme_pill("light", "浅色", cx)),
                    ),
            )
    }

    /// Terminal-content settings: currently just font family/size, which only
    /// affect `TerminalView` rendering (see `Workspace::apply_font_settings`),
    /// not the application chrome.
    fn render_terminal_tab(&self, cx: &mut Context<Self>) -> impl IntoElement {
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
                            .child("字体族"),
                    )
                    .child(Input::new(&self.font_family_input)),
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
                            .child("字号 (px)"),
                    )
                    .child(Input::new(&self.font_size_input)),
            )
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
}
