# README Rewrite Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace `README.md` with a feature/roadmap-showcase document (bilingual, English primary with Chinese `<details>` folds) that drops all build/install content and gives the WindTerm/NyaTerm tribute real weight, per `docs/superpowers/specs/2026-07-23-readme-rewrite-design.md`.

**Architecture:** Single-file documentation change. No code, no tests in the traditional sense — "correctness" here means the file matches the approved spec structure and every removed/added section is verifiable by direct inspection (grep for headers, absence of removed sections).

**Tech Stack:** Markdown only.

## Global Constraints

- Single file touched: `README.md`. No other files created or modified.
- Section order: Title/tagline → Features → Roadmap → Acknowledgments → License → closing note. (Per spec §"Section structure".)
- No Building-from-source or Prebuilt-binaries sections, anywhere in the file. (Per spec §"Scope".)
- English primary; Features/Roadmap/Acknowledgments each get a trailing `<details><summary>中文</summary>` Chinese translation fold. License and the closing note do not get a fold. (Per spec §"Language format".)
- Roadmap is self-contained — no link to `docs/reference/nyaterm-gap-roadmap.md` or any other internal doc. (Per spec §"Scope" and §"Roadmap".)
- No screenshots, no CI/build badges. (Per spec §"Non-goals".)
- Acknowledgments and the closing note use impersonal/project-voice tone ("Caracal owes a debt...", "Caracal is built..."), never first-person. (Per spec §"Acknowledgments" and §"Closing note".)

---

### Task 1: Rewrite README.md

**Files:**
- Modify: `README.md` (full replacement of existing content)

**Interfaces:**
- Consumes: nothing (no other tasks precede this one)
- Produces: nothing (no other tasks follow this one — this is the entire plan)

- [ ] **Step 1: Replace the full contents of `README.md`**

Write this exact content to `README.md`, overwriting everything currently in the file:

```markdown
# Caracal

A native, GPU-accelerated terminal / SSH / Telnet / serial client built on
[GPUI](https://github.com/zed-industries/zed) (the UI framework behind
[Zed](https://zed.dev)) and [gpui-component](https://github.com/longbridge/gpui-component).

## Features

- **Protocols** — local terminal (native PTY), SSH (password or saved,
  encrypted key auth), Telnet (RFC 854 IAC negotiation: terminal type,
  suppress-go-ahead, echo), and Serial (baud rate, data bits, parity, stop
  bits, flow control).
- **Saved Connections** — a group tree with drag-and-drop reorder within and
  across groups, search, sort, and TOML import/export.
- **SFTP File Browser** — a sortable, multi-column file browser sharing the
  same SSH connection (no second dial), with a right-click context menu
  (rename/properties/delete), a hidden-files toggle, directory history, and
  bidirectional working-directory sync with the terminal.
- **Resource Monitoring** — a live, per-host view of the *remote* machine's
  CPU, memory, network, and disk usage, gathered over the existing SSH
  channel.
- **Quick Commands** — a command drawer for snippets you run often, sent to
  the active terminal as either an immediate execute or an append.
- **Fully configurable keyboard shortcuts** — live rebinding, conflict
  detection, and one-click reset to defaults.
- **Security** — SSH private keys are encrypted at rest (AES-256-GCM, with
  an Argon2id-derived key) behind a master password, with optional OS
  keyring unlock (Keychain / Credential Manager / Secret Service). Passwords
  are always direct-entry and are never persisted.
- **Customization** — 20+ built-in terminal color themes, plus a font
  family/size picker with a bundled Nerd Font and CJK fallback.
- **Internationalization** — English and 简体中文, switchable at runtime.
- **GPU-accelerated rendering** via `wgpu`, with Nerd Font / powerline glyph
  support.

<details>
<summary>中文</summary>

- **协议支持** — 本地终端(原生 PTY)、SSH(密码认证或已保存的加密密钥认证)、
  Telnet(RFC 854 IAC 协商：终端类型、suppress-go-ahead、回显)、串口
  (波特率、数据位、校验位、停止位、流控)。
- **已保存的连接** — 支持分组的连接树，可在组内/跨组拖拽排序，支持搜索、排序
  以及 TOML 格式的导入/导出。
- **SFTP 文件浏览器** — 与 SSH 共用同一条连接(无需二次拨号)的多列可排序文件
  浏览器，支持右键菜单(重命名/属性/删除)、隐藏文件切换、目录历史，以及与
  终端双向同步当前工作目录。
- **资源监控** — 通过已有的 SSH 通道实时查看*远程*主机的 CPU、内存、网络与
  磁盘使用情况(按主机独立展示)。
- **快捷命令** — 用于存放常用命令的抽屉面板，可选择立即执行或追加到当前终端。
- **完全可配置的快捷键** — 支持实时重新绑定、冲突检测，以及一键恢复默认值。
- **安全性** — SSH 私钥在主密码保护下以加密方式静态存储(AES-256-GCM，密钥由
  Argon2id 派生)，并可选启用系统密钥链解锁(Keychain / 凭据管理器 / Secret
  Service)。密码始终为直接输入，从不持久化保存。
- **个性化** — 内置 20+ 款终端配色主题，支持字体族/字号选择，内置 Nerd Font
  与 CJK 回退字体。
- **国际化** — 支持英文与简体中文，可在运行时切换。
- **GPU 加速渲染** — 基于 `wgpu`，支持 Nerd Font / powerline 图标字形。

</details>

## Roadmap

- **Resource Monitoring** — GPU stats, a process manager, and a Docker
  container view.
- **Quick Commands** — categories, search, `{{variable}}` templating, and
  import/export.
- **SSH port forwarding / tunnel management** — local, remote, and dynamic
  (SOCKS) proxy tunnels.
- **Multi-session broadcast input** — type once, send to multiple open tabs
  or panes simultaneously.
- **Session logging & recording** — capture terminal output to a file and
  replay it later.
- **Cloud backup & sync** — back up saved connections and settings (e.g.
  via WebDAV) and sync them across devices.
- **macOS support** — currently Linux and Windows only.

<details>
<summary>中文</summary>

- **资源监控** — GPU 状态、进程管理器、Docker 容器视图。
- **快捷命令** — 分类、搜索、`{{variable}}` 变量模板、导入/导出。
- **SSH 端口转发 / 隧道管理** — 本地、远程以及动态(SOCKS)代理隧道。
- **多会话广播输入** — 一次输入，同时发送到多个已打开的标签页/面板。
- **会话日志与录制** — 将终端输出记录到文件，并可在之后回放。
- **云端备份与同步** — 备份已保存的连接与设置(例如通过 WebDAV)，并在多设备
  间同步。
- **macOS 支持** — 目前仅支持 Linux 与 Windows。

</details>

## Acknowledgments

Caracal owes a debt to the terminal clients that came before it:

- **[WindTerm](https://github.com/kingToolbox/WindTerm)** — for proving how
  far a single native terminal/SSH client can go: sessions, SFTP, serial,
  and resource monitoring, all in one fast, keyboard-driven tool. Caracal's
  saved-connections group tree, its settings-and-shortcuts model, and the
  general shape of "everything in one window" all follow WindTerm's lead.
- **[NyaTerm](https://github.com/nyakang/nyaterm)** — more than an early
  inspiration: NyaTerm is the project Caracal's own roadmap was built
  against feature-by-feature, gap by gap, to decide what to build next.

Thank you both.

<details>
<summary>中文</summary>

Caracal 的很多设计都得益于在它之前出现的终端客户端：

- **[WindTerm](https://github.com/kingToolbox/WindTerm)** — 证明了一款原生
  终端/SSH 客户端能做到多完整：会话管理、SFTP、串口、资源监控，全部集成在
  一个快速、以键盘为核心的工具里。Caracal 的已保存连接分组树、设置与快捷键
  模型，以及"一个窗口容纳一切"的整体思路，都延续了 WindTerm 的方向。
- **[NyaTerm](https://github.com/nyakang/nyaterm)** — 不仅仅是早期灵感来源：
  Caracal 早期的 roadmap 正是逐项对照 NyaTerm 的功能、逐条分析差距而制定的。

谢谢你们。

</details>

## License

Licensed under either of

- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE) or
  <http://www.apache.org/licenses/LICENSE-2.0>)
- MIT license ([LICENSE-MIT](LICENSE-MIT) or
  <http://opensource.org/licenses/MIT>)

at your option.

---

Caracal is built primarily by an engineer with no prior Rust experience,
working with Claude as the main coding collaborator.
```

- [ ] **Step 2: Verify removed sections are gone and required sections are present**

Run:

```bash
grep -n "^## " README.md
grep -c "Building from source\|Prebuilt binaries" README.md
```

Expected: the first command lists exactly four `##` headers, in this order —
`Features`, `Roadmap`, `Acknowledgments`, `License`. The second command
outputs `0` (no leftover references to the removed sections).

- [ ] **Step 3: Verify bilingual folds are present where required**

Run:

```bash
grep -c "<summary>中文</summary>" README.md
```

Expected: `3` (one fold each for Features, Roadmap, Acknowledgments — License
and the closing note intentionally have none).

- [ ] **Step 4: Verify referenced license files still exist**

Run:

```bash
ls LICENSE-APACHE LICENSE-MIT
```

Expected: both files listed, no "No such file" error (confirms the License
section's relative links aren't broken).

- [ ] **Step 5: Commit**

```bash
git add README.md
git commit -m "docs: rewrite README as a feature/roadmap showcase

Reorganizes around Features/Roadmap/Acknowledgments/License, drops the
build-from-source and prebuilt-binaries sections, adds bilingual
(English + Chinese detail-folds) content, expands the WindTerm/NyaTerm
tribute, and adds a closing note on how the project is built."
```
