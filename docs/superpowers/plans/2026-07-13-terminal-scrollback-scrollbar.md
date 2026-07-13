# Configurable Scrollback + Terminal Scrollbar Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add a "scrollback lines" option to the Terminal settings tab (default
10,000, range 1,000–50,000) that sets the alacritty scrollback capacity for
newly opened terminal tabs, and add a right-side scrollbar to the terminal view
that shows/drags through that scrollback.

**Architecture:** `TerminalSettings` gains a persisted `scrollback_lines: u32`
field; the settings window gains a fourth input on the Terminal tab. `new_term()`
takes that value instead of a hardcoded `10_000`, read fresh from disk at the one
place a `Term` is constructed (`TerminalView::base_setup`) — so only tabs opened
after a settings change pick up the new capacity, never already-open ones.
The scrollbar is a `gpui-component` `Scrollbar` element driven by a small adapter,
`TerminalScrollbarHandle`, that translates alacritty's line-based
`display_offset`/`total_lines`/`screen_lines` model into the pixel-space
`ScrollbarHandle` trait gpui-component expects. This adapter and its rendering
**must** live in `src/panels/terminal.rs`, not `src/terminal/`, because this
codebase enforces a hard boundary: `src/terminal/*` may never import
`gpui_component` (verified live: `grep -rn gpui_component src/terminal/*.rs`
returns no `use` statements today); only `src/panels/*.rs` adapters may. Two
small pure-`gpui` accessors are added to `TerminalView` so the panel-side
adapter can reach the shared `Term` handle and the live cell height without
crossing that boundary.

**Tech Stack:** Rust, `alacritty_terminal` 0.26 (`Term`, `Grid`, `Dimensions`,
`Scroll`), `gpui` (geometry types: `Point`, `Size`, `Pixels`), `gpui-component`
(`scroll::{Scrollbar, ScrollbarHandle, ScrollbarShow}`), `toml`/`serde`
(settings persistence, already a dependency).

## Global Constraints

- Full spec: `docs/superpowers/specs/2026-07-13-terminal-scrollback-scrollbar-design.md`.
- `scrollback_lines` default is **10,000**; valid input range is **1,000–50,000**
  (reject out-of-range input, don't silently clamp — matches the existing
  `parse_font_size`/`parse_monitor_interval` convention).
- Changing the setting **only affects newly opened terminal tabs** — never
  resize or recreate an already-open tab's `Term`/grid.
- The scrollbar's display mode is gpui-component's `ScrollbarShow::Scrolling`,
  set **explicitly** on the `Scrollbar` element — do not rely on
  `cx.theme().scrollbar_show` (ambient/inheritable) and do not use `Hover` or
  `Always`.
- `src/terminal/*` must never import `gpui_component`. Any code that touches
  `gpui_component::scroll::*` types goes in `src/panels/terminal.rs`.
- `ScrollbarHandle::offset()`/`set_offset()` use gpui's own scrollbar sign
  convention: `0` at the top of content, increasingly **negative** toward the
  bottom (down to `-(content_height - viewport_height)`). Do not invert this.

---

### Task 1: Settings data model (`src/settings.rs`)

**Files:**
- Modify: `src/settings.rs`

**Interfaces:**
- Produces: `TerminalSettings.scrollback_lines: u32` (default `10_000` via
  `default_scrollback_lines()`) — consumed by Task 2 (settings UI) and Task 3
  (`new_term` threading).

- [ ] **Step 1: Write the failing tests**

