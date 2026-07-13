# Appearance/Terminal Font Pickers Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add primary+fallback font dropdown pickers to both the Appearance
and Terminal tabs in Settings (5 dropdowns total, one shared 4-choice list),
with Terminal's fallback chain and Appearance's chrome font both becoming
fully settings-driven instead of hardcoded.

**Architecture:** `TerminalSettings` gains `font_fallback1`/`font_fallback2`;
a new `AppearanceSettings.font_family`/`font_fallback` mirrors that shape.
`TerminalView` gains a `set_font_fallbacks` setter alongside its existing
`set_font_family`/`set_font_size`, both now driven straight from settings
(the old `Workspace::resolved_font_family` pre-resolution is retired).
`Workspace` gains two new fields for the resolved (never-empty) chrome font,
seeded at startup and re-resolved on Settings → Apply, applied via an
explicit `.font(...)` override on `Workspace::render()`'s own top-level
element — overriding `gpui-component`'s single-family `Theme.font_family`
without patching that vendored crate. `settings_window.rs` gets one reusable
`font_picker` dropdown (built from the existing `DropdownButton`/`PopupMenu`
components) driving all 5 fields through a small `FontSlot` field-selector
enum.

**Tech Stack:** Rust, `gpui` (`Font`, `FontFallbacks`, the `font()` builder),
`gpui-component` (`button::{Button, DropdownButton}`,
`menu::{PopupMenu, PopupMenuItem}` — already used elsewhere in this codebase,
no new widget type), `serde`/`toml` (settings persistence).

## Global Constraints

- Full spec: `docs/superpowers/specs/2026-07-13-appearance-terminal-font-pickers-design.md`.
- All 5 font fields share the exact same 4 choices: `""` (系统默认),
  `"JetBrains Mono"`, `"Sarasa Mono SC"`, `"Symbols Nerd Font"` — no free-text
  entry anywhere, including Terminal's primary field (today free-text; this
  plan converts it to the same dropdown).
- Terminal's fallback chain stays **two independent, ordered** slots
  (`font_fallback1` tried before `font_fallback2`) — not a single merged
  fallback setting.
- **Behavior change, deliberate:** an empty Terminal primary font now
  triggers real `system_monospace_family()` detection (previously it
  resolved to the bundled `"JetBrains Mono"` default via the
  now-deleted `Workspace::resolved_font_family`).
- Appearance's fallback mechanism must not touch the vendored
  `gpui-component` crate — implemented via `Workspace`'s own `.font(...)`
  override instead (see Task 3).
- `system_ui_font_family()`/`system_monospace_family()` (OS-detection,
  subprocess-based) must be resolved **eagerly** (at seed/apply time), never
  from inside `Render::render()` — resolving on every render would spawn a
  subprocess every frame.

---

### Task 1: Settings data model (`src/settings.rs`)

**Files:**
- Modify: `src/settings.rs`

**Interfaces:**
- Produces: `TerminalSettings.font_fallback1: String` (default
  `"Symbols Nerd Font"`), `TerminalSettings.font_fallback2: String` (default
  `"Sarasa Mono SC"`), `AppearanceSettings.font_family: String` (default
  `""`), `AppearanceSettings.font_fallback: String` (default
  `"Sarasa Mono SC"`) — consumed by Task 2 (Terminal application), Task 3
  (Appearance application), and Task 4 (settings UI).

- [ ] **Step 1: Write the failing tests**

