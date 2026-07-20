# Keyboard shortcuts

## Problem

Caracal has almost no app-level keyboard shortcuts today. The only
`KeyBinding`s that exist ([main.rs:153](../../../src/main.rs#L153)) reclaim
`ctrl-c`/`tab`/`shift-tab` from gpui-component's `Root` so they reach the
terminal as raw input, and a couple of terminal-only combos
(`Ctrl+Shift+C`/`V` for copy/paste, `Shift+PageUp`/etc. for scrollback) are
hand-checked inside `TerminalView::on_key_down`
([terminal/view.rs:745](../../../src/terminal/view.rs#L745)). Everything
else — opening a new tab, closing one, switching between them, opening
Settings, toggling the side panels or the quick-commands drawer, adjusting
font size, clearing the screen — requires the mouse.

## Scope

Tab management, panel/window toggling, new-connection, and a small set of
terminal-editing conveniences (font zoom, clear screen). Search/find inside
the terminal is explicitly **out of scope** — it doesn't exist as a feature
yet and would need its own UI (match highlighting, next/prev-match
navigation); it deserves its own design.

## Cross-platform convention

Every binding is expressed with gpui's `"secondary-x"` modifier alias
rather than a hardcoded `ctrl-`. `Modifiers::secondary()` resolves to `Cmd`
on macOS and `Ctrl` on Windows/Linux
(`crates/gpui/src/platform/keystroke.rs`), so one binding table covers all
three platforms with no `#[cfg]` branching.

## Full keybinding table

| Shortcut | Action |
|---|---|
| `secondary-shift-t` | New tab: duplicate the focused tab's connection (new shell on the same host if the focused tab is SSH; new local shell otherwise, or if nothing is focused) |
| `secondary-shift-w` | Close the focused tab |
| `secondary-tab` / `secondary-shift-tab` | Next / previous tab in the current tab group |
| `secondary-1` .. `secondary-9` | Jump to the Nth tab in the current tab group (1-indexed) |
| `secondary-shift-n` | Open the "New Connection" window (focuses it if already open) |
| `secondary-b` | Toggle the left sidebar (SFTP / Network / Security & Auth) |
| `secondary-shift-b` | Toggle the right sidebar (Sessions / History / Monitor) |
| `secondary-j` | Toggle the bottom quick-commands drawer |
| `secondary-,` | Open Settings (focuses it if already open) |
| `secondary-=` | Zoom terminal font in |
| `secondary--` | Zoom terminal font out |
| `secondary-shift-l` | Clear screen: erase the visible viewport only, not scrollback |

Plain `Ctrl+L` is unaffected — it still passes through as a raw `0x0c` byte
to the shell/remote program, which almost universally already binds it to
clear-screen (readline's `clear-screen`), or to whatever else the running
program wants it for (e.g. `vim`/`htop` redraw). `Ctrl+T`/`Ctrl+W` are left
alone too, for the same reason (`Ctrl+W` deletes a word in
bash/readline) — that's why tab management uses the `Shift` variants,
matching the convention most terminal emulators (iTerm2, Windows Terminal,
GNOME Terminal) already use to avoid this exact collision.

## Architecture

### Action dispatch works even with a terminal focused

New actions are declared with `gpui::actions!` and bound with `cx.bind_keys`
at `None` context (global), the same mechanism already used for
`Interrupt`/`SendTab`/`SendBackTab`
([terminal/view.rs:45](../../../src/terminal/view.rs#L45),
[main.rs:153](../../../src/main.rs#L153)). `on_action` handlers live on
`Workspace`'s root element.

This works reliably even while a `TerminalView` has keyboard focus because
of how gpui resolves a keystroke (`Window::dispatch_key_event` in gpui's own
`window.rs`): it first walks the dispatch-context path from the focused
element up to the root looking for a matching `KeyBinding`, and only if
*nothing* matches does it fall back to delivering a raw `KeyDownEvent` to
`on_key_down` handlers. None of the new shortcuts collide with the
`TERMINAL_KEY_CONTEXT`-scoped bindings or gpui-component `Root`'s own
default keymap, so they always win the match and never reach
`TerminalView::on_key_down`'s raw-byte fallback.

### Close tab reuses gpui-component's own action

`gpui-component`'s `dock` module already defines `actions!(dock, [ToggleZoom,
ClosePanel])` and `TabPanel` already has an `on_action` handler for
`ClosePanel` (wired into its own context-menu "Close" item). Caracal just
needs a `KeyBinding::new("secondary-shift-w", gpui_component::dock::ClosePanel,
None)` — no new action or handler required.

### Next/prev/jump-to-N tab without forking gpui-component

`TabPanel::set_active_ix` is private, and there's no public "next/previous"
helper — but `DockItem::active_index(new_ix, cx) -> Self` **is** public and
mutates the live `TabPanel` entity's `active_ix` field in place (it's a
method on the same crate as the private field, so it has access; Caracal
just calls the public wrapper). It doesn't call `cx.notify()` itself
(confirmed by reading its body), so `Workspace`'s handler calls
`cx.notify()` on the `Entity<TabPanel>` immediately after, to force the
repaint. This means next/prev/goto-N are implementable entirely through
gpui-component's existing public API — no fork or vendored patch needed.

`Workspace` resolves "the current tab group" by walking `dock_area.center()`
today (a `Split` wrapping one `Tabs` item, since there's no split-view UI
yet); the same walk generalizes if splits are ever added later — the
shortcut would then operate on whichever tab group contains the focused
terminal.

### Sidebar toggle remembers what was showing

`left_active`/`right_active` are `Option<PanelId>`, and closing them (mouse
click on the active icon) already sets them to `None`. A generic
keyboard toggle needs to know what to reopen, so `Workspace` gains a
remembered last-shown `PanelId` per side (defaulting to `Sftp` for left,
`Sessions` for right — the latter matches today's actual startup default).
Toggling flips between `None` and that remembered value, updating the
memory whenever a side's active panel changes to `Some(_)` for any reason
(mouse click included), not just via the keyboard.

### Zoom reuses the existing font-broadcast path

`Workspace::apply_font_settings` already broadcasts a font family/size/
fallback to every open tab and is called from Settings → Apply
([workspace.rs:1019](../../../src/workspace.rs#L1019)). Zoom in/out reads
the current persisted `terminal.font_size`, adjusts it by a fixed step
(clamped to a sane min/max), calls `apply_font_settings` with the rest of
the font config unchanged, and persists the change the same way Settings →
Apply does — so zooming behaves exactly like changing the font-size field
in Settings, just without opening the dialog, and survives restart.

### Clear screen is terminal-local

A new `ClearScreen` action, declared alongside `Interrupt`/`SendTab` in
`terminal::view`'s `actions!` block and bound only in
`TERMINAL_KEY_CONTEXT` (so it's a no-op with no terminal focused). Its
handler calls `alacritty_terminal`'s `Term::clear_screen(ClearMode::All)`
on the focused terminal — clears the visible grid, leaves scrollback
history untouched (there's no shortcut in this design for clearing
scrollback).

## Error handling / edge cases

- Any tab-management shortcut fires with no terminal focused (e.g. a
  sidebar panel has focus): no-op.
- `secondary-1`..`secondary-9` beyond the current tab count: no-op.
- New-tab (`secondary-shift-t`) on a connection type with no reconnect
  config (already-closed tab, or a type that never had one) falls back to
  opening a new local shell — the same fallback `open_local_with` already
  uses elsewhere.
- Zoom clamps to existing font-size bounds; it can't be pushed to an
  unreadable or unbounded size.

## Testing

Manual verification after implementation — each shortcut gets tried by
hand; no screenshot-driven GUI checks (per prior guidance in this project).
Pure logic with real edge cases (next/prev wraparound at the ends of the
tab list, jump-to-N out-of-range, the last-shown-panel bookkeeping) gets
unit tests, following the existing precedent of
`Workspace::lowest_free_number`.
