# Settings Page Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add a standalone settings window (File menu → "设置...") with a draft +
Apply/Confirm/Cancel model, backed by a new `settings.toml`, with a working Appearance tab
(font family/size, theme mode) that broadcasts to every already-open terminal tab, plus
placeholder General/Terminal tabs for future sub-projects to fill in.

**Architecture:** New `src/settings.rs` mirrors `src/config.rs`'s existing
load/save/path shape exactly, for a separate `settings.toml`. A new
`src/panels/settings_window.rs` is a second gpui window (same `cx.open_window` +
`gpui_component::Root::new` recipe `main.rs` already uses for the main window), holding a
cloned draft of `AppSettings` that only gets written back on Apply/Confirm. `Workspace`
gains a `terminal_views: Vec<WeakEntity<TerminalView>>` registry (populated at every
terminal-creation call site) so Apply can broadcast the new font to every live tab via the
existing `TerminalView::set_font_family`/`set_font_size` methods, plus a
`settings_window: Option<WindowHandle<Root>>` so re-opening focuses the existing window
instead of duplicating it.

**Tech Stack:** Rust, gpui + gpui_platform (git rev pinned in `Cargo.toml`), gpui-component
(`Input`/`InputState`, `Root`, `Button`, `Theme`/`ThemeMode`), `serde`/`toml` (already a
dependency via `config.rs`).

## Global Constraints

- Only `Regular` weights / existing font infra — no new font files, this plan only adds a
  *picker* for the family name already resolved by `terminal::view`'s existing
  `set_font_family`/`set_font_size`.
- Settings persist to a **new**, separate `~/.config/caracal/settings.toml` — never folded
  into `connections.toml` / `AppConfig`.
- Draft/Apply/Confirm/Cancel: no field written to disk or applied live except on Apply or
  Confirm; Cancel discards the draft entirely.
- Font changes on Apply/Confirm must reach **every currently-open terminal tab**, not just
  future ones.
- Only one settings window open at a time; re-triggering the menu item while one is open
  focuses it instead of opening a second.
- General and Terminal tabs are **placeholders only** in this pass — no backing fields, no
  premature struct scaffolding for settings that don't exist yet.
- Full spec: `docs/superpowers/specs/2026-07-07-settings-page-design.md`. Full roadmap
  context: `docs/reference/nyaterm-gap-roadmap.md`.

---

### Task 1: `src/settings.rs` — data model and persistence

**Files:**
- Create: `src/settings.rs`
- Modify: `src/main.rs:9-13` (add `mod settings;`)

**Interfaces:**
- Produces: `AppSettings { appearance: AppearanceSettings }`,
  `AppearanceSettings { font_family: String, font_size: f32, theme_mode: String }`,
  `settings::settings_path() -> PathBuf`, `settings::load() -> AppSettings`,
  `settings::save(&AppSettings) -> anyhow::Result<()>` — consumed by Task 3
  (`settings_window.rs`) and Task 5 (`main.rs` startup/theme persistence).

- [ ] **Step 1: Write the failing tests**