In `src/settings.rs`, replace the `tests` module (current content is lines
136-243) with:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_settings_have_expected_values() {
        let settings = AppSettings::default();
        assert_eq!(settings.terminal.font_family, "");
        assert_eq!(settings.terminal.font_size, 14.0);
        assert_eq!(settings.terminal.scrollback_lines, 10_000);
        assert_eq!(settings.terminal.font_fallback1, "Symbols Nerd Font");
        assert_eq!(settings.terminal.font_fallback2, "Sarasa Mono SC");
        assert_eq!(settings.appearance.theme_mode, "dark");
        assert_eq!(settings.appearance.font_family, "");
        assert_eq!(settings.appearance.font_fallback, "Sarasa Mono SC");
    }

    #[test]
    fn round_trip_preserves_fields() {
        let settings = AppSettings {
            appearance: AppearanceSettings {
                theme_mode: "light".to_string(),
                font_family: "JetBrains Mono".to_string(),
                font_fallback: "Symbols Nerd Font".to_string(),
            },
            terminal: TerminalSettings {
                font_family: "Consolas".to_string(),
                font_size: 16.0,
                monitor_basic_enabled: true,
                monitor_basic_interval_secs: 10,
                scrollback_lines: 20_000,
                font_fallback1: "JetBrains Mono".to_string(),
                font_fallback2: "Symbols Nerd Font".to_string(),
            },
        };
        let text = toml::to_string_pretty(&settings).expect("serialize");
        let parsed: AppSettings = toml::from_str(&text).expect("deserialize");
        assert_eq!(parsed.terminal.font_family, "Consolas");
        assert_eq!(parsed.terminal.font_size, 16.0);
        assert!(parsed.terminal.monitor_basic_enabled);
        assert_eq!(parsed.terminal.monitor_basic_interval_secs, 10);
        assert_eq!(parsed.terminal.scrollback_lines, 20_000);
        assert_eq!(parsed.terminal.font_fallback1, "JetBrains Mono");
        assert_eq!(parsed.terminal.font_fallback2, "Symbols Nerd Font");
        assert_eq!(parsed.appearance.theme_mode, "light");
        assert_eq!(parsed.appearance.font_family, "JetBrains Mono");
        assert_eq!(parsed.appearance.font_fallback, "Symbols Nerd Font");
    }

    #[test]
    fn partial_toml_still_deserializes_with_defaults() {
        // Simulates a settings.toml written before a future field is added:
        // empty [appearance]/[terminal] tables should still fill in every
        // default.
        let toml_text = "[appearance]\n[terminal]\n";
        let settings: AppSettings =
            toml::from_str(toml_text).expect("partial settings must still parse");
        assert_eq!(settings.terminal.font_family, "");
        assert_eq!(settings.terminal.font_size, 14.0);
        assert_eq!(settings.terminal.scrollback_lines, 10_000);
        assert_eq!(settings.appearance.theme_mode, "dark");
        assert_eq!(settings.appearance.font_family, "");
        assert_eq!(settings.appearance.font_fallback, "Sarasa Mono SC");
    }

    #[test]
    fn empty_file_yields_default_appearance() {
        let settings: AppSettings = toml::from_str("").expect("empty file must still parse");
        assert_eq!(settings.terminal.font_size, 14.0);
    }

    #[test]
    fn old_settings_file_without_terminal_table_still_deserializes() {
        // Simulates a settings.toml written by the pre-split version of this
        // module, where font lived under [appearance]. The old font keys are
        // simply dropped (not migrated) — this only proves the file doesn't
        // fail to parse and the new [terminal] section falls back to
        // defaults, matching `AppSettings`'s top-level `#[serde(default)]`.
        let toml_text = r#"
            [appearance]
            font_family = "Consolas"
            font_size = 16.0
            theme_mode = "light"
        "#;
        let settings: AppSettings =
            toml::from_str(toml_text).expect("old-format settings must still parse");
        assert_eq!(settings.appearance.theme_mode, "light");
        assert_eq!(settings.terminal.font_family, "");
        assert_eq!(settings.terminal.font_size, 14.0);
    }

    #[test]
    fn old_settings_file_without_monitor_fields_still_deserializes() {
        // Simulates a settings.toml written before this round: a [terminal]
        // table with only the font keys, no monitor_basic_* keys at all.
        let toml_text = r#"
            [terminal]
            font_family = "Consolas"
            font_size = 16.0
        "#;
        let settings: AppSettings =
            toml::from_str(toml_text).expect("old-format settings must still parse");
        assert!(!settings.terminal.monitor_basic_enabled);
        assert_eq!(settings.terminal.monitor_basic_interval_secs, 5);
    }

    #[test]
    fn old_settings_file_without_scrollback_lines_still_deserializes() {
        // Simulates a settings.toml written before this field existed: a
        // [terminal] table with font + monitor keys, no scrollback_lines key.
        let toml_text = r#"
            [terminal]
            font_family = "Consolas"
            font_size = 16.0
            monitor_basic_enabled = true
            monitor_basic_interval_secs = 10
        "#;
        let settings: AppSettings =
            toml::from_str(toml_text).expect("old-format settings must still parse");
        assert_eq!(settings.terminal.scrollback_lines, 10_000);
    }

    #[test]
    fn old_settings_file_without_font_fallback_fields_still_deserializes() {
        // Simulates a settings.toml written before font_fallback1/2 and
        // appearance's font fields existed.
        let toml_text = r#"
            [appearance]
            theme_mode = "light"

            [terminal]
            font_family = "Consolas"
            font_size = 16.0
            scrollback_lines = 5000
        "#;
        let settings: AppSettings =
            toml::from_str(toml_text).expect("old-format settings must still parse");
        assert_eq!(settings.terminal.font_fallback1, "Symbols Nerd Font");
        assert_eq!(settings.terminal.font_fallback2, "Sarasa Mono SC");
        assert_eq!(settings.appearance.font_family, "");
        assert_eq!(settings.appearance.font_fallback, "Sarasa Mono SC");
    }
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test settings:: 2>&1 | tail -60`
Expected: FAIL to compile — `TerminalSettings`/`AppearanceSettings` have no
such fields yet.

- [ ] **Step 3: Add the fields and defaults**

In `src/settings.rs`, current `AppearanceSettings` (lines 24-29):

```rust
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AppearanceSettings {
    /// `"dark"` | `"light"`.
    #[serde(default = "default_theme_mode")]
    pub theme_mode: String,
}
```

Replace with:

```rust
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AppearanceSettings {
    /// `"dark"` | `"light"`.
    #[serde(default = "default_theme_mode")]
    pub theme_mode: String,
    /// Primary application-chrome font. Empty string = detect a system UI
    /// font at startup/apply time (`Workspace::system_ui_font_family`) — see
    /// the design spec for why chrome text needs its own resolution
    /// separate from the terminal's `system_monospace_family`.
    #[serde(default)]
    pub font_family: String,
    /// Fallback chrome font, consulted for glyphs `font_family` lacks.
    #[serde(default = "default_appearance_font_fallback")]
    pub font_fallback: String,
}
```

Current `TerminalSettings` (lines 32-59):

```rust
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TerminalSettings {
    /// Empty string = bundled default (`terminal::view`'s `DEFAULT_FONT_FAMILY`).
    #[serde(default)]
    pub font_family: String,
    /// Raw point size; `TerminalView::set_font_size` takes `gpui::Pixels`, so
    /// callers convert via `px(settings.terminal.font_size)` — `Pixels`
    /// itself isn't (de)serializable, hence the raw `f32` here.
    #[serde(default = "default_font_size")]
    pub font_size: f32,
    /// Whether the 资源监控 (basic system stats) panel polls the remote
    /// host at all. Off by default — matches nyaterm's own "all off by
    /// default" convention for remote-monitoring panels.
    #[serde(default)]
    pub monitor_basic_enabled: bool,
    /// Poll interval in seconds. Read once when a `MonitorPanel` is
    /// created; changing this in Settings takes effect for panels created
    /// afterward (not a live-reload of already-open panels).
    #[serde(default = "default_monitor_interval_secs")]
    pub monitor_basic_interval_secs: u32,
    /// Scrollback capacity in lines, passed to alacritty's
    /// `Config::scrolling_history` when a new terminal tab is constructed
    /// (`terminal::model::new_term`). Read once per tab; changing this in
    /// Settings only affects tabs opened afterward, never an already-open
    /// tab's grid.
    #[serde(default = "default_scrollback_lines")]
    pub scrollback_lines: u32,
}
```

Replace with:

```rust
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TerminalSettings {
    /// Empty string = detect the system monospace font at seed/apply time
    /// (`terminal::view::system_monospace_family`, via
    /// `TerminalView::set_font_family`).
    #[serde(default)]
    pub font_family: String,
    /// Raw point size; `TerminalView::set_font_size` takes `gpui::Pixels`, so
    /// callers convert via `px(settings.terminal.font_size)` — `Pixels`
    /// itself isn't (de)serializable, hence the raw `f32` here.
    #[serde(default = "default_font_size")]
    pub font_size: f32,
    /// Whether the 资源监控 (basic system stats) panel polls the remote
    /// host at all. Off by default — matches nyaterm's own "all off by
    /// default" convention for remote-monitoring panels.
    #[serde(default)]
    pub monitor_basic_enabled: bool,
    /// Poll interval in seconds. Read once when a `MonitorPanel` is
    /// created; changing this in Settings takes effect for panels created
    /// afterward (not a live-reload of already-open panels).
    #[serde(default = "default_monitor_interval_secs")]
    pub monitor_basic_interval_secs: u32,
    /// Scrollback capacity in lines, passed to alacritty's
    /// `Config::scrolling_history` when a new terminal tab is constructed
    /// (`terminal::model::new_term`). Read once per tab; changing this in
    /// Settings only affects tabs opened afterward, never an already-open
    /// tab's grid.
    #[serde(default = "default_scrollback_lines")]
    pub scrollback_lines: u32,
    /// First fallback font (tried before `font_fallback2`), consulted for
    /// glyphs `font_family` lacks — the icon/powerline glyph slot.
    #[serde(default = "default_font_fallback1")]
    pub font_fallback1: String,
    /// Second fallback font, consulted after `font_fallback1` — the CJK
    /// glyph slot.
    #[serde(default = "default_font_fallback2")]
    pub font_fallback2: String,
}
```

Add the default functions near `default_scrollback_lines` (lines 69-71):

```rust
fn default_font_fallback1() -> String {
    "Symbols Nerd Font".to_string()
}

