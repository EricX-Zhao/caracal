# README rewrite: design

Date: 2026-07-23

## Purpose

The current `README.md` is a build-instructions-first document with a short feature list and
a brief Acknowledgments footnote. This rewrite repositions the README as a feature/roadmap
showcase with the WindTerm/NyaTerm tribute given real weight, dropping build/install content
entirely (out of scope for this doc — not replaced elsewhere, just removed).

## Scope

One file: `README.md`. Full rewrite, not an incremental edit — structure, wording, and content
all change. No other files are touched by this project. The stale internal doc
`docs/reference/nyaterm-gap-roadmap.md` is explicitly left alone (not updated, not linked from
the new README) — the new README's Roadmap section is self-contained and is the sole authority
for public-facing roadmap content going forward.

## Language format

Single `README.md`. English is the primary/visible text throughout. Each major section (Features,
Roadmap, Acknowledgments) gets a trailing `<details><summary>中文</summary>` block containing a
Chinese translation of that section's content. License and the title/tagline do not need a
Chinese fold (short, and license text shouldn't be translated). This keeps one file, fully
searchable in both languages, without maintaining two parallel documents.

## Section structure (in order)

1. Title + one-line tagline
2. Features
3. Roadmap
4. Acknowledgments
5. License
6. Closing note on how the project is built

No Building-from-source or Prebuilt-binaries sections — removed per explicit decision, not
relocated.

## Section content

### Title + tagline

Same factual content as today's opening line: native, GPU-accelerated terminal / SSH / Telnet /
serial client built on GPUI (Zed's UI framework) and gpui-component. No badges added.

### Features

Ten bullets, organized by capability area, each reflecting *verified current source state* (not
aspirational) — confirmed by direct code review on 2026-07-23:

1. **Protocols** — local terminal (native PTY), SSH (password or saved-encrypted key auth),
   Telnet (RFC 854 IAC negotiation), Serial (baud/data bits/parity/stop bits/flow control)
2. **Saved Connections** — group tree, drag-reorder within/across groups, search, sort, TOML
   import/export
3. **SFTP File Browser** — sortable multi-column browser sharing the SSH connection (no second
   dial), context menu (rename/properties/delete), hidden-files toggle, directory history,
   bidirectional cwd sync with the terminal, transfer queue
4. **Resource Monitoring** — per-host live view of remote CPU/memory/network/disk over the
   existing SSH channel (current scope only — GPU/process/Docker are Roadmap items, not
   claimed here)
5. **Quick Commands** — a command drawer for repeatable snippets, execute-or-append into the
   active terminal
6. **Configurable keyboard shortcuts** — live rebind, conflict detection, reset-to-default
7. **Security** — SSH private keys encrypted at rest (AES-256-GCM, Argon2id-derived key) behind
   a master password; optional OS keyring unlock (Keychain/Credential Manager/Secret Service);
   passwords are always direct-entry, never persisted
8. **Customization** — 20+ built-in terminal color themes, font family/size picker with a
   bundled Nerd Font + CJK fallback
9. **i18n** — English / 简体中文, switchable at runtime
10. **GPU-accelerated rendering** via `wgpu` (GPUI)

### Roadmap

Self-contained, high-level, no priority ordering, no links to internal docs. Seven items —
three carried from real current gaps, four newly proposed as natural next steps for this class
of software:

- Resource Monitoring: GPU stats, process manager, Docker container view
- Quick Commands: categories, search, `{{variable}}` templating, import/export
- SSH port forwarding / tunnel management: local, remote, and dynamic (SOCKS) proxy tunnels
- Multi-session broadcast input: type once, send to multiple open tabs/panes simultaneously
- Session logging & recording: capture terminal output to file, replay later
- Cloud backup & sync: back up saved connections/settings (e.g. WebDAV) and sync across devices
- macOS support (currently Linux + Windows only, verified via `.github/workflows/build.yml`
  matrix and absence of any macOS packager config)

Explicitly excluded from this list (per discussion): SSH auth extensions (jump-host/2FA/
algorithm-order), file-browser move/bookmarks, and the connection-history/network-discovery
stub panels — dropped from the public roadmap, not merely deprioritized.

### Acknowledgments

Keep the existing two-entry, impersonal/project-voice structure ("Caracal owes a debt...") —
not first-person, no author bio. Expand each entry with concrete specifics rather than generic
praise:

- **WindTerm** — name the specific shape Caracal borrowed: all-in-one session/SFTP/serial/
  monitoring in one fast, keyboard-first tool; the saved-connections group-tree UX; the
  settings/shortcuts model.
- **NyaTerm** — reframe from "planted the seed" to reflect its actual role: the direct,
  feature-by-feature reference this project's early roadmap was gap-analyzed against (per
  `docs/reference/nyaterm-gap-roadmap.md`'s history), not just a vague inspiration.

### License

Unchanged: dual MIT / Apache-2.0, same wording as today.

### Closing note on how the project is built

A short, final note (after License, last thing in the file) stating that Caracal is built
primarily by an engineer with no prior Rust experience, working with Claude as the main coding
collaborator. Same impersonal/project-voice tone as Acknowledgments — matter-of-fact, not a
marketing line, one or two sentences, no Chinese fold needed (short enough to read either way,
consistent with License's treatment).

## Non-goals

- No screenshots/GIFs (none exist in `assets/`; not fabricating placeholders)
- No CI/build-status badges
- No update to `docs/reference/nyaterm-gap-roadmap.md`
- No restoration of build/install instructions anywhere in this doc
