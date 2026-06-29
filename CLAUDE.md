# CLAUDE.md — Caracal

> **Project name:** Caracal  ← 改这一行即可换名(如改回 nyaterm)
> **What it is:** 原生 GPUI 的 GPU 加速终端 / SSH / serial 客户端,WindTerm 的开源替代。
> Rust + GPUI 渲染。终端**模型层用 `alacritty_terminal` 直连**(不用 `gpui-terminal` wrapper,
> 因为它缺鼠标选区和 scrollback),**渲染/交互层自写**;`gpui-component` 只做外壳布局。
> ⚠️ Zed 的 terminal crate 是 GPL-3.0,**只可读源码参考,不可 vendor 代码**。
>
> 本文件是**常驻约束**。每次开工先读。顺序任务见 `PLAN.md`。

---

## 0. 这是什么 / 不是什么

- 这是一个**进程内原生 Rust 应用**。
- **不是** Tauri、不是 web、没有前后端分离。
- **没有 IPC、没有 command/handler/invoke 间接层。** 前端调后端就是直接函数调用 / entity update。
- 任何 Tauri 心智模型的残留(把操作包成 message 发给 handler 再返回)都是 bug,删掉改直接调用。

---

## 1. 铁律(任何阶段都不许违反)

1. **边界纪律(最重要)**
   - `terminal/` 目录**禁止 import `gpui_component`**。
   - `gpui_component` 只允许出现在 `panels/`。
   - 两个世界唯一相遇点是 `panels/` 里的 `*Panel` adapter;adapter 只做:嵌入内层 entity、标题、**focus 委托**。不写业务逻辑。

2. **API 不许凭记忆写(上次失败主因)**
   - GPUI、`gpui-component`、`russh` 都**不保证 semver,API 经常变**。
   - 每用一个不确定的 API,先看该 crate **当前安装版本**的 `examples/` 和 docs.rs,以实际签名为准。
   - 编译不过时**改一次到位**,不要反复瞎改。**连续编译失败 3 次就停下报告现状**,不要把代码越改越乱。

3. **用 GPUI 范式表达状态**
   - 状态用 `Entity<T>` + `Context` + `cx.notify()`。
   - **不要到处撒 `Arc<Mutex>`**,不要用一堆 channel 模拟本该是 entity 的共享状态。
   - 跨执行上下文(tokio ↔ GPUI)才用 channel,且**只允许在 `terminal/bridge.rs` 内**。

4. **节奏**
   - 一个 phase 一个 commit;按 `PLAN.md` 的验收标准逐条自检,**不达标不进下一阶段**。
   - 每阶段开工先 `cargo build` 确认上阶段干净。

---

## 2. 架构不变量(实现时必须始终成立)

- **后端抽象:** local-pty / ssh / serial 全部实现同一个 `PtyBackend` trait。`TerminalView` 对"后端是什么"**无感**。新增后端不得改 TerminalView / TerminalPanel。
- **桥的本质:** `Bridge` 桥的是**执行模型**,不是数据。它焊接三个上下文:GPUI 主线程 / 读 PTY 的 reader / russh(或 tokio-serial)的 async runtime。async↔blocking 用 `flume`(`recv()` 阻塞、`recv_async().await` 异步,正好骑两边)。
- **唤醒 + 节流:** GPUI 是 pull-based。后台字节进 `Term` 后**必须** `cx.notify()` 才重绘;drain 在 GPUI 侧 `cx.spawn` task 里做。但**直接吃 alacritty 后必须自己做节流**——一帧内多次 grid 变动合并成一次 notify(coalesce / ~16ms),否则 `cat` 大文件刷爆冻屏(zTerm 踩过)。`gpui-terminal` 原本替你做了这件事,现在归你。
- **Term 共享状态:** alacritty 的 `Term` 用 `Arc<FairMutex<Term<Listener>>>` + listener(包 `UnboundedSender`)是其**既定模式**,属唯一豁免——不算违反"少用 Arc<Mutex>"。除此之外不得新增 Arc<Mutex>。
- **选区/scrollback 是自己的责任:** 模型层 `Selection`/`SelectionRange`、`display_offset`/`Scroll` 都现成,GPUI 的鼠标拖选、滚轮翻历史 wiring 要自己写(`selection.rs`/`scrollback.rs`)。selection 坐标换算要把 `display_offset` 算进去。
- **尺寸三同步:** 渲染区 cell metrics 算出的 cols·rows,必须同步到 (a) 本地 grid、(b) 后端(SSH `window_change` / serial 无所谓)。resize 责任在 `TerminalView`,**不在 dock**。Wayland HiDPI 要把 scale factor 算进 cell 尺寸。
- **focus 委托:** `*Panel` 的 `Focusable::focus_handle` 必须返回**内层 entity 的 handle**,不是自己的。返回自己的 = 按键全丢。
- **一个 Session 一条连接:** SSH 的终端 channel 和 SFTP subsystem channel **复用同一条 russh 连接**。不为 SFTP 另开连接。
- **后台 tab 照常 drain:** entity 被 dock 持有就活着,drain task 不停,只是不重绘。flume 用 unbounded 或无条件消费,别让"没人看"时堵死 channel 反压到 IO 线程。
- **theme 两套独立:** root 套 `gpui-component` theme provider(组件依赖);终端自己的 ANSI 调色板独立于它,**别混**。