fn default_font_fallback2() -> String {
    "Sarasa Mono SC".to_string()
}

fn default_appearance_font_fallback() -> String {
    "Sarasa Mono SC".to_string()
}
```

Update `impl Default for AppearanceSettings` (lines 77-83):

```rust
impl Default for AppearanceSettings {
    fn default() -> Self {
        Self {
            theme_mode: default_theme_mode(),
            font_family: String::new(),
            font_fallback: default_appearance_font_fallback(),
        }
    }
}
```

Update `impl Default for TerminalSettings` (lines 85-95):

```rust
impl Default for TerminalSettings {
    fn default() -> Self {
        Self {
            font_family: String::new(),
            font_size: default_font_size(),
            monitor_basic_enabled: false,
            monitor_basic_interval_secs: default_monitor_interval_secs(),
            scrollback_lines: default_scrollback_lines(),
            font_fallback1: default_font_fallback1(),
            font_fallback2: default_font_fallback2(),
        }
    }
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test settings:: 2>&1 | tail -60`
Expected: PASS (8 tests).

- [ ] **Step 5: Commit**

```bash
git add src/settings.rs
git commit -m "feat: add font_fallback1/2 and appearance font settings"
```

---

### Task 2: Terminal fallback application (`src/terminal/view.rs`, `src/workspace.rs`)

**Files:**
- Modify: `src/terminal/view.rs`
- Modify: `src/workspace.rs`
- Modify: `src/panels/settings_window.rs`

**Interfaces:**
- Consumes: `TerminalSettings.font_fallback1`/`font_fallback2` (Task 1).
- Produces: `TerminalView::set_font_fallbacks(&mut self, fallbacks:
  Vec<SharedString>, cx: &mut Context<Self>)`; `pub(crate) const
  DEFAULT_FONT_FAMILY`/`CJK_FALLBACK`/`SYMBOL_FALLBACK` (visibility widened,
  consumed by Task 4's `FONT_CHOICES` list). `Workspace::apply_font_settings`
  gains 2 new `String` parameters (`font_fallback1`, `font_fallback2`) —
  its one call site, in `settings_window.rs`'s `apply()`, is fixed in this
  same task (Step 6), so the build stays green at every commit; Task 4 does
  not touch this call site again.

- [ ] **Step 1: Write the failing test**

In `src/terminal/view.rs`, in the `tests` module (current content starts at
line 1005), add after `to_font_carries_fallback_chain` (ends at line 1030):

```rust
    #[test]
    fn set_font_fallbacks_replaces_chain_in_order() {
        let mut config = FontConfig::default();
        // Directly exercise the struct + to_font(), matching how the other
        // FontConfig tests in this module already work (TerminalView itself
        // needs a live gpui window to construct).
        config.fallbacks = vec!["Fallback One".into(), "Fallback Two".into()];
        let font = config.to_font();
        let fallbacks = font.fallbacks.expect("fallbacks should be set");
        assert_eq!(
            fallbacks.fallback_list(),
            &["Fallback One".to_string(), "Fallback Two".to_string()]
        );
    }
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test terminal::view:: 2>&1 | tail -40`
Expected: PASS already (this test only exercises the existing `FontConfig`/
`to_font()` plumbing, not `set_font_fallbacks` itself, which lives on
`TerminalView` and needs a live gpui window to construct — untestable
without one, consistent with every other `TerminalView` method in this
file). This step is a sanity check that `fallback_list()` (gpui's public
accessor on `FontFallbacks`, `crates/gpui/src/text_system/font_fallbacks.rs`)
is spelled correctly and the ordering assertion holds — it is not expected
to fail first per strict TDD, since it doesn't touch new production code.
If it fails to compile, `fallback_list()` is the wrong method name — re-check
`FontFallbacks`'s public API in the gpui checkout before proceeding.

- [ ] **Step 3: Implement `set_font_fallbacks`**

In `src/terminal/view.rs`, right after `set_font_family` (current, lines
628-638):

```rust
    /// Set the primary font family (empty string = system monospace).
    #[allow(dead_code)]
    pub fn set_font_family(&mut self, family: impl Into<SharedString>, cx: &mut Context<Self>) {
        let family = family.into();
        self.font_config.family = if family.is_empty() {
            system_monospace_family()
        } else {
            family
        };
        cx.notify();
    }
```

Add:

```rust
    /// Replace the fallback chain, in order. Empty entries resolve to the
    /// system monospace font — same "" convention `set_font_family` already
    /// uses — so a settings value of `""` for either fallback slot means
    /// "detect a system font here too", not "no fallback".
    #[allow(dead_code)]
    pub fn set_font_fallbacks(&mut self, fallbacks: Vec<SharedString>, cx: &mut Context<Self>) {
        self.font_config.fallbacks = fallbacks
            .into_iter()
            .map(|f| if f.is_empty() { system_monospace_family() } else { f })
            .collect();
        cx.notify();
    }
```

Widen visibility of the three constants (current, lines 51-64):

```rust
const SYMBOL_FALLBACK: &str = "Symbols Nerd Font";
...
const CJK_FALLBACK: &str = "Sarasa Mono SC";
...
const DEFAULT_FONT_FAMILY: &str = "JetBrains Mono";
```

Replace each `const` with `pub(crate) const` (keep the doc comments above
each unchanged):

```rust
pub(crate) const SYMBOL_FALLBACK: &str = "Symbols Nerd Font";
```
```rust
pub(crate) const CJK_FALLBACK: &str = "Sarasa Mono SC";
```
```rust
pub(crate) const DEFAULT_FONT_FAMILY: &str = "JetBrains Mono";
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test terminal::view:: 2>&1 | tail -40`
Expected: PASS.

- [ ] **Step 5: Wire settings through `Workspace`**

In `src/workspace.rs`, current `resolved_font_family`/`seed_font_from_settings`/
`apply_font_settings` (lines 589-626):

```rust
    fn resolved_font_family(raw: &str) -> String {
        if raw.is_empty() {
            FontConfig::default().family.to_string()
        } else {
            raw.to_string()
        }
    }