In `src/settings.rs`, update the `tests` module (replace the whole module —
current content is lines 124-211):

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
        assert_eq!(settings.appearance.theme_mode, "dark");
    }

    #[test]
    fn round_trip_preserves_fields() {
        let settings = AppSettings {
            appearance: AppearanceSettings {
                theme_mode: "light".to_string(),
            },
            terminal: TerminalSettings {
                font_family: "Consolas".to_string(),
                font_size: 16.0,
                monitor_basic_enabled: true,
                monitor_basic_interval_secs: 10,
                scrollback_lines: 20_000,
            },
        };
        let text = toml::to_string_pretty(&settings).expect("serialize");
        let parsed: AppSettings = toml::from_str(&text).expect("deserialize");
        assert_eq!(parsed.terminal.font_family, "Consolas");
        assert_eq!(parsed.terminal.font_size, 16.0);
        assert!(parsed.terminal.monitor_basic_enabled);
        assert_eq!(parsed.terminal.monitor_basic_interval_secs, 10);
        assert_eq!(parsed.terminal.scrollback_lines, 20_000);
        assert_eq!(parsed.appearance.theme_mode, "light");
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
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --lib settings:: 2>&1 | tail -40`
Expected: FAIL to compile — `TerminalSettings` has no field `scrollback_lines`.

- [ ] **Step 3: Add the field**

In `src/settings.rs`, current `TerminalSettings` (lines 33-52):

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
}
```

Replace with:

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

Add the default function near `default_monitor_interval_secs` (lines 58-60):

```rust
fn default_scrollback_lines() -> u32 {
    10_000
}
```

Update `impl Default for TerminalSettings` (lines 74-83):

```rust
impl Default for TerminalSettings {
    fn default() -> Self {
        Self {
            font_family: String::new(),
            font_size: default_font_size(),
            monitor_basic_enabled: false,
            monitor_basic_interval_secs: default_monitor_interval_secs(),
            scrollback_lines: default_scrollback_lines(),
        }
    }
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --lib settings:: 2>&1 | tail -40`
Expected: PASS (7 tests).

- [ ] **Step 5: Commit**

```bash
git add src/settings.rs
git commit -m "feat: add scrollback_lines setting to TerminalSettings"
```

---

### Task 2: Settings UI (`src/panels/settings_window.rs`)

**Files:**
- Modify: `src/panels/settings_window.rs`

**Interfaces:**
- Consumes: `AppSettings.terminal.scrollback_lines: u32` (Task 1).
- Produces: `parse_scrollback_lines(&str) -> Option<u32>`; a fourth input field
  on the Terminal settings tab, wired through the existing draft/Apply flow.

- [ ] **Step 1: Write the failing tests**

In `src/panels/settings_window.rs`, in the `tests` module at the bottom (current
content is lines 455-477), add:

```rust
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
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --lib panels::settings_window:: 2>&1 | tail -40`
Expected: FAIL to compile — `parse_scrollback_lines` not defined.

- [ ] **Step 3: Add the parser**

In `src/panels/settings_window.rs`, right after `parse_monitor_interval` (lines
19-30), add:

```rust
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
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --lib panels::settings_window:: 2>&1 | tail -40`
Expected: PASS (parser tests pass; existing font-size tests still pass).

- [ ] **Step 5: Wire the input field into `SettingsWindow`**

Add the field to the struct (current, lines 60-69):

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

In `SettingsWindow::new` (current, lines 72-96), add construction of the new
input and include it in the returned struct:

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

In `sync_inputs_to_draft` (current, lines 101-119), add validation before the
final `self.error = None; true`:

```rust
    fn sync_inputs_to_draft(&mut self, cx: &App) -> bool {
        self.draft.terminal.font_family = self.font_family_input.read(cx).value().to_string();
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
```

In `render_terminal_tab` (current, lines 281-344), add a fifth group after the
"轮询间隔 (秒)" group (i.e. right before the closing of the method, after the
existing last `.child(...)` block):

```rust
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
```

(This becomes the new final `.child(...)` of the method, right before its
closing `}`.)

- [ ] **Step 6: Build and run the full module's tests**

Run: `cargo build 2>&1 | tail -40`
Expected: builds successfully.

Run: `cargo test --lib panels::settings_window:: 2>&1 | tail -40`
Expected: PASS (6 tests: 3 existing font-size tests + 3 new scrollback-lines
tests).

- [ ] **Step 7: Commit**

```bash
git add src/panels/settings_window.rs
git commit -m "feat: add scrollback-lines input to the Terminal settings tab"
```

---

### Task 3: Thread `scrollback_lines` into `Term` construction

**Files:**
- Modify: `src/terminal/model.rs`
- Modify: `src/terminal/view.rs`
- Modify: `src/terminal/grid_snapshot.rs`

**Interfaces:**
- Consumes: `crate::settings::load().terminal.scrollback_lines: u32` (Task 1).
- Produces: `new_term(cols: usize, rows: usize, scrollback_lines: usize, events:
  flume::Sender<Event>) -> SharedTerm` — the new 4-argument signature (argument
  order: cols, rows, scrollback_lines, events). Any other caller of `new_term`
  must be updated to this order.

- [ ] **Step 1: Write the failing test**

In `src/terminal/model.rs`, add a `tests` module at the end of the file (there
is none currently):

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use alacritty_terminal::grid::Dimensions;
    use alacritty_terminal::vte::ansi::Processor;

    #[test]
    fn scrollback_lines_param_caps_grid_history() {
        let (tx, _rx) = flume::unbounded();
        // 3-row screen, a deliberately small 5-line scrollback cap — if this
        // were still hardcoded to the old 10_000 default, total_lines would
        // end up far above 3 + 5 after only 50 lines of output.
        let term = new_term(10, 3, 5, tx);
        {
            let mut t = term.lock();
            let mut parser: Processor = Processor::new();
            let bytes: Vec<u8> = (0..50)
                .flat_map(|i| format!("line {i}\r\n").into_bytes())
                .collect();
            parser.advance(&mut *t, &bytes);
        }
        let t = term.lock();
        assert!(t.total_lines() <= t.screen_lines() + 5);
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --lib terminal::model:: 2>&1 | tail -40`
Expected: FAIL to compile — `new_term` takes 3 arguments, called here with 4.

- [ ] **Step 3: Change `new_term`'s signature**

In `src/terminal/model.rs`, current (lines 67-75):

```rust
/// Build a fresh shared terminal at the given size.
pub fn new_term(cols: usize, rows: usize, events: flume::Sender<Event>) -> SharedTerm {
    let config = Config {
        scrolling_history: 10_000,
        ..Config::default()
    };
    let dims = TermDimensions::new(cols, rows);
    let term = Term::new(config, &dims, EventProxy(events));
    Arc::new(FairMutex::new(term))
}
```

Replace with:

```rust
/// Build a fresh shared terminal at the given size and scrollback capacity.
pub fn new_term(
    cols: usize,
    rows: usize,
    scrollback_lines: usize,
    events: flume::Sender<Event>,
) -> SharedTerm {
    let config = Config {
        scrolling_history: scrollback_lines,
        ..Config::default()
    };
    let dims = TermDimensions::new(cols, rows);
    let term = Term::new(config, &dims, EventProxy(events));
    Arc::new(FairMutex::new(term))
}
```

- [ ] **Step 4: Fix the other two call sites**

In `src/terminal/view.rs`, `base_setup` (current, lines 365-380):

```rust
    fn base_setup(
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> (FocusHandle, SharedTerm, flume::Sender<Event>, Task<()>) {
        let focus_handle = cx.focus_handle();
        window.focus(&focus_handle, cx);

        let (events_tx, events_rx) = flume::unbounded::<Event>();
        let term = new_term(DEFAULT_COLS, DEFAULT_ROWS, events_tx.clone());

        let drain_task = cx.spawn(async move |weak, cx| {
            run_drain(weak, events_rx, cx).await;
        });

        (focus_handle, term, events_tx, drain_task)
    }
```

Replace with:

```rust
    fn base_setup(
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> (FocusHandle, SharedTerm, flume::Sender<Event>, Task<()>) {
        let focus_handle = cx.focus_handle();
        window.focus(&focus_handle, cx);

        let (events_tx, events_rx) = flume::unbounded::<Event>();
        let scrollback_lines = crate::settings::load().terminal.scrollback_lines as usize;
        let term = new_term(DEFAULT_COLS, DEFAULT_ROWS, scrollback_lines, events_tx.clone());

        let drain_task = cx.spawn(async move |weak, cx| {
            run_drain(weak, events_rx, cx).await;
        });

        (focus_handle, term, events_tx, drain_task)
    }
```

`src/terminal/view.rs` doesn't import `crate::settings` yet — add it to the
`use` block at the top of the file (current, lines 19-28):

```rust
use crate::terminal::backend::{DeadBackend, LocalPty, PtyBackend};
use crate::terminal::bridge::{run_drain, run_feeder};
use crate::terminal::keymap::{PastePayload, encode_key, encode_paste};
use crate::terminal::model::{SharedTerm, new_term};
use crate::terminal::render::{cell_metrics, terminal_canvas};
use crate::terminal::scrollback;
use crate::terminal::selection;
use crate::terminal::serial::{SerialBackend, SerialConfig};
use crate::terminal::ssh::SshSession;
use crate::terminal::telnet::{TelnetBackend, TelnetConfig};
```

Add `use crate::settings;` above that block (this is safe — `settings.rs` is
explicitly plain Rust with no `gpui_component`, so importing it from
`terminal/` doesn't cross the boundary).

In `src/terminal/grid_snapshot.rs`, the test helper `term_with` (current, lines
166-175):

```rust
    fn term_with(bytes: &[u8], cols: usize, rows: usize) -> SharedTerm {
        let (tx, _rx) = flume::unbounded();
        let term = new_term(cols, rows, tx);
        {
            let mut t = term.lock();
            let mut parser: Processor = Processor::new();
            parser.advance(&mut *t, bytes);
        }
        term
    }
```

Replace the `new_term` call with a fixed literal (this is a test helper —
settings don't apply here):

```rust
    fn term_with(bytes: &[u8], cols: usize, rows: usize) -> SharedTerm {
        let (tx, _rx) = flume::unbounded();
        let term = new_term(cols, rows, 10_000, tx);
        {
            let mut t = term.lock();
            let mut parser: Processor = Processor::new();
            parser.advance(&mut *t, bytes);
        }
        term
    }
```

- [ ] **Step 5: Run test to verify it passes, then build everything**

Run: `cargo test --lib terminal::model:: 2>&1 | tail -40`
Expected: PASS.

Run: `cargo build 2>&1 | tail -60`
Expected: builds successfully (this is the step that proves both other call
sites were fixed correctly).

Run: `cargo test --lib terminal:: 2>&1 | tail -60`
Expected: PASS — all existing `terminal::*` tests (including
`grid_snapshot::tests`) still pass with the `term_with` change.

- [ ] **Step 6: Commit**

```bash
git add src/terminal/model.rs src/terminal/view.rs src/terminal/grid_snapshot.rs
git commit -m "feat: thread scrollback_lines setting into Term construction"
```

---

### Task 4: Scrollbar — `TerminalView` accessors + `TerminalScrollbarHandle` + `TerminalPanel` wiring

**Files:**
- Modify: `src/terminal/view.rs`
- Modify: `src/panels/terminal.rs`

**Interfaces:**
- Consumes: `crate::terminal::model::SharedTerm`, `crate::terminal::model::new_term`
  (tests only), `crate::terminal::scrollback::apply` (Task-independent, already
  exists), `alacritty_terminal::grid::{Dimensions, Scroll}`,
  `gpui_component::scroll::{Scrollbar, ScrollbarHandle, ScrollbarShow}`.
- Produces: `TerminalView::shared_term(&self) -> SharedTerm`,
  `TerminalView::last_cell_height(&self) -> f32`; `TerminalScrollbarHandle`
  (private to `panels/terminal.rs`) implementing `ScrollbarHandle`; a visible,
  draggable scrollbar in every rendered `TerminalPanel`.

- [ ] **Step 1: Write the failing tests**

In `src/panels/terminal.rs`, add at the end of the file:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use alacritty_terminal::vte::ansi::Processor;
    use crate::terminal::model::new_term;

    /// Builds a term with a known scrollback depth: `rows`-row screen,
    /// `scrollback_lines`-line history cap, `total_output_lines` lines of
    /// output fed through the VTE parser (enough to overflow the cap so the
    /// resulting `total_lines()` is deterministic).
    fn term_with_history(rows: usize, scrollback_lines: usize, total_output_lines: usize) -> SharedTerm {
        let (tx, _rx) = flume::unbounded();
        let term = new_term(10, rows, scrollback_lines, tx);
        {
            let mut t = term.lock();
            let mut parser: Processor = Processor::new();
            let bytes: Vec<u8> = (0..total_output_lines)
                .flat_map(|i| format!("line {i}\r\n").into_bytes())
                .collect();
            parser.advance(&mut *t, &bytes);
        }
        term
    }

    #[test]
    fn offset_and_content_size_reflect_scrollback_depth() {
        // 30 lines into a 3-row screen with a 20-line history cap: 27 lines
        // scroll into history, capped at 20 -> total_lines == 23.
        let term = term_with_history(3, 20, 30);
        let handle = TerminalScrollbarHandle::new(term.clone());
        handle.cell_h.set(10.0);

        assert_eq!(term.lock().total_lines(), 23);
        assert_eq!(term.lock().grid().display_offset(), 0);

        assert_eq!(handle.content_size(), size(px(0.0), px(230.0)));
        // hidden_above = 23 total - 3 screen - 0 display_offset = 20 lines
        // hidden above the current (live, bottom) viewport.
        assert_eq!(handle.offset(), point(px(0.0), px(-200.0)));
    }

    #[test]
    fn set_offset_to_top_scrolls_all_the_way_back() {
        let term = term_with_history(3, 20, 30);
        let handle = TerminalScrollbarHandle::new(term.clone());
        handle.cell_h.set(10.0);

        handle.set_offset(point(px(0.0), px(0.0)));
        assert_eq!(term.lock().grid().display_offset(), 20);
    }

    #[test]
    fn set_offset_to_bottom_returns_to_live_area() {
        let term = term_with_history(3, 20, 30);
        let handle = TerminalScrollbarHandle::new(term.clone());
        handle.cell_h.set(10.0);

        handle.set_offset(point(px(0.0), px(0.0)));
        assert_eq!(term.lock().grid().display_offset(), 20);

        handle.set_offset(point(px(0.0), px(-200.0)));
        assert_eq!(term.lock().grid().display_offset(), 0);
    }

    #[test]
    fn zero_cell_height_is_a_safe_no_op() {
        let term = term_with_history(3, 20, 30);
        let handle = TerminalScrollbarHandle::new(term.clone());
        // cell_h left at its default 0.0 (pre-first-paint state).

        assert_eq!(handle.offset(), point(px(0.0), px(0.0)));
        assert_eq!(handle.content_size(), size(px(0.0), px(0.0)));

        handle.set_offset(point(px(0.0), px(-999.0)));
        assert_eq!(term.lock().grid().display_offset(), 0);
    }
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --lib panels::terminal:: 2>&1 | tail -40`
Expected: FAIL to compile — `TerminalScrollbarHandle` not defined.

- [ ] **Step 3: Implement `TerminalScrollbarHandle`**

In `src/panels/terminal.rs`, update the imports at the top (current, lines
6-16):

```rust
use gpui::{
    App, ClickEvent, Context, Entity, FocusHandle, Focusable, InteractiveElement, IntoElement,
    ParentElement, Render, StatefulInteractiveElement, Styled, WeakEntity, Window, div,
};
use gpui_component::dock::{Panel, PanelControl, PanelEvent, PanelView, TabPanel};
use gpui_component::ActiveTheme;

use crate::panels::icons::{AppIcon, icon};
use std::sync::Arc;

use crate::terminal::view::TerminalView;
```

Replace with:

```rust
use std::cell::Cell;
use std::rc::Rc;
use std::sync::Arc;

use alacritty_terminal::grid::{Dimensions, Scroll};
use gpui::{
    App, ClickEvent, Context, Entity, FocusHandle, Focusable, InteractiveElement, IntoElement,
    ParentElement, Pixels, Point, Render, Size, StatefulInteractiveElement, Styled, WeakEntity,
    Window, div, point, px, size,
};
use gpui_component::dock::{Panel, PanelControl, PanelEvent, PanelView, TabPanel};
use gpui_component::scroll::{Scrollbar, ScrollbarHandle, ScrollbarShow};
use gpui_component::ActiveTheme;

use crate::panels::icons::{AppIcon, icon};
use crate::terminal::model::SharedTerm;
use crate::terminal::scrollback;
use crate::terminal::view::TerminalView;
```

Add the adapter type, right before `pub struct TerminalPanel` (current line 18):

```rust
/// Adapts the terminal's alacritty grid (line-based scrollback) to
/// gpui-component's pixel-based `ScrollbarHandle` contract. Lives here, not
/// under `terminal/`, because `gpui_component` may only be imported from
/// `panels/` (see the design spec) — `terminal/view.rs` only exposes the
/// plain-`gpui` accessors (`shared_term`, `last_cell_height`) this needs.
#[derive(Clone)]
struct TerminalScrollbarHandle {
    term: SharedTerm,
    /// Cached each render from `TerminalView::last_cell_height()`.
    /// `ScrollbarHandle` methods take `&self` with no `cx`, so they can't
    /// read the live entity value directly — this is refreshed by
    /// `TerminalPanel::render` before the `Scrollbar` element is built.
    cell_h: Rc<Cell<f32>>,
}

impl TerminalScrollbarHandle {
    fn new(term: SharedTerm) -> Self {
        Self {
            term,
            cell_h: Rc::new(Cell::new(0.0)),
        }
    }
}

impl ScrollbarHandle for TerminalScrollbarHandle {
    /// gpui's scrollbar sign convention: `0` at the top of content,
    /// increasingly negative toward the bottom.
    fn offset(&self) -> Point<Pixels> {
        let cell_h = self.cell_h.get();
        if cell_h <= 0.0 {
            return point(px(0.0), px(0.0));
        }
        let term = self.term.lock();
        let hidden_above = term
            .total_lines()
            .saturating_sub(term.screen_lines())
            .saturating_sub(term.grid().display_offset());
        point(px(0.0), px(-(hidden_above as f32 * cell_h)))
    }

    fn set_offset(&self, offset: Point<Pixels>) {
        let cell_h = self.cell_h.get();
        if cell_h <= 0.0 {
            return;
        }
        let mut term = self.term.lock();
        let total = term.total_lines();
        let screen = term.screen_lines();
        let current_display_offset = term.grid().display_offset();
        let hidden_above = ((-f32::from(offset.y)) / cell_h).round().max(0.0) as usize;
        let target_display_offset = total.saturating_sub(screen).saturating_sub(hidden_above);
        let delta = target_display_offset as i32 - current_display_offset as i32;
        if delta != 0 {
            scrollback::apply(&mut term, Scroll::Delta(delta));
        }
    }

    fn content_size(&self) -> Size<Pixels> {
        let cell_h = self.cell_h.get();
        if cell_h <= 0.0 {
            return size(px(0.0), px(0.0));
        }
        let term = self.term.lock();
        // Width is unused: gpui-component's Scrollbar only reads `.height`/
        // `offset().y` for a vertical-only axis (scrollbar.rs:541-559).
        size(px(0.0), px(term.total_lines() as f32 * cell_h))
    }
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --lib panels::terminal:: 2>&1 | tail -60`
Expected: PASS (4 new tests).

- [ ] **Step 5: Add the two accessors to `TerminalView`**

In `src/terminal/view.rs`, right after `remember_cell_metrics` (current, lines
649-664, ending `self.last_origin_y = origin_y;\n    }`), add:

```rust
    /// The shared `Term` handle, for building read-only viewers outside the
    /// `Entity`/`cx` system — currently only `panels::terminal`'s scrollbar
    /// adapter, which needs it inside `ScrollbarHandle` methods that take no
    /// `cx`.
    pub fn shared_term(&self) -> SharedTerm {
        self.term.clone()
    }

    /// The cell height measured at the last paint (`0.0` before the first
    /// paint). Used by `panels::terminal`'s scrollbar adapter, which has no
    /// `cx` inside its `ScrollbarHandle` methods to read this live.
    pub fn last_cell_height(&self) -> f32 {
        self.last_cell_h
    }
```

- [ ] **Step 6: Wire the handle into `TerminalPanel`**

In `src/panels/terminal.rs`, update the struct (current, lines 18-25):

```rust
pub struct TerminalPanel {
    terminal: Entity<TerminalView>,
    /// The `TabPanel` this panel currently lives in, handed to us via
    /// `on_added_to`. Needed so the close button (embedded in `title()`, since
    /// this gpui-component revision's tab strip has no built-in per-tab close
    /// icon) can remove *this specific* panel regardless of which tab is active.
    tab_panel: Option<WeakEntity<TabPanel>>,
}
```

Replace with:

```rust
pub struct TerminalPanel {
    terminal: Entity<TerminalView>,
    /// The `TabPanel` this panel currently lives in, handed to us via
    /// `on_added_to`. Needed so the close button (embedded in `title()`, since
    /// this gpui-component revision's tab strip has no built-in per-tab close
    /// icon) can remove *this specific* panel regardless of which tab is active.
    tab_panel: Option<WeakEntity<TabPanel>>,
    /// Lazily built on first render (`TerminalPanel::new` takes no `cx`, and
    /// the handle needs `self.terminal.read(cx)` to get the shared `Term`).
    scrollbar_handle: Option<TerminalScrollbarHandle>,
}
```

Update `TerminalPanel::new` (current, lines 27-33):

```rust
impl TerminalPanel {
    pub fn new(terminal: Entity<TerminalView>) -> Self {
        Self {
            terminal,
            tab_panel: None,
        }
    }
```

Replace with:

```rust
impl TerminalPanel {
    pub fn new(terminal: Entity<TerminalView>) -> Self {
        Self {
            terminal,
            tab_panel: None,
            scrollbar_handle: None,
        }
    }
```

Update `impl Render for TerminalPanel` (current, lines 56-61):

```rust
impl Render for TerminalPanel {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        // Just embed the inner terminal entity; it renders/handles input itself.
        div().size_full().child(self.terminal.clone())
    }
}
```

Replace with:

```rust
impl Render for TerminalPanel {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if self.scrollbar_handle.is_none() {
            let term = self.terminal.read(cx).shared_term();
            self.scrollbar_handle = Some(TerminalScrollbarHandle::new(term));
        }
        let handle = self.scrollbar_handle.as_ref().expect("initialized above");
        handle.cell_h.set(self.terminal.read(cx).last_cell_height());

        // Embed the inner terminal entity (it renders/handles input itself),
        // plus a right-side scrollbar overlay driven by `handle`.
        div()
            .relative()
            .size_full()
            .child(self.terminal.clone())
            .child(
                div().absolute().inset_0().child(
                    Scrollbar::vertical(handle)
                        .id("terminal-scrollbar")
                        .scrollbar_show(ScrollbarShow::Scrolling),
                ),
            )
    }
}
```

- [ ] **Step 7: Build the whole crate**

Run: `cargo build 2>&1 | tail -60`
Expected: builds successfully.

Run: `cargo test --lib 2>&1 | tail -80`
Expected: PASS — full test suite (settings, settings_window, terminal::*,
panels::terminal) all green.

- [ ] **Step 8: Commit**

```bash
git add src/terminal/view.rs src/panels/terminal.rs
git commit -m "feat: add a right-side scrollback scrollbar to the terminal panel"
```

---

### Task 5: Build verification and manual smoke test

**Files:** none (verification only)

- [ ] **Step 1: Release build**

Run: `cargo build --release 2>&1 | tail -40`
Expected: builds successfully.

- [ ] **Step 2: Manual smoke test**

If a display is available in this environment, run `cargo run --release` and
verify by hand (if no display is available, note that explicitly rather than
claiming it was checked):

1. Open Settings → Terminal. Confirm a "回滚行数" field is present, prefilled
   with `10000`. Change it to `2000`, click 应用 (Apply). Confirm no error is
   shown.
2. Enter a clearly invalid value (`500`, then `abc`) and click 应用; confirm an
   error message appears each time and the window stays open (draft not
   persisted).
3. Set it back to a valid value (e.g. `2000`) and click 确定 (Confirm); confirm
   the window closes.
4. Open a **new** local terminal tab. Run a command that produces well over
   2,000 lines of output (e.g. `seq 1 5000`). Scroll to the very top with
   Shift+Home; confirm the oldest visible line is consistent with roughly a
   2,000-line cap (not the full 5,000 lines), proving the new tab picked up the
   setting.
5. On that same tab, move the mouse over the terminal and scroll the wheel;
   confirm a thin scrollbar thumb appears on the right edge and moves as you
   scroll, then fades out a couple of seconds after you stop.
6. Drag the scrollbar thumb to the very top; confirm the view jumps to the
   oldest retained history. Drag it to the very bottom; confirm the view
   returns to the live prompt and typing there still works normally.
7. Resize the terminal pane/window while scrolled partway through history;
   confirm the scrollbar thumb size/position stays sane (no panic, no
   wildly-wrong thumb position).
8. Open a terminal tab whose entire output fits on one screen (e.g. a fresh
   shell with no long output). Confirm the scrollbar is not meaningfully
   visible/draggable (nothing to scroll).
9. Open a second terminal tab that was already open *before* changing the
   setting in step 1 (if one exists) — or open one, change the setting, then
   revisit this same old tab — and confirm its scrollback depth is unaffected
   by the later settings change (only new tabs pick it up).

- [ ] **Step 3: Report results**

Summarize which of the 9 manual checks passed, and paste the full text of any
that didn't, before considering this task/plan complete.