---

## 3. 目录结构(及边界归属)

```
src/
  main.rs            # 入口:DockArea + theme provider
  workspace.rs       # 顶层状态 + 事件路由(OpenSession -> 新 panel)
  session.rs         # Session:id/host/creds + 共享连接句柄
  panels/            # ★ 唯一允许 import gpui_component
    terminal.rs      #   TerminalPanel(adapter,focus 委托)
    session_list.rs  #   左侧列表,点击 emit OpenSession
    sftp.rs          #   SftpPanel(复用同连接)
  terminal/          # ★ 禁止 gpui_component
    view.rs          #   TerminalView entity:持有 Term + focus,接 mouse/key/scroll
    render.rs        #   自写 GPUI Element/canvas:grid -> 字形,画 selection/光标
    model.rs         #   alacritty_terminal Term 封装 + Listener(UnboundedSender)
    selection.rs     #   鼠标拖动 -> alacritty Selection;复制
    scrollback.rs    #   滚轮/PageUp -> display_offset
    bridge.rs        #   执行模型桥(flume + notify 节流)— 唯一允许跨上下文 channel
    backend.rs       #   PtyBackend trait + Local/Ssh/Serial
    keymap.rs        #   keystroke -> 终端字节(含 DECCKM)
    ssh.rs           #   russh Handler + 专用 tokio runtime 线程
```

边界自检:`grep -r gpui_component src/terminal/` 必须**无结果**。

---

## 4. 终端正确性清单(改 keymap/bridge 时对照)

- 方向键:默认 `ESC [ A/B/C/D`;**DECCKM 开启时 `ESC O A/B/C/D`**(读 `alacritty_terminal` 的 `TermMode::APP_CURSOR`,不写死)。
- Enter=`\r`(不是 `\n`);Ctrl-<x>→control byte(Ctrl-C=0x03);Alt-<x>→ESC 前缀。
- **不本地 echo**(远端会 echo,否则双字符)。
- **bracketed paste:** `Term` 报 `BRACKETED_PASTE` mode 时,粘贴要包 `ESC[200~`/`ESC[201~`。
- SSH **stderr 走 `ChannelMsg::ExtendedData`,不是 `Data`**,别漏。

---

## 5. 版本敏感缝(动到时先核实当前版本,别假设)

- `alacritty_terminal`:用 crates.io 独立版 **0.26+**,不 vendor Zed fork。`Term` 泛型 listener、`Event`/`EventListener`、`Selection`/`SelectionType`、`grid().display_offset()`/`Scroll`、`TermMode` 标志按当前版本核实。
- `russh`:`check_server_key` key 类型路径 / `AuthResult.success()` vs 裸 bool / Handler trait 的 async 形态 / `ChannelMsg` 变体名。
- `gpui-component`:`Panel` / `Focusable` / `DockArea::add_panel` 的方法名与签名,对 `examples/` 核实。
- `gpui`:`on_key_down` / `on_mouse_down` / `on_scroll_wheel` / `track_focus` / `cx.spawn` / 自定义 Element/`canvas` 签名。

---

## 6. 环境

- Arch Linux + KDE Plasma + **Wayland**。注意 WebKit 无关(本项目无 webview);但 GPU/Vulkan 与 Wayland HiDPI 的 cell metrics 要算对。
- 构建:`cargo run` / `cargo build`。系统依赖:base-devel、cmake、clang、Vulkan stack、Wayland libs。

---

## 7. 工作协议(给执行 agent)

1. 先读本文件 + `PLAN.md`,从当前 phase 继续。
2. 不确定的 API → 查当前版本 `examples/` / docs.rs,**不凭记忆**。
3. 改动遵守第 1、2 节铁律和第 2 节不变量。
4. phase 末按 `PLAN.md` 验收标准逐条自检,通过才 commit + 进下一阶段。
5. 连续编译失败 3 次 → 停,报告现状,不要继续乱改。