    /// Seed a newly-created terminal's font from persisted settings, so a new
    /// tab picks up whatever was last applied via Settings → Terminal instead
    /// of always starting at the compiled-in default.
    fn seed_font_from_settings(terminal: &Entity<TerminalView>, cx: &mut Context<Self>) {
        let loaded = settings::load();
        let family = Self::resolved_font_family(&loaded.terminal.font_family);
        terminal.update(cx, |view, cx| {
            view.set_font_family(family, cx);
            view.set_font_size(px(loaded.terminal.font_size), cx);
        });
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
        let font_family = Self::resolved_font_family(&font_family);
        self.terminal_views.retain(|weak| {
            weak.update(cx, |view, cx| {
                view.set_font_family(font_family.clone(), cx);
                view.set_font_size(font_size, cx);
            })
            .is_ok()
        });
    }
```

Replace with (note `resolved_font_family` is deleted — `set_font_family`/
`set_font_fallbacks` now do their own empty-string resolution internally):

```rust
    /// Seed a newly-created terminal's font from persisted settings, so a new
    /// tab picks up whatever was last applied via Settings → Terminal instead
    /// of always starting at the compiled-in default.
    fn seed_font_from_settings(terminal: &Entity<TerminalView>, cx: &mut Context<Self>) {
        let loaded = settings::load();
        terminal.update(cx, |view, cx| {
            view.set_font_family(loaded.terminal.font_family.clone(), cx);
            view.set_font_size(px(loaded.terminal.font_size), cx);
            view.set_font_fallbacks(
                vec![
                    loaded.terminal.font_fallback1.clone().into(),
                    loaded.terminal.font_fallback2.clone().into(),
                ],
                cx,
            );
        });
    }

