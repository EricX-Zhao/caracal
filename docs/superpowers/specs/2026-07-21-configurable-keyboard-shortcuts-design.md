# Configurable keyboard shortcuts

## Problem

Caracal's application-level keyboard shortcuts (added in the prior
[keyboard-shortcuts feature](2026-07-20-keyboard-shortcuts-design.md)) are
hardcoded: 20 `KeyBinding::new(...)` calls scattered across
[main.rs:159-183](../../../src/main.rs#L159-L183), bound once at startup
and never touched again. There's no way for a user to see what's bound to
what, or to change a binding, without editing and recompiling the source.

## Scope

Only the shortcuts introduced by the prior feature: 12 individually
rebindable actions, plus the 9 "jump to tab N" bindings (`secondary-1`
through `secondary-9`) shown as one fixed reference row (see "Jump-to-tab
handling" below). Terminal-input basics (`Interrupt`/`SendTab`/
`SendBackTab` — raw key handling for SIGINT/tab-completion, not gpui
actions users think of as "shortcuts") and every binding gpui-component
registers internally (copy, focus navigation, dock zoom, etc.) are out of
scope and untouched.

## How live rebinding works without touching gpui-component

The obvious approach — `cx.clear_key_bindings()` then re-register
everything — was investigated and rejected. `clear_key_bindings()` wipes
gpui's *entire* global keymap, including everything gpui-component
registers internally across ~13 of its own modules (`dock::init`,
`input::init`, `select::init`, etc. — confirmed by grep across the vendored
crate). The only way to restore those is calling
`gpui_component::init(cx)` again, but that function also calls
`theme::init(cx)`, which does `Theme::change(ThemeMode::Light, None, cx)`
internally (confirmed by reading its source) — re-running it to fix the
keymap would silently reset the user's theme back to default every time
they change a shortcut. Zed itself avoids this problem, but only because
*all* of Zed's own keybindings are declarative JSON loaded via
`KeymapFile::load_asset`, fully decoupled from other subsystem init —
gpui-component doesn't have that separation, and replicating it would mean
Caracal hand-maintaining a duplicate of gpui-component's internal
keybindings that has to be kept in sync on every upstream update.

The actual solution needs no clearing at all. gpui's own keymap doc
comment (`crates/gpui/src/keymap.rs`) states the precedence rule directly:

> In the case of multiple bindings at the same depth, the ones added to
> the keymap later take precedence.

So changing a shortcut is just `cx.bind_keys([KeyBinding::new(new_key,
Action, context)])` — the new binding silently shadows the old one for
that exact keystroke, takes effect immediately, and gpui-component's own
bindings are never touched. The shadowed-old binding stays in gpui's
internal `Vec` (harmless — bounded by how many times a user changes
shortcuts in one running session, never cleaned up, never a correctness
issue). This is the same mechanism Zed's own user-keymap-override feature
relies on.

## Data model

`src/keybindings.rs` (new file) is the single source of truth for all 12
rebindable actions plus the 1 read-only jump-to-tab entry, replacing the
literal `KeyBinding::new(...)` calls currently in `main.rs`:

- `DEFAULT_KEYBINDINGS: &[(&str, &str)]` — stable action-id string (e.g.
  `"new_tab"`, `"close_tab"`, `"next_tab"`, `"prev_tab"`, `"new_connection"`,
  `"toggle_left_sidebar"`, `"toggle_right_sidebar"`,
  `"toggle_quick_commands"`, `"open_settings"`, `"zoom_in"`, `"zoom_out"`,
  `"clear_screen"`) mapped to its single default gpui binding string
  (`"secondary-shift-t"`, etc. — `zoom_in`'s default/editable entry is
  `"secondary-="`). `secondary-shift-=` (the shifted-`+` convenience alias
  from the prior feature, for keyboards where reaching `+` naturally means
  holding Shift) stays a second, permanently-fixed binding to `ZoomIn`,
  registered at startup outside the override table and never affected by
  rebinding or reset — the user-configurable key only ever changes the
  primary `"secondary-="` entry.
- `fn build_key_bindings(overrides: &HashMap<String, String>) -> Vec<KeyBinding>`
  — one `match` arm per action-id, mapping the (possibly-overridden) key
  string to the concrete typed `KeyBinding::new(key, ActualActionType,
  context)`. `"close_tab"` maps to `gpui_component::dock::ClosePanel` (not
  a Caracal-owned action — we only own its keybinding entry, not its
  behavior, which is unaffected). Used both at startup (replacing
  `main.rs`'s literal list) and by the Settings window on Apply (passing
  only the changed entries to `cx.bind_keys`, per the live-rebind mechanism
  above).
- `fn format_keystroke(ks: &gpui::Keystroke) -> String` — canonicalizes a
  captured keypress to the portable `"secondary-..."` form by checking
  `ks.modifiers.secondary()` rather than the literal Cmd/Ctrl bit, so a
  binding recorded on Linux still reads correctly if the same
  `settings.toml` is ever used on macOS.
- `fn action_label_key(action_id: &str) -> &'static str` — maps an
  action-id to its locale key (e.g. `"Settings.shortcut_new_tab"`) for the
  UI's row labels.

Persistence: `AppSettings` (`src/settings.rs`) gains a fourth field,
`pub keybindings: KeybindingsSettings`, `#[serde(default)]`, wrapping a
`HashMap<String, String>` keyed by action-id — **only the entries the user
has actually overridden are stored**; anything absent falls back to
`DEFAULT_KEYBINDINGS`. This keeps `settings.toml` untouched for users who
never open the new tab, and means a future addition to
`DEFAULT_KEYBINDINGS` (a new shortcut in some later feature) doesn't need
a migration.

## Settings UI

A 5th Settings tab (`SettingsTab::Shortcuts`, "快捷键" / "Shortcuts"),
listing:

- **12 editable rows**, each: localized action label, current key
  (read from `self.draft.keybindings`, falling back to the default),
  a "记录" (Record) button, and a "恢复默认" (Reset) button.
- **1 read-only reference row**: "跳转到标签页 1-9" / "Jump to tab 1-9" —
  `secondary-1` .. `secondary-9`, no Record/Reset controls. See
  "Jump-to-tab handling" below for why these stay fixed.
- A tab-level "全部恢复默认" (Reset All) button.

**Capture flow:** clicking Record turns that row's button into "按下新按键…"
/ "Press a key…" and focuses a small capture element. The next keystroke
with at least one modifier held (bare unmodified keys are rejected with an
inline "需要至少一个修饰键" message, since none of these 12 actions should
ever bind to a bare letter — that would swallow normal typing anywhere the
shortcut's context is global) is read via `on_key_down`, canonicalized via
`format_keystroke`, and staged into `self.draft.keybindings` — nothing
written to disk or bound live yet, matching every other field in this
window. **Escape cancels** the recording and reverts the row to showing
whatever key it had before (a standard capture-widget convention, not
itself capturable as a shortcut).

**Conflict handling:** on Apply, `sync_inputs_to_draft`-equivalent
validation checks every pair of the 12 editable action-ids' *resolved*
keys (draft override or default) for exact string equality. A duplicate
blocks Apply the same way an invalid font-size does today — inline error
naming both the key and which action already owns it (e.g. "secondary-b
已被"切换左侧栏"占用"). The 9 fixed jump-to-tab keys, the fixed `secondary-shift-=` zoom-in alias,
and gpui-component's own internal bindings are not part of this check
(out of scope — a user technically *could* pick e.g. `secondary-1` for
another action, silently shadowing that jump-to-tab binding per the
precedence rule above; this is an accepted edge case, not guarded against,
since guarding it would mean hardcoding awareness of every out-of-scope
binding this feature explicitly excludes).

**Reset:** per-row "恢复默认" stages that one action-id's default string
back into the draft (or removes the override, functionally identical).
The tab-level "全部恢复默认" clears every override in
`self.draft.keybindings` at once. Neither writes to disk until Apply.

**Apply integration:** `SettingsWindow::apply` (already the single place
that persists+broadcasts every other tab's changes) gains one more step:
diff `self.committed.keybindings` against `self.draft.keybindings`,
call `build_key_bindings` for just the changed action-ids, `cx.bind_keys`
those, save the full draft via the existing `settings::save`.

## Jump-to-tab handling

`secondary-1`..`secondary-9` (`GotoTab1`..`GotoTab9`) are shown as one
fixed, non-interactive reference row rather than 9 rebindable rows.
Rebinding "jump to tab 3" to an arbitrary different single key is a
low-value, high-UI-cost use case (9 near-identical rows for a feature
nobody asks to individually remap) — users who want a different set of
tab-jump keys entirely are not served by remapping them one at a time
anyway. If demand for this ever appears, it's a separate, later
scope decision, not bundled into this one.

## Testing

`format_keystroke`'s canonicalization and the conflict-detection check are
pure functions (`Keystroke`/`String` in, `String`/`bool` out) — unit
tested the same way as this codebase's other pure logic (see
`Workspace::lowest_free_number`, `Workspace::clamped_font_size`). The
capture widget's `on_key_down` wiring and the live-rebind call itself are
gpui glue, verified manually — no screenshot-driven GUI checks, per this
project's standing convention.

## Error handling / edge cases

- Recording, then pressing a bare unmodified key: rejected inline, row
  stays in recording state so the user can try again.
- Recording, then pressing Escape: recording cancelled, row reverts.
- Two editable actions ending up with the same key after independent
  edits: Apply blocked with an inline conflict message naming both.
- Closing the Settings window (or Cancel) without Apply: `self.draft` is
  discarded the same way it already is for every other tab; no keybinding
  change was ever live (capture only stages into the draft, never calls
  `cx.bind_keys` before Apply).
