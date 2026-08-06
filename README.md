# Caracal

A native, GPU-accelerated terminal / SSH / Telnet / serial client built on
[GPUI](https://github.com/zed-industries/zed) (the UI framework behind
[Zed](https://zed.dev)) and [gpui-component](https://github.com/longbridge/gpui-component).

## Features

- **Protocols** — local terminal (native PTY), SSH (password, or saved
  encrypted-key, auth), Telnet (RFC 854 IAC negotiation: terminal type,
  suppress-go-ahead, echo), and Serial (baud rate, data bits, parity, stop
  bits, flow control).
- **Saved Connections** — a group tree with drag-and-drop reorder within and
  across groups, search, sort, and TOML import/export.
- **SFTP File Browser** — a sortable, multi-column file browser sharing the
  same SSH connection (no second dial), with a right-click context menu
  (rename/properties/delete), a hidden-files toggle, directory history, a
  transfer queue, and bidirectional working-directory sync with the
  terminal.
- **Resource Monitoring** — a live, per-host view of the *remote* machine's
  CPU, memory, network, and disk usage (Linux remote hosts), gathered over
  the existing SSH channel.
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
  浏览器，支持右键菜单(重命名/属性/删除)、隐藏文件切换、目录历史、传输队列，
  以及与终端双向同步当前工作目录。
- **资源监控** — 通过已有的 SSH 通道实时查看*远程*主机(仅限 Linux)的 CPU、
  内存、网络与磁盘使用情况(按主机独立展示)。
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

- MIT license ([LICENSE-MIT](LICENSE-MIT) or
  <http://opensource.org/licenses/MIT>)

at your option.

---

Caracal is built primarily by an engineer with no prior Rust experience,
working with Claude as the main coding collaborator.