    /// Broadcast a new font family/size/fallback chain to every currently-open
    /// terminal tab, pruning any that have since closed. Called by
    /// [`SettingsWindow`] on Apply/Confirm.
    pub fn apply_font_settings(
        &mut self,
        font_family: String,
        font_size: Pixels,
        font_fallback1: String,
        font_fallback2: String,
        cx: &mut Context<Self>,
    ) {
        self.terminal_views.retain(|weak| {
            weak.update(cx, |view, cx| {
                view.set_font_family(font_family.clone(), cx);
                view.set_font_size(font_size, cx);
                view.set_font_fallbacks(
                    vec![font_fallback1.clone().into(), font_fallback2.clone().into()],
                    cx,
                );
            })
            .is_ok()
        });
    }
```

Delete the two tests that exercised the now-removed `resolved_font_family`,
in the `tests` module at the bottom of `src/workspace.rs` (current, lines
1021-1029):

```rust
    #[test]
    fn resolved_font_family_empty_uses_bundled_default() {
        assert_eq!(Workspace::resolved_font_family(""), "JetBrains Mono");
    }

    #[test]
    fn resolved_font_family_passes_through_explicit_value() {
        assert_eq!(Workspace::resolved_font_family("Consolas"), "Consolas");
    }
```

Delete both — leave the `tests` module's surrounding braces (`mod tests { use
super::*; ... }`) intact with nothing else in between (Task 3 adds new tests
to this same module).

- [ ] **Step 6: Fix `apply_font_settings`'s one call site**

`apply_font_settings`'s signature changed, so its one call site (in
`settings_window.rs`'s `apply()`) needs the two new arguments now. The
fallback *values* already exist as of Task 1 (`self.draft.terminal
.font_fallback1`/`font_fallback2`) even though the dropdown UI to edit them
doesn't exist until Task 4 — so this fixes the call site completely, with no
placeholder values.

In `src/panels/settings_window.rs`, current (inside `apply()`):

```rust
        let font_family = self.draft.terminal.font_family.clone();
        let font_size = px(self.draft.terminal.font_size);
        let _ = self.workspace.update(cx, |workspace, cx| {
            workspace.apply_font_settings(font_family, font_size, cx);
        });
```

Replace with:

```rust
        let font_family = self.draft.terminal.font_family.clone();
        let font_size = px(self.draft.terminal.font_size);
        let font_fallback1 = self.draft.terminal.font_fallback1.clone();
        let font_fallback2 = self.draft.terminal.font_fallback2.clone();
        let _ = self.workspace.update(cx, |workspace, cx| {
            workspace.apply_font_settings(font_family, font_size, font_fallback1, font_fallback2, cx);
        });
```

- [ ] **Step 7: Build and run the full test suite**

Run: `cargo build 2>&1 | tail -60`
Expected: builds successfully.

Run: `cargo test 2>&1 | tail -100`
Expected: PASS — full suite, including the two deleted
`resolved_font_family` tests no longer present and no new failures.

- [ ] **Step 8: Commit**

```bash
git add src/terminal/view.rs src/workspace.rs src/panels/settings_window.rs
git commit -m "feat: make terminal font fallback chain settings-driven"
```

---

### Task 3: Appearance font application (`src/workspace.rs`, `src/panels/settings_window.rs`)

**Files:**
- Modify: `src/workspace.rs`
- Modify: `src/panels/settings_window.rs`

**Interfaces:**
- Consumes: `AppearanceSettings.font_family`/`font_fallback` (Task 1).
- Produces: `Workspace.appearance_font_family: SharedString`,
  `Workspace.appearance_font_fallback: SharedString` (new fields);
  `Workspace::apply_appearance_font_settings(&mut self, font_family: String,
  font_fallback: String, cx: &mut Context<Self>)` (called from `apply()` in
  this same task — nothing later depends on this signature).

- [ ] **Step 1: Write the failing test**

In `src/workspace.rs`'s `tests` module (after Task 2 removed the two
`resolved_font_family` tests, it now only contains `use super::*;`), add:

```rust
    #[test]
    fn resolve_appearance_font_passes_through_explicit_value() {
        assert_eq!(Workspace::resolve_appearance_font("Consolas"), "Consolas".to_string());
    }
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test workspace:: 2>&1 | tail -40`
Expected: FAIL to compile — `resolve_appearance_font` not found.

- [ ] **Step 3: Add `system_ui_font_family`, `resolve_appearance_font`, and `apply_appearance_font_settings`**

In `src/workspace.rs`, right where `resolved_font_family` used to be (deleted
in Task 2, roughly lines 585-595), add:

```rust
    /// The system's default UI-chrome font family, used when Appearance's
    /// primary/fallback font is left at "系统默认" (empty string). Mirrors
    /// `terminal::view`'s `system_monospace_family()` but resolves a UI font,
    /// not monospace — chrome text (menus, buttons, panels) reading in a
    /// monospace font looks unusually rigid.
    fn system_ui_font_family() -> SharedString {
        #[cfg(target_os = "linux")]
        {
            if let Ok(out) = std::process::Command::new("fc-match")
                .args(["-f", "%{family[0]}", "sans-serif"])
                .output()
                && out.status.success()
            {
                let name = String::from_utf8_lossy(&out.stdout).trim().to_string();
                if !name.is_empty() {
                    return name.into();
                }
            }
        }
        #[cfg(target_os = "windows")]
        {
            return "Segoe UI".into();
        }
        // macOS (and any detection failure above): gpui-component's own
        // default sentinel, which already resolves correctly there — only
        // Windows/Linux needed an override (see `main.rs`'s existing
        // `Theme::global_mut(cx).font_family` comment).
        #[allow(unreachable_code)]
        ".SystemUIFont".into()
    }