Create `src/settings.rs` with just the test module first (everything else will 404 on
compile, which is step 2's expected-fail):

```rust
//! Persisted application-level settings (font, theme, and future preferences),
//! separate from `config.rs`'s connections/groups. Plain Rust — no
//! `gpui_component` here (CLAUDE.md §1 boundary).
//!
//! Stored at `$XDG_CONFIG_HOME/caracal/settings.toml` (else
//! `~/.config/caracal/settings.toml`).

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_settings_have_expected_values() {
        let settings = AppSettings::default();
        assert_eq!(settings.appearance.font_family, "");
        assert_eq!(settings.appearance.font_size, 14.0);
        assert_eq!(settings.appearance.theme_mode, "dark");
    }

    #[test]
    fn round_trip_preserves_fields() {
        let settings = AppSettings {
            appearance: AppearanceSettings {
                font_family: "Consolas".to_string(),
                font_size: 16.0,
                theme_mode: "light".to_string(),
            },
        };
        let text = toml::to_string_pretty(&settings).expect("serialize");
        let parsed: AppSettings = toml::from_str(&text).expect("deserialize");
        assert_eq!(parsed.appearance.font_family, "Consolas");
        assert_eq!(parsed.appearance.font_size, 16.0);
        assert_eq!(parsed.appearance.theme_mode, "light");
    }

    #[test]
    fn partial_toml_still_deserializes_with_defaults() {
        // Simulates a settings.toml written before a future field is added:
        // an empty [appearance] table should still fill in every default.
        let toml_text = "[appearance]\n";
        let settings: AppSettings =
            toml::from_str(toml_text).expect("partial settings must still parse");
        assert_eq!(settings.appearance.font_family, "");
        assert_eq!(settings.appearance.font_size, 14.0);
        assert_eq!(settings.appearance.theme_mode, "dark");
    }

    #[test]
    fn empty_file_yields_default_appearance() {
        let settings: AppSettings = toml::from_str("").expect("empty file must still parse");
        assert_eq!(settings.appearance.font_size, 14.0);
    }
}
```

(This mirrors `src/config.rs`'s own test module exactly in spirit: it never tests `load`/
`save`/`*_path` directly — those are thin `std::fs` wrappers with no branching logic worth
mocking the environment for — only the pure serde round-trip.)

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --lib settings:: 2>&1 | tail -30`
Expected: FAIL to compile — `AppSettings`/`AppearanceSettings` not found, `settings` module
not yet registered in `main.rs`. Register it now so the compiler error is just "type not
found" rather than "module not found": in `src/main.rs`, current state (lines 9-13):

```rust
mod assets;
mod config;
mod panels;
mod terminal;
mod workspace;
```

Replace with:

```rust
mod assets;
mod config;
mod panels;
mod settings;
mod terminal;
mod workspace;
```

Re-run the same test command. Expected: FAIL — `AppSettings`/`AppearanceSettings` not
defined in `src/settings.rs`.

- [ ] **Step 3: Implement `AppSettings`/`AppearanceSettings` and persistence**

Add above the `#[cfg(test)]` block in `src/settings.rs`:

```rust
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// The whole persisted settings file.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct AppSettings {
    #[serde(default)]
    pub appearance: AppearanceSettings,
}

/// Font + theme settings, editable from Settings → Appearance.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AppearanceSettings {
    /// Empty string = bundled default (`terminal::view`'s `DEFAULT_FONT_FAMILY`).
    #[serde(default)]
    pub font_family: String,
    /// Raw point size; `TerminalView::set_font_size` takes `gpui::Pixels`, so
    /// callers convert via `px(settings.appearance.font_size)` — `Pixels`
    /// itself isn't (de)serializable, hence the raw `f32` here.
    #[serde(default = "default_font_size")]
    pub font_size: f32,
    /// `"dark"` | `"light"`.
    #[serde(default = "default_theme_mode")]
    pub theme_mode: String,
}

fn default_font_size() -> f32 {
    14.0
}

fn default_theme_mode() -> String {
    "dark".to_string()
}

impl Default for AppearanceSettings {
    fn default() -> Self {
        Self {
            font_family: String::new(),
            font_size: default_font_size(),
            theme_mode: default_theme_mode(),
        }
    }
}

/// `~/.config/caracal/settings.toml`.
pub fn settings_path() -> PathBuf {
    let base = std::env::var("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
            PathBuf::from(home).join(".config")
        });
    base.join("caracal").join("settings.toml")
}

/// Load settings. Missing file → default. A parse error is logged and also
/// yields the default, so a corrupt file never crashes startup.
pub fn load() -> AppSettings {
    let path = settings_path();
    let text = match std::fs::read_to_string(&path) {
        Ok(t) => t,
        Err(_) => return AppSettings::default(),
    };
    match toml::from_str(&text) {
        Ok(settings) => settings,
        Err(e) => {
            log::warn!("failed to parse {}: {e}", path.display());
            AppSettings::default()
        }
    }
}

/// Persist settings, creating the parent directory if needed.
pub fn save(settings: &AppSettings) -> anyhow::Result<()> {
    let path = settings_path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let text = toml::to_string_pretty(settings)?;
    std::fs::write(&path, text)?;
    Ok(())
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --lib settings:: -- --nocapture`
Expected: PASS — all 4 tests (`default_settings_have_expected_values`,
`round_trip_preserves_fields`, `partial_toml_still_deserializes_with_defaults`,
`empty_file_yields_default_appearance`) succeed.

- [ ] **Step 5: Run the full suite to check for regressions**

Run: `cargo test 2>&1 | tail -15`
Expected: all pre-existing tests still pass (43 before this task), plus the 4 new ones (47
total), 0 failed.

- [ ] **Step 6: Commit**

```bash
git add src/settings.rs src/main.rs
git commit -m "feat: add settings.rs persisted app-level settings (font + theme)"
```

---

### Task 2: `Workspace` — track every open terminal view

**Files:**
- Modify: `src/workspace.rs` (struct field + 5 terminal-creation methods)

**Interfaces:**
- Produces: `Workspace.terminal_views: Vec<WeakEntity<TerminalView>>`, populated by every
  method that creates a `TerminalView` — consumed by Task 3's `apply_font_settings`.

This task has no new automated tests (there's no pure logic to unit-test — it's `Vec::push`
at 5 call sites plus one field addition; the codebase doesn't unit-test view-tree wiring
like this, consistent with `open_local`/`open_ssh`/etc. having no existing tests of their
own). Verify with `cargo build` at the end.

- [ ] **Step 1: Add the field**

Current state (`src/workspace.rs`, in the `Workspace` struct):

```rust
pub struct Workspace {
    /// Hosts the CENTER terminal tabs only (no side docks anymore).
    dock_area: Entity<DockArea>,
    /// Shared SSH connections, keyed by `user@host:port`.
    ssh_sessions: HashMap<String, Arc<SshSession>>,
```

Replace with:

```rust
pub struct Workspace {
    /// Hosts the CENTER terminal tabs only (no side docks anymore).
    dock_area: Entity<DockArea>,
    /// Shared SSH connections, keyed by `user@host:port`.
    ssh_sessions: HashMap<String, Arc<SshSession>>,
    /// Every `TerminalView` this workspace has created, so settings changes
    /// (e.g. font) can be broadcast to already-open tabs. Dead weak refs are
    /// pruned lazily on the next broadcast rather than on tab close.
    terminal_views: Vec<WeakEntity<TerminalView>>,
```

- [ ] **Step 2: Initialize the field in `Workspace::new`**

Current state (`src/workspace.rs`, the `Self { .. }` literal in `new`):

```rust
        Self {
            dock_area,
            ssh_sessions: HashMap::new(),
            saved_panel: saved.into(),
```

Replace with:

```rust
        Self {
            dock_area,
            ssh_sessions: HashMap::new(),
            terminal_views: Vec::new(),
            saved_panel: saved.into(),
```

- [ ] **Step 3: Register in `open_local`**

Current state:

```rust
    pub fn open_local(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let terminal = cx.new(|cx| TerminalView::new(window, cx));
        let handle = terminal.read(cx).focus_handle(cx);
        let term_weak = terminal.downgrade();
        let sub = cx.on_focus(&handle, window, move |this, window, cx| {
```

Replace with:

```rust
    pub fn open_local(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let terminal = cx.new(|cx| TerminalView::new(window, cx));
        let handle = terminal.read(cx).focus_handle(cx);
        let term_weak = terminal.downgrade();
        self.terminal_views.push(term_weak.clone());
        let sub = cx.on_focus(&handle, window, move |this, window, cx| {
```

- [ ] **Step 4: Register in `open_local_with`**

Current state:

```rust
        };
        let handle = terminal.read(cx).focus_handle(cx);
        let term_weak = terminal.downgrade();
        let sub = cx.on_focus(&handle, window, move |this, window, cx| {
            this.set_active_title_from(&term_weak, cx);
            this.show_sftp_placeholder(window, cx);
        });
        self._subscriptions.push(sub);
        let panel = cx.new(|_cx| TerminalPanel::new(terminal));
        self.add_center(Arc::new(panel), window, cx);
        self.show_sftp_placeholder(window, cx);
    }

    /// Open an SSH shell terminal
```

Replace with (this matches `open_local_with`'s body — the one right before the SSH doc
comment):

```rust
        };
        let handle = terminal.read(cx).focus_handle(cx);
        let term_weak = terminal.downgrade();
        self.terminal_views.push(term_weak.clone());
        let sub = cx.on_focus(&handle, window, move |this, window, cx| {
            this.set_active_title_from(&term_weak, cx);
            this.show_sftp_placeholder(window, cx);
        });
        self._subscriptions.push(sub);
        let panel = cx.new(|_cx| TerminalPanel::new(terminal));
        self.add_center(Arc::new(panel), window, cx);
        self.show_sftp_placeholder(window, cx);
    }

    /// Open an SSH shell terminal
```

- [ ] **Step 5: Register in `open_ssh`**

Current state:

```rust
        if let Some(session) = self.ssh_session(&config) {
            let terminal = cx.new(|cx| TerminalView::new_ssh_shell(window, cx, session));
            let follow = config.clone();
            let handle = terminal.read(cx).focus_handle(cx);
            let term_weak = terminal.downgrade();
            let sub = cx.on_focus(&handle, window, move |this, window, cx| {
```

Replace with:

```rust
        if let Some(session) = self.ssh_session(&config) {
            let terminal = cx.new(|cx| TerminalView::new_ssh_shell(window, cx, session));
            let follow = config.clone();
            let handle = terminal.read(cx).focus_handle(cx);
            let term_weak = terminal.downgrade();
            self.terminal_views.push(term_weak.clone());
            let sub = cx.on_focus(&handle, window, move |this, window, cx| {
```

- [ ] **Step 6: Register in `open_telnet`**

Current state:

```rust
    pub fn open_telnet(&mut self, config: TelnetConfig, window: &mut Window, cx: &mut Context<Self>) {
        let terminal = cx.new(|cx| TerminalView::new_telnet(window, cx, config));
        let handle = terminal.read(cx).focus_handle(cx);
        let term_weak = terminal.downgrade();
        let sub = cx.on_focus(&handle, window, move |this, window, cx| {
```

Replace with:

```rust
    pub fn open_telnet(&mut self, config: TelnetConfig, window: &mut Window, cx: &mut Context<Self>) {
        let terminal = cx.new(|cx| TerminalView::new_telnet(window, cx, config));
        let handle = terminal.read(cx).focus_handle(cx);
        let term_weak = terminal.downgrade();
        self.terminal_views.push(term_weak.clone());
        let sub = cx.on_focus(&handle, window, move |this, window, cx| {
```

- [ ] **Step 7: Register in `open_serial`**

Current state:

```rust
    pub fn open_serial(&mut self, config: SerialConfig, window: &mut Window, cx: &mut Context<Self>) {
        let terminal = cx.new(|cx| TerminalView::new_serial(window, cx, config));
        let handle = terminal.read(cx).focus_handle(cx);
        let term_weak = terminal.downgrade();
        let sub = cx.on_focus(&handle, window, move |this, window, cx| {
```

Replace with:

```rust
    pub fn open_serial(&mut self, config: SerialConfig, window: &mut Window, cx: &mut Context<Self>) {
        let terminal = cx.new(|cx| TerminalView::new_serial(window, cx, config));
        let handle = terminal.read(cx).focus_handle(cx);
        let term_weak = terminal.downgrade();
        self.terminal_views.push(term_weak.clone());
        let sub = cx.on_focus(&handle, window, move |this, window, cx| {
```

- [ ] **Step 8: Build to confirm it compiles**

Run: `cargo build 2>&1 | tail -30`
Expected: builds successfully, no errors (an unused-field warning is expected and fine —
`terminal_views` isn't read yet until Task 3).

- [ ] **Step 9: Commit**

```bash
git add src/workspace.rs
git commit -m "feat: track every open TerminalView on Workspace for settings broadcast"
```

---

### Task 3: `SettingsWindow` view + `Workspace` wiring to open/broadcast

**Files:**
- Create: `src/panels/settings_window.rs`
- Modify: `src/panels/mod.rs` (register the new module)
- Modify: `src/workspace.rs` (imports, `settings_window` field, `open_settings`,
  `apply_font_settings`)

**Interfaces:**
- Consumes: `settings::{AppSettings, AppearanceSettings, load, save}` (Task 1),
  `Workspace.terminal_views` (Task 2), `TerminalView::set_font_family`/`set_font_size`
  (pre-existing, `src/terminal/view.rs`).
- Produces: `SettingsWindow::new(workspace: WeakEntity<Workspace>, window: &mut Window, cx:
  &mut Context<Self>) -> Self`, `Workspace::open_settings(&mut self, window: &mut Window, cx:
  &mut Context<Self>)`, `Workspace::apply_font_settings(&mut self, font_family: String,
  font_size: gpui::Pixels, cx: &mut Context<Self>)` — consumed by Task 4 (`header.rs`'s menu
  item calls `open_settings`).

This task has one small pure/testable unit (`parse_font_size`) and otherwise UI wiring with
no automated tests, consistent with the rest of the codebase's `panels/*.rs` (`render()`
methods aren't unit-tested; `cargo build` + the end-to-end manual check in Task 6 is the
verification for the UI itself).

- [ ] **Step 1: Write the failing test for the font-size parser**

Create `src/panels/settings_window.rs` with just this much:

```rust
//! `SettingsWindow`: a standalone second window (File → "设置...") for
//! application-level settings. Follows nyaterm's draft + Apply/Confirm/Cancel
//! model: [`SettingsWindow`] clones the committed [`crate::settings::AppSettings`]
//! into a local draft on open; nothing is written to `settings.toml` or applied
//! live until Apply or Confirm.

/// Parse the Appearance tab's font-size text field. Rejects non-finite,
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
```

- [ ] **Step 2: Run tests to verify they pass**

Run: `cargo test --lib panels::settings_window:: -- --nocapture`

This will fail to compile until the module is registered — that's expected. Register it now
in `src/panels/mod.rs`: read the file first (`cat src/panels/mod.rs`) and add
`pub mod settings_window;` alongside the other `pub mod` declarations there (matching
whatever style the existing entries use — e.g. `pub mod sftp;`, `pub mod stub;`).

Re-run: `cargo test --lib panels::settings_window:: -- --nocapture`
Expected: PASS — all 3 tests (`parses_valid_size`, `rejects_non_numeric`,
`rejects_out_of_range`) succeed. (There's no separate "write it failing first" step for this
one beyond the module-not-registered compile failure, since the function above is trivial
enough to write correctly in one pass — but confirm the 3 tests actually exercise it by
running them now before moving on.)

- [ ] **Step 3: Add `Workspace` wiring — imports, field, `open_settings`,
  `apply_font_settings`**

In `src/workspace.rs`, current imports (top of file):

```rust
use gpui::{
    AnyView, App, AppContext, Context, Entity, Focusable, IntoElement, ParentElement,
    Render, SharedString, StatefulInteractiveElement, Styled, Subscription, WeakEntity, Window,
    div, prelude::FluentBuilder, px,
};
use gpui_component::dock::{DockArea, DockPlacement};
use gpui_component::resizable::{ResizableState, resizable_panel, h_resizable};
use gpui_component::ActiveTheme;

use crate::config;
use crate::panels::activity_bar::{PanelId, Side, activity_button, side_items};
use crate::panels::header::render_header;
use crate::panels::saved_connections::{SavedConnectionsEvent, SavedConnectionsPanel};
use crate::panels::side_region::side_region_content;
use crate::panels::sftp::{SftpPanel, SftpPlaceholder};
use crate::panels::stub::StubPanel;
use crate::panels::terminal::TerminalPanel;
use crate::terminal::serial::SerialConfig;
use crate::terminal::ssh::{SshConfig, SshSession};
use crate::terminal::telnet::TelnetConfig;
use crate::terminal::view::TerminalView;
```

Replace with:

```rust
use gpui::{
    AnyView, App, AppContext, Bounds, Context, Entity, Focusable, IntoElement, ParentElement,
    Pixels, Render, SharedString, StatefulInteractiveElement, Styled, Subscription, WeakEntity,
    Window, WindowBounds, WindowHandle, WindowOptions, div, prelude::FluentBuilder, px, size,
};
use gpui_component::dock::{DockArea, DockPlacement};
use gpui_component::resizable::{ResizableState, resizable_panel, h_resizable};
use gpui_component::{ActiveTheme, Root};

use crate::config;
use crate::panels::activity_bar::{PanelId, Side, activity_button, side_items};
use crate::panels::header::render_header;
use crate::panels::saved_connections::{SavedConnectionsEvent, SavedConnectionsPanel};
use crate::panels::settings_window::SettingsWindow;
use crate::panels::side_region::side_region_content;
use crate::panels::sftp::{SftpPanel, SftpPlaceholder};
use crate::panels::stub::StubPanel;
use crate::panels::terminal::TerminalPanel;
use crate::terminal::serial::SerialConfig;
use crate::terminal::ssh::{SshConfig, SshSession};
use crate::terminal::telnet::TelnetConfig;
use crate::terminal::view::TerminalView;
```

Current state (the `terminal_views` field added in Task 2, plus what follows it):

```rust
    /// Every `TerminalView` this workspace has created, so settings changes
    /// (e.g. font) can be broadcast to already-open tabs. Dead weak refs are
    /// pruned lazily on the next broadcast rather than on tab close.
    terminal_views: Vec<WeakEntity<TerminalView>>,
```

Add right after it:

```rust
    /// The open settings window, if any — re-triggering the menu item
    /// focuses this instead of opening a duplicate.
    settings_window: Option<WindowHandle<Root>>,
```

Current state (the `Self { .. }` literal in `Workspace::new`, right after Task 2's
`terminal_views: Vec::new(),`):

```rust
        Self {
            dock_area,
            ssh_sessions: HashMap::new(),
            terminal_views: Vec::new(),
            saved_panel: saved.into(),
```

Replace with:

```rust
        Self {
            dock_area,
            ssh_sessions: HashMap::new(),
            terminal_views: Vec::new(),
            settings_window: None,
            saved_panel: saved.into(),
```

Add these two new methods anywhere in the first `impl Workspace` block (e.g. right after
`open_serial`, before `set_active_title_from`):

```rust
    /// Open the settings window, or focus it if one is already open.
    pub fn open_settings(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        if let Some(handle) = &self.settings_window {
            if handle
                .update(cx, |_root, window, _cx| window.activate_window())
                .is_ok()
            {
                return;
            }
            // Handle is stale (window was closed) — fall through and open a
            // fresh one, replacing it below.
        }

        let workspace = cx.entity().downgrade();
        let bounds = Bounds::centered(None, size(px(640.0), px(480.0)), cx);
        let result = cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                focus: true,
                ..Default::default()
            },
            move |window, cx| {
                let settings_window =
                    cx.new(|cx| SettingsWindow::new(workspace.clone(), window, cx));
                cx.new(|cx| Root::new(settings_window, window, cx).bg(cx.theme().background))
            },
        );
        match result {
            Ok(handle) => self.settings_window = Some(handle),
            Err(e) => log::error!("failed to open settings window: {e}"),
        }
    }

    /// Broadcast a new font family/size to every currently-open terminal tab,
    /// pruning any that have since closed. Called by [`SettingsWindow`] on
    /// Apply/Confirm.
    pub fn apply_font_settings(
        &mut self,
        font_family: String,
        font_size: Pixels,
        cx: &mut Context<Self>,
    ) {
        self.terminal_views.retain(|weak| {
            weak.update(cx, |view, cx| {
                view.set_font_family(font_family.clone(), cx);
                view.set_font_size(font_size, cx);
            })
            .is_ok()
        });
    }
```

- [ ] **Step 4: Write `SettingsWindow`**

Append to `src/panels/settings_window.rs` (after the `parse_font_size` function, before the
`#[cfg(test)]` module):

```rust
use gpui::{
    App, ClickEvent, Context, Entity, IntoElement, ParentElement, Render, SharedString,
    StatefulInteractiveElement, Styled, WeakEntity, Window, div, prelude::FluentBuilder, px,
    red, transparent_black,
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
                .default_value(committed.appearance.font_family.clone())
        });
        let font_size_input = cx.new(|cx| {
            InputState::new(window, cx).default_value(committed.appearance.font_size.to_string())
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
        self.draft.appearance.font_family = self.font_family_input.read(cx).value().to_string();
        let size_text = self.font_size_input.read(cx).value();
        match parse_font_size(&size_text) {
            Some(size) => {
                self.draft.appearance.font_size = size;
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

        let font_family = self.draft.appearance.font_family.clone();
        let font_size = px(self.draft.appearance.font_size);
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
}

impl Render for SettingsWindow {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let border = cx.theme().border;
        let content = match self.active_tab {
            SettingsTab::General => self.render_placeholder_tab("General", cx).into_any_element(),
            SettingsTab::Appearance => self.render_appearance_tab(cx).into_any_element(),
            SettingsTab::Terminal => self.render_placeholder_tab("Terminal", cx).into_any_element(),
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
```

Note: `.when_some` requires `gpui::prelude::FluentBuilder` in scope — add it to this file's
`use gpui::{ .. }` list (`prelude::FluentBuilder`), matching how `workspace.rs` already
imports it. `gpui::red()` (`crates/gpui/src/color.rs:484`) and `gpui::transparent_black()`
(`crates/gpui/src/color.rs:444`) are both confirmed free functions returning `Hsla` in the
pinned gpui rev — add both to the same `use gpui::{ .. }` list.

- [ ] **Step 5: Build to confirm it compiles**

Run: `cargo build 2>&1 | tail -60`
Expected: builds successfully. Fix any import-path or API-shape mismatches against the
actual vendored `gpui`/`gpui-component` source before proceeding — the code above was
written against the same APIs already used elsewhere in this codebase
(`saved_connections.rs`'s `field`/`pill` idiom, `sftp.rs`'s `Input`/`InputState` usage,
`main.rs`'s `open_window`/`Root::new`/`Theme::change` usage), but exact method names should
be verified against the pinned `gpui`/`gpui-component` git revs in `Cargo.toml` if anything
doesn't compile.

- [ ] **Step 6: Run the full test suite**

Run: `cargo test 2>&1 | tail -15`
Expected: all tests still pass (47 from Task 1 + the 3 new `settings_window` tests already
counted in Task 1's total — confirm the count matches: 47 total, 0 failed).

- [ ] **Step 7: Commit**

```bash
git add src/panels/settings_window.rs src/panels/mod.rs src/workspace.rs
git commit -m "feat: add SettingsWindow (standalone window, draft/Apply/Confirm/Cancel)"
```

---

### Task 4: Trigger — "设置..." in the File menu

**Files:**
- Modify: `src/panels/header.rs`

**Interfaces:**
- Consumes: `Workspace::open_settings` (Task 3).

- [ ] **Step 1: Add the menu item**

Current state (`src/panels/header.rs`):

```rust
    let file_menu = Button::new("menu-file")
        .ghost()
        .xsmall()
        .label("文件")
        .dropdown_menu(move |menu, _window, _cx| menu.item(new_local_item(ws_file.clone())));
```

Replace with:

```rust
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
```

(`ws_settings` is cloned once outside the `dropdown_menu` closure — matching how
`view_menu`/`terminal_menu` already clone their own `ws_*` variables above this point in the
same function — and once more inside the inner `on_click` closure, since `dropdown_menu`'s
builder closure itself may run more than once across re-renders.)

- [ ] **Step 2: Build to confirm it compiles**

Run: `cargo build 2>&1 | tail -30`
Expected: builds successfully.

- [ ] **Step 3: Commit**

```bash
git add src/panels/header.rs
git commit -m "feat: add \"设置...\" to the File menu"
```

---

### Task 5: Startup theme from settings + persist Ctrl+K toggles; fix multi-window quit bug

**Files:**
- Modify: `src/main.rs`

**Interfaces:**
- Consumes: `settings::{load, save, AppSettings}` (Task 1).

This task also fixes a real bug this feature would otherwise introduce: `App::on_window_closed`
fires for **every** window closing, not just the main one. Today's handler
(`cx.on_window_closed(|cx, _window_id| cx.quit())`) ignores the window id and always quits —
meaning closing the *settings* window (once Task 3 exists) would quit the entire app. This
must be fixed as part of adding the second window, not left as a landmine.

- [ ] **Step 1: Load persisted theme at startup**

Current state (`src/main.rs`):

```rust
        // Apply dark theme by default — uses the built-in dark theme from
        // gpui-component (One Dark variant).
        Theme::change(ThemeMode::Dark, None, cx);
```

Replace with:

```rust
        // Apply the persisted theme (defaults to dark if no settings.toml
        // exists yet, or its theme_mode isn't "light") — uses the built-in
        // dark/light themes from gpui-component (One Dark / One Light).
        let startup_settings = settings::load();
        let startup_theme = if startup_settings.appearance.theme_mode == "light" {
            ThemeMode::Light
        } else {
            ThemeMode::Dark
        };
        Theme::change(startup_theme, None, cx);
```

- [ ] **Step 2: Persist the theme on every Ctrl+K toggle**

Current state:

```rust
        cx.on_action(|_action: &ToggleTheme, cx| {
            let next = if Theme::global(cx).mode.is_dark() {
                ThemeMode::Light
            } else {
                ThemeMode::Dark
            };
            Theme::change(next, None, cx);
        });
```

Replace with:

```rust
        cx.on_action(|_action: &ToggleTheme, cx| {
            let next = if Theme::global(cx).mode.is_dark() {
                ThemeMode::Light
            } else {
                ThemeMode::Dark
            };
            Theme::change(next, None, cx);

            let mut settings = settings::load();
            settings.appearance.theme_mode = if next.is_dark() { "dark" } else { "light" }.to_string();
            if let Err(e) = settings::save(&settings) {
                log::error!("failed to persist theme toggle: {e}");
            }
        });
```

- [ ] **Step 3: Fix `on_window_closed` to only quit when the *main* window closes**

Current state:

```rust
        let bounds = Bounds::centered(None, size(px(900.0), px(600.0)), cx);
        cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                focus: true,
                ..Default::default()
            },
            |window, cx| {
                let workspace = cx.new(|cx| Workspace::new(window, cx));
                // The window's top-level view must be a gpui-component `Root`
                // (provides theme context, overlays, notifications).
                cx.new(|cx| Root::new(workspace, window, cx).bg(cx.theme().background))
            },
        )
        .expect("failed to open window");

        cx.on_window_closed(|cx, _window_id| cx.quit()).detach();
        cx.activate(true);
```

Replace with:

```rust
        let bounds = Bounds::centered(None, size(px(900.0), px(600.0)), cx);
        let main_window = cx
            .open_window(
                WindowOptions {
                    window_bounds: Some(WindowBounds::Windowed(bounds)),
                    focus: true,
                    ..Default::default()
                },
                |window, cx| {
                    let workspace = cx.new(|cx| Workspace::new(window, cx));
                    // The window's top-level view must be a gpui-component `Root`
                    // (provides theme context, overlays, notifications).
                    cx.new(|cx| Root::new(workspace, window, cx).bg(cx.theme().background))
                },
            )
            .expect("failed to open window");

        // `on_window_closed` fires for every window (including the settings
        // window opened by `Workspace::open_settings`), not just this one —
        // only quit the app when the *main* window is the one that closed.
        let main_window_id = main_window.window_id();
        cx.on_window_closed(move |cx, window_id| {
            if window_id == main_window_id {
                cx.quit();
            }
        })
        .detach();
        cx.activate(true);
```

- [ ] **Step 4: Build to confirm it compiles**

Run: `cargo build 2>&1 | tail -30`
Expected: builds successfully.

- [ ] **Step 5: Run the full test suite**

Run: `cargo test 2>&1 | tail -15`
Expected: all tests still pass.

- [ ] **Step 6: Commit**

```bash
git add src/main.rs
git commit -m "fix: persist theme + only quit app when the main window closes"
```

---

### Task 6: Build verification and manual smoke test

**Files:** none (verification only)

- [ ] **Step 1: Release build**

Run: `cargo build --release 2>&1 | tail -30`
Expected: builds successfully.

- [ ] **Step 2: Manual smoke test**

If a display is available in this environment, run `cargo run --release` and verify by hand
(if no display is available, note that explicitly rather than claiming it was checked):

1. Open two terminal tabs (e.g. "新建本地终端" twice from the File or Terminal menu).
2. File menu → "设置..." — a second window opens titled with the default OS chrome, showing
   General / Appearance / Terminal tabs on the left, Appearance active by default.
3. Change the font-family field to a different installed monospace font name and the
   font-size field to a different number (e.g. 18); click 应用 (Apply). Confirm **both**
   already-open tabs re-render with the new font/size without needing to reopen them.
4. Click a theme pill (深色/浅色) different from the current one, click 应用. Confirm the
   whole app's theme switches live.
5. Close the settings window via its OS close button (not 确定). Confirm the **main app
   does not quit** (this is the bug fixed in Task 5, Step 3 — verify it explicitly).
6. Re-open File → 设置..., confirm the previously-applied font/theme are shown as the
   current values (proving `settings.toml` round-trips).
7. Try an invalid font size (e.g. "abc" or "500"), click 应用 — confirm an inline error
   message appears and nothing crashes.
8. Quit and relaunch the app entirely. Confirm the theme from step 4 is still in effect
   (proves the Task 5 startup-load path works, not just the in-session apply path).

- [ ] **Step 3: Report results**

Summarize which of the 8 manual checks passed, and paste the full text of any that didn't,
before considering this task/plan complete.