    /// Resolve an Appearance font-setting value: `""` (系统默认) becomes a
    /// detected system UI font; anything else passes through unchanged.
    /// Called only at startup (`Workspace::new`) and on Settings →
    /// Apply/Confirm (`apply_appearance_font_settings`) — never from
    /// `Render::render`, since this can spawn a subprocess on Linux.
    fn resolve_appearance_font(raw: &str) -> SharedString {
        if raw.is_empty() {
            Self::system_ui_font_family()
        } else {
            raw.into()
        }
    }

    /// Resolve and store a new Appearance primary/fallback font, then
    /// `cx.notify()` so `Render for Workspace`'s own `.font(...)` override
    /// (see that impl's doc comment) picks it up immediately — no restart
    /// required. Called by `SettingsWindow` on Apply/Confirm, alongside
    /// `apply_font_settings` (terminal) and `Theme::change` (theme mode).
    pub fn apply_appearance_font_settings(
        &mut self,
        font_family: String,
        font_fallback: String,
        cx: &mut Context<Self>,
    ) {
        self.appearance_font_family = Self::resolve_appearance_font(&font_family);
        self.appearance_font_fallback = Self::resolve_appearance_font(&font_fallback);
        cx.notify();
    }
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test workspace:: 2>&1 | tail -40`
Expected: PASS.

- [ ] **Step 5: Add the two new `Workspace` fields and seed them at startup**

In `src/workspace.rs`, current `Workspace` struct's tail (lines 119-128):

```rust
    /// Focused terminal's title, shown centered in the header.
    active_title: SharedString,
    /// Forwards the currently-focused terminal's `cx.notify()` into a
    /// `Workspace`-level `cx.notify()`, so the status bar's cursor-position
    /// readout repaints on new terminal output/cursor movement, not just on
    /// focus changes. Replaced (dropping the old subscription) every time
    /// `set_active_title_from` runs, so only the current focus is observed.
    _focused_terminal_observation: Option<Subscription>,
    _subscriptions: Vec<Subscription>,
}
```

Replace with:

```rust
    /// Focused terminal's title, shown centered in the header.
    active_title: SharedString,
    /// Forwards the currently-focused terminal's `cx.notify()` into a
    /// `Workspace`-level `cx.notify()`, so the status bar's cursor-position
    /// readout repaints on new terminal output/cursor movement, not just on
    /// focus changes. Replaced (dropping the old subscription) every time
    /// `set_active_title_from` runs, so only the current focus is observed.
    _focused_terminal_observation: Option<Subscription>,
    /// The application chrome's primary/fallback font, already resolved
    /// (never the raw `""` "系统默认" sentinel) — seeded from
    /// `AppSettings.appearance` in `Workspace::new`, re-resolved by
    /// `apply_appearance_font_settings` on Settings → Apply. Applied by
    /// `Render for Workspace` via an explicit `.font(...)` override on its
    /// own top-level element (see that impl's doc comment).
    appearance_font_family: SharedString,
    appearance_font_fallback: SharedString,
    _subscriptions: Vec<Subscription>,
}
```

In `Workspace::new` (current, lines 131-204), add the settings load + seeding
right after the `dock_area` line (current line 133):

```rust
        // Center-only dock: terminal tabs live here. No left/right docks.
        let dock_area = cx.new(|cx| DockArea::new("caracal-main", Some(1), window, cx));
```

Replace with:

```rust
        // Center-only dock: terminal tabs live here. No left/right docks.
        let dock_area = cx.new(|cx| DockArea::new("caracal-main", Some(1), window, cx));

        // Seed the chrome font (see `apply_appearance_font_settings`'s doc
        // comment) once at startup — resolved eagerly here, not on every
        // render.
        let startup_appearance = settings::load().appearance;
        let appearance_font_family = Self::resolve_appearance_font(&startup_appearance.font_family);
        let appearance_font_fallback =
            Self::resolve_appearance_font(&startup_appearance.font_fallback);
```

And add the two fields to the `Self { ... }` literal (current, lines
178-203), right after `_focused_terminal_observation: None,` (current line
185):

```rust
            _focused_terminal_observation: None,
```

Replace with:

```rust
            _focused_terminal_observation: None,
            appearance_font_family,
            appearance_font_fallback,
```

- [ ] **Step 6: Apply the chrome font override in `Render for Workspace`**

In `src/workspace.rs`, add `Font, FontFallbacks, font` to the existing `use
gpui::{...}` block (current, lines 28-33):

```rust
use gpui::{
    AnyView, App, AppContext, Axis, Bounds, Context, Entity, EntityId, Focusable,
    InteractiveElement, IntoElement, ParentElement, Pixels, Render, SharedString,
    StatefulInteractiveElement, Styled, Subscription, Task, WeakEntity, Window, WindowBounds,
    WindowHandle, WindowOptions, div, prelude::FluentBuilder, px, size,
};
```

Replace with:

```rust
use gpui::{
    AnyView, App, AppContext, Axis, Bounds, Context, Entity, EntityId, Focusable, Font,
    FontFallbacks, InteractiveElement, IntoElement, ParentElement, Pixels, Render, SharedString,
    StatefulInteractiveElement, Styled, Subscription, Task, WeakEntity, Window, WindowBounds,
    WindowHandle, WindowOptions, div, font, prelude::FluentBuilder, px, size,
};
```

`impl Render for Workspace`'s current opening (lines 974-989):

```rust
impl Render for Workspace {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let header = render_header(cx.entity().downgrade(), self.active_title.clone(), cx);
        let left_bar = self.render_activity_bar(Side::Left, cx);
        let right_bar = self.render_activity_bar(Side::Right, cx);
        let body = self.render_body(cx);
        let status_bar = self.render_status_bar(cx);
        let border = cx.theme().border;
        let show_quick_commands = self.show_quick_commands;
        let quick_commands_panel = self.quick_commands_panel.clone();

        div()
            .flex()
            .flex_col()
            .size_full()
            .child(header)
```

Replace with:

```rust
impl Render for Workspace {
    /// `gpui-component`'s `Root` (which wraps this `Workspace`, see
    /// `main.rs`) renders its own top-level `.font_family(cx.theme()
    /// .font_family.clone())` — a single family, no fallback field exists on
    /// `Theme` at all (`gpui-component/crates/ui/src/theme/mod.rs`). Rather
    /// than patch that vendored crate, this `div`'s own more specific
    /// `.font(...)` below overrides it for all of `Workspace`'s content
    /// (effectively the whole app UI) via GPUI's normal style cascade — a
    /// descendant's explicit font setting wins over an ancestor's.
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let header = render_header(cx.entity().downgrade(), self.active_title.clone(), cx);
        let left_bar = self.render_activity_bar(Side::Left, cx);
        let right_bar = self.render_activity_bar(Side::Right, cx);
        let body = self.render_body(cx);
        let status_bar = self.render_status_bar(cx);
        let border = cx.theme().border;
        let show_quick_commands = self.show_quick_commands;
        let quick_commands_panel = self.quick_commands_panel.clone();

        let mut chrome_font: Font = font(self.appearance_font_family.clone());
        chrome_font.fallbacks = Some(FontFallbacks::from_fonts(vec![
            self.appearance_font_fallback.to_string(),
        ]));

        div()
            .flex()
            .flex_col()
            .size_full()
            .font(chrome_font)
            .child(header)
```

- [ ] **Step 7: Wire `apply()` to call `apply_appearance_font_settings`**

In `src/panels/settings_window.rs`, current (inside `apply()`, right after
the block Task 2's Step 6 added):

```rust
        let font_family = self.draft.terminal.font_family.clone();
        let font_size = px(self.draft.terminal.font_size);
        let font_fallback1 = self.draft.terminal.font_fallback1.clone();
        let font_fallback2 = self.draft.terminal.font_fallback2.clone();
        let _ = self.workspace.update(cx, |workspace, cx| {
            workspace.apply_font_settings(font_family, font_size, font_fallback1, font_fallback2, cx);
        });
```

Replace with:

```rust
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
```

- [ ] **Step 8: Build and run the full test suite**

Run: `cargo build 2>&1 | tail -60`
Expected: builds successfully.

Run: `cargo test 2>&1 | tail -100`
Expected: PASS — full suite.

- [ ] **Step 9: Commit**

```bash
git add src/workspace.rs src/panels/settings_window.rs
git commit -m "feat: make appearance chrome font settings-driven"
```

(Same caveat as Task 2 — the build stays intentionally red until Task 4
fixes the `settings_window.rs` call site.)

---

### Task 4: Settings UI — font-picker dropdowns (`src/panels/settings_window.rs`)

**Files:**
- Modify: `src/panels/settings_window.rs`

**Interfaces:**
- Consumes: `TerminalSettings.font_fallback1`/`font_fallback2`,
  `AppearanceSettings.font_family`/`font_fallback` (Task 1);
  `crate::terminal::view::{DEFAULT_FONT_FAMILY, CJK_FALLBACK,
  SYMBOL_FALLBACK}` (Task 2, now `pub(crate)`). `apply()`'s calls into
  `Workspace::apply_font_settings`/`apply_appearance_font_settings` were
  already wired in Tasks 2-3 — this task only adds the dropdown UI that
  edits the `draft` fields those calls read.
- Produces: fully wired Settings UI — this is the last task; nothing beyond
  it depends on this file's new symbols.

- [ ] **Step 1: Add imports and the shared `FONT_CHOICES`/`FontSlot`**

In `src/panels/settings_window.rs`, current imports (lines 44-53):

```rust
use gpui::{
    App, AppContext, ClickEvent, Context, Entity, InteractiveElement, IntoElement, ParentElement,
    Render, SharedString, StatefulInteractiveElement, Styled, WeakEntity, Window,
    div, prelude::FluentBuilder, px, red, transparent_black,
};
use gpui_component::input::{Input, InputState};
use gpui_component::{ActiveTheme, Theme, ThemeMode};

use crate::settings::{self, AppSettings};
use crate::workspace::Workspace;
```

Replace with:

```rust
use gpui::{
    App, AppContext, ClickEvent, Context, Entity, InteractiveElement, IntoElement, ParentElement,
    Render, SharedString, StatefulInteractiveElement, Styled, WeakEntity, Window,
    div, prelude::FluentBuilder, px, red, transparent_black,
};
use gpui_component::button::{Button, DropdownButton};
use gpui_component::input::{Input, InputState};
use gpui_component::menu::PopupMenuItem;
use gpui_component::{ActiveTheme, Sizable, Theme, ThemeMode};

use crate::settings::{self, AppSettings};
use crate::terminal::view::{CJK_FALLBACK, DEFAULT_FONT_FAMILY, SYMBOL_FALLBACK};
use crate::workspace::Workspace;
```

Right after the existing `parse_scrollback_lines` fn (current, ends at line
42, right before the `use gpui::{...}` block), add:

```rust
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
```

- [ ] **Step 2: Add `font_slot_value`/`set_font_slot`/`font_picker` to `impl SettingsWindow`**

In `src/panels/settings_window.rs`, right after `toggle_monitor_enabled`
(current, lines 202-205):

```rust
    fn toggle_monitor_enabled(&mut self, cx: &mut Context<Self>) {
        self.draft.terminal.monitor_basic_enabled = !self.draft.terminal.monitor_basic_enabled;
        cx.notify();
    }
```

Add:

```rust
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
```

- [ ] **Step 3: Remove `font_family_input` (superseded by the dropdown)**

Struct definition (current, lines 72-82):

```rust
pub struct SettingsWindow {
    workspace: WeakEntity<Workspace>,
    committed: AppSettings,
    draft: AppSettings,
    active_tab: SettingsTab,
    font_family_input: Entity<InputState>,
    font_size_input: Entity<InputState>,
    monitor_interval_input: Entity<InputState>,
    scrollback_input: Entity<InputState>,
    error: Option<SharedString>,
}
```

Replace with:

```rust
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
```

`SettingsWindow::new` (current, lines 85-114):

```rust
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
            font_family_input,
            font_size_input,
            monitor_interval_input,
            scrollback_input,
            error: None,
        }
    }
```

Replace with:

```rust
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
```

`sync_inputs_to_draft` (current, lines 119-144):

```rust
    fn sync_inputs_to_draft(&mut self, cx: &App) -> bool {
        self.draft.terminal.font_family = self.font_family_input.read(cx).value().to_string();
        let size_text = self.font_size_input.read(cx).value();
```

Replace with:

```rust
    fn sync_inputs_to_draft(&mut self, cx: &App) -> bool {
        let size_text = self.font_size_input.read(cx).value();
```

(Everything else in that function — the font-size, monitor-interval, and
scrollback-lines validation blocks — is unchanged. Terminal's primary font
is no longer synced here at all: it's already live in `self.draft` via
`set_font_slot`, same as `theme_mode`/`monitor_basic_enabled`.)

**Note:** `apply()`'s font-broadcasting block already reached its final form
in Tasks 2 and 3 (it now calls both `apply_font_settings(..., font_fallback1,
font_fallback2, cx)` and `apply_appearance_font_settings(...)`) — nothing
left to change there in this task.

- [ ] **Step 4: Add the dropdowns to both tabs**

`render_appearance_tab` (current, lines 276-301):

```rust
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
```

Replace with:

```rust
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
            .child(self.font_picker("首选字体", FontSlot::AppearancePrimary, cx))
            .child(self.font_picker("备选字体", FontSlot::AppearanceFallback, cx))
    }
```

`render_terminal_tab` (current, lines 306-382) — replace the whole method:

```rust
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
                    .child(
                        div()
                            .flex()
                            .flex_row()
                            .gap_2()
                            .child(self.monitor_enabled_pill(cx)),
                    ),
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
```

Replace with:

```rust
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
                    .child(
                        div()
                            .flex()
                            .flex_row()
                            .gap_2()
                            .child(self.monitor_enabled_pill(cx)),
                    ),
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
```

- [ ] **Step 5: Build and run the full test suite**

Run: `cargo build 2>&1 | tail -80`
Expected: builds successfully.

Run: `cargo test 2>&1 | tail -100`
Expected: PASS — full suite (settings, terminal::view, workspace,
settings_window, everything else) all green.

- [ ] **Step 6: Commit**

```bash
git add src/panels/settings_window.rs
git commit -m "feat: add primary/fallback font dropdowns to Appearance and Terminal settings"
```

---

### Task 5: Build verification and manual smoke test

**Files:** none (verification only)

- [ ] **Step 1: Release build**

Run: `cargo build --release 2>&1 | tail -40`
Expected: builds successfully.

- [ ] **Step 2: Manual smoke test**

If you can safely drive a GUI window in this environment, run `cargo run
--release` and verify by hand (if not, note that explicitly rather than
claiming it was checked):

1. Open Settings → Terminal. Confirm three dropdowns — "首选字体" (showing
   "JetBrains Mono"), "备选字体 1" (showing "Symbols Nerd Font"), "备选字体 2"
   (showing "Sarasa Mono SC") — each open a menu with all 4 choices when
   clicked.
2. Open Settings → Appearance. Confirm two dropdowns — "首选字体" (showing
   "系统默认") and "备选字体" (showing "Sarasa Mono SC") — below the existing
   深色/浅色 theme pills.
3. Change Terminal's "首选字体" to "Sarasa Mono SC", click 应用. Open a new
   local terminal tab; confirm its text renders in Sarasa Mono SC, not
   JetBrains Mono.
4. Change Terminal's "首选字体" to "系统默认", click 应用. Open another new
   terminal tab; confirm it renders in a real detected system monospace font
   (not JetBrains Mono, not a fallback-looking placeholder).
5. Change Appearance's "首选字体" to "JetBrains Mono", click 应用. Confirm the
   app's own chrome — menu bar text, sidebar labels, settings window text
   itself — visibly switches to a monospace look, without restarting the app.
6. Change it back to "系统默认", click 应用; confirm chrome text returns to a
   normal (non-monospace) look.
7. Click 取消 after making unsaved picks in either tab; reopen Settings;
   confirm the unsaved picks were discarded (dropdowns show the previously
   *applied* values, not the discarded ones).
8. Quit and relaunch the app; reopen Settings; confirm all 5 dropdowns still
   show whatever was last applied (proves `settings.toml` round-trips these
   fields).

- [ ] **Step 3: Report results**

Summarize which of the 8 manual checks passed, and paste the full text of
any that didn't, before considering this task/plan complete.
