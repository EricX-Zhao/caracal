# Caracal 重写计划 (GPUI + alacritty_terminal + gpui-component + russh)

> **项目名:** Caracal(原 nyaterm,名字可改)
> 目标:把 Tauri2 版终端重写为原生 GPUI 应用。终端**模型层直接用 `alacritty_terminal`**
> (不经 `gpui-terminal` 的 wrapper),**渲染/交互层自写**——这样选区、scrollback 不受制于人。
> 外壳(分屏 / 会话列表 / SFTP)用 `gpui-component`。本计划供 Claude Code 分阶段执行。
>
> **为什么不用 gpui-terminal:** 它的 `TerminalView` wrapper 缺鼠标选区和 scrollback 导航,
> 而这两个对 WindTerm 替代品是刚需。底层 `alacritty_terminal` 模型本就支持 selection
> (`Selection`/`SelectionRange`)和 scrollback(`display_offset`/`Scroll`),缺的只是 GPUI 这层 wiring。
> 直接吃 alacritty_terminal、自己接 wiring,比往别人 render 层打补丁更可控。
> 参考实现:**Paneflow**(`Arc<FairMutex<Term<Listener>>>` + `UnboundedSender` listener 模式)、zTerm。
> ⚠️ Zed 自己的 terminal crate 是 **GPL-3.0**,可读源码参考但**不要 vendor 代码**,否则污染授权。

---

## 0. 全局铁律(每个阶段都必须守,违反即视为失败)

1. **边界纪律**
   - `terminal/` 目录**禁止 import `gpui_component`**。
   - `gpui_component` 只允许出现在 `panels/` 目录。
   - 两个世界唯一相遇点是 `panels/` 里的 `*Panel` adapter,adapter 不写业务逻辑,只做嵌入 + 标题 + focus 委托。

2. **不要 port,要 rewrite**
   - 这是进程内原生应用,**没有 IPC、没有 command/handler 间接层**。
   - 严禁保留任何 Tauri `invoke` 风格的抽象。前端调后端就是直接函数调用 / entity update。
   - 看到"把某操作包成一个 message 发给某 handler 再返回"这种结构,删掉,改直接调用。

3. **API churn 处理(上次失败的主因,重点)**
   - GPUI、`gpui-component`、`russh` **都不保证 semver,API 经常变**。
   - **不许凭记忆/猜测写 API**。每用一个不确定的 API,先看该 crate 当前安装版本的 `examples/` 和 `docs.rs`,以实际签名为准。
   - 涉及版本敏感缝(见下)时,先写最小验证再铺开。
   - 编译不过时**不要反复瞎改**;先确认 API 签名,改一次到位。连续修 3 次还不过就停下来报告,不要把代码越改越乱。

4. **状态用 GPUI 范式**
   - 用 `Entity<T>` + `Context` + `cx.notify()` 表达状态。
   - **不要到处撒 `Arc<Mutex>`**、不要用一堆 channel 模拟本该是 entity 的共享状态。
   - 跨执行上下文(tokio ↔ GPUI)才用 channel,且仅在 `Bridge` 内。

5. **节奏**
   - 一个 phase 一个 commit。
   - 每个 phase 末尾有"验收标准",**不达标不进入下一阶段**。
   - 每阶段开始前先 `cargo build` 确认上阶段是干净的。

### 已知版本敏感缝(动到时格外小心)
- `alacritty_terminal`:用 crates.io 上**独立发布的 0.26+**,不要 vendor Zed 的 fork。`Term` 的泛型 listener 参数、`Event`/`EventListener` trait、`Selection`/`SelectionType`、`grid().display_offset()` 与 `Scroll` 的 API 按当前版本核实。
- `russh`:`check_server_key` 的 key 类型路径、`AuthResult.success()` vs 裸 bool、Handler trait 是否 native async fn、`ChannelMsg` 变体名。
- `russh`:stderr 走 `ChannelMsg::ExtendedData`,**不是** `Data`,别漏接。
- `gpui-component`:`Panel` / `Focusable` / `DockArea` 的 trait 方法名与 `add_panel` 签名按版本对 `examples/` 核实。
- `gpui`:`on_key_down`、`on_mouse_down`/`on_mouse_move`/`on_scroll_wheel`、`track_focus`、`cx.spawn`、`canvas`/自定义 Element 的签名。

---

## 目标目录结构

```
src/
  main.rs            # app 入口:装 DockArea、套 theme provider
  workspace.rs       # 顶层状态 + 事件路由(谁开了哪个 session)
  session.rs         # Session 模型:id/host/creds + 一条共享连接句柄
  panels/            # ★ 唯一允许 import gpui_component
    mod.rs
    terminal.rs      # TerminalPanel:包 Entity<TerminalView>
    session_list.rs  # 左侧 dock:点击 emit OpenSession
    sftp.rs          # SftpPanel(后期)
  terminal/          # ★ 禁止出现 gpui_component
    mod.rs
    view.rs          # TerminalView entity:持有 Term + focus,接 mouse/key/scroll 事件
    render.rs        # 自写 GPUI Element/canvas:grid -> 字形;画 selection 高亮、光标
    model.rs         # alacritty_terminal Term 封装 + Listener(UnboundedSender)
    selection.rs     # 鼠标拖动 -> alacritty Selection;复制到剪贴板
    scrollback.rs    # 滚轮/PageUp -> display_offset
    bridge.rs        # PtyBackend ↔ Term 的执行模型桥(flume + 唤醒/notify 节流)
    backend.rs       # PtyBackend trait + LocalPty / Ssh / Serial 实现
    keymap.rs        # GPUI keystroke -> 终端字节序列(含 DECCKM)
    ssh.rs           # russh Handler + tokio runtime 线程
```

---

## Phase 1 — 裸窗口 + 本地 shell 跑通(alacritty_terminal 直连 + 自写 render,不碰 gpui-component,不碰 SSH)

**目的:** 在最简后端上验证"模型驱动 / 渲染 / 唤醒 / 输入 / resize"全对。这是后面一切的地基。
**注意:本阶段不再用 `gpui-terminal`,直接吃 `alacritty_terminal` 并自写渲染。**

**做什么**
- `Cargo.toml`:加 `gpui`、`alacritty_terminal`(crates.io 独立版 0.26+)、`flume`、`tokio`(rt-multi-thread, macros)、`portable-pty`(本地 PTY)。版本用各 crate 当前最新稳定。
- 定义 `PtyBackend` trait:抽象成统一字节流接口,屏蔽后端是 local/ssh/serial。
  ```
  trait PtyBackend: Send {
      // 读:产出字节流(local 可阻塞 read;ssh/serial 内部 async->flume)
      // 写:接收要发往后端的字节
      // resize:把 (cols, rows) 通知后端
  }
  ```
- `LocalPty` backend:用 `portable-pty` spawn 用户默认 shell。
- `model.rs`:封装 `alacritty_terminal::Term`。**按 Paneflow 模式**:`Term` 的 listener 是个薄 newtype,包 `futures UnboundedSender<Event>`;Term 每次 grid 变动 `send_event`,receiver 在 GPUI 主线程。共享状态用 `Arc<FairMutex<Term<Listener>>>`(这是 alacritty 既定模式,**不算违反"少用 Arc<Mutex>"铁律**——它是 Term 本身要求的)。
- `render.rs`:自写 GPUI Element / `canvas`,把 `Term` 的 grid 逐 cell 画成字形(字体 metrics、颜色、光标)。**先只画可见屏,不画 selection,scrollback 留 Phase 3。**
- `view.rs`:`TerminalView` entity 持有 `Arc<FairMutex<Term>>` + `FocusHandle` + backend;render 调 `render.rs`。
- `Bridge`:后台读 PTY 字节 → 喂进 `Term`(`Term::handle_event`/parser);GPUI 侧 `cx.spawn` 的 task 消费 listener 的 `UnboundedReceiver`,**批量 + 节流后 `cx.notify()`**(见下,zTerm 的坑)。
- `main.rs`:开裸 GPUI 窗口,只放 TerminalView,**不引入 gpui-component**。

**必须做对的点**
- **唤醒 + 节流(zTerm 踩过的坑)**:不要每个 PTY event 都 `cx.notify()`——`cat` 大文件会刷爆。drain 时把一帧内的多次 grid 变动**合并成一次 notify**(coalesce / 按 ~16ms 或按批),否则终端冻住。这是直接吃 alacritty 后**新增的关键责任**(gpui-terminal 原本替你做了)。
- **Term ↔ PTY 尺寸**:`Term::resize` 与 PTY 的 `set_size` 必须同步;rows/cols 由渲染区 cell metrics 算。
- **focus**:render 里 `track_focus(&focus_handle)`,否则键盘进不来。
- **resize**:TerminalView 在 render 拿到 measured bounds → cell metrics 算 cols·rows → `Term::resize` + backend resize。Wayland HiDPI 把 scale factor 算进 cell 尺寸。
- **输入**:`on_key_down` → `keymap.rs` 编码 → backend.write。Enter=`\r`,Ctrl 组合→control byte。

**验收标准(逐条手动验)**
- [ ] `cargo run` 出窗口,自写 render 显示本地 shell 提示符,字形/颜色/光标正确。
- [ ] 输出**实时刷新且不冻**:跑 `cat 大文件` 或 `yes | head -n 1000000`,画面流畅不卡死(验节流);`top` 持续动,**不需要晃鼠标才刷新**(验唤醒)。
- [ ] 键盘输入正常,`ls`、Ctrl-C、方向键历史都对。
- [ ] resize 窗口后 `stty size` 的行列与可见区域一致;`htop`/`vim` 不错位。

---

## Phase 2 — 按键编码正确性(DECCKM / Ctrl / Alt)

**目的:** 把 `keymap.rs` 做扎实,这是"能跑但 vim 里方向键废"类 bug 的根。

**做什么 / 必须做对**
- 方向键:默认 `ESC [ A/B/C/D`;**当终端进入 application cursor key mode (DECCKM) 时发 `ESC O A/B/C/D`**。必须读 `alacritty_terminal` 的 `Term` mode(`TermMode::APP_CURSOR`)来决定,不能写死。
- Enter=`\r`;Tab=`\t`;Backspace 按约定(通常 `0x7f`)。
- Ctrl-<x> → control byte(Ctrl-C=0x03, Ctrl-D=0x04 ...)。
- Alt-<x> → ESC 前缀 + 字符。
- Function keys / Home / End / PageUp/Down 的 CSI 序列。
- **不要本地 echo**:SSH/PTY 远端会 echo,本地再 echo 就双字符。

**验收标准**
- [ ] `vim` 里方向键移动正常(这条专门验 DECCKM)。
- [ ] `htop` 方向键选择正常。
- [ ] Ctrl-C 能中断、Ctrl-D 能退出、Ctrl-A/E 行首行尾正常。
- [ ] 中文输入 / 粘贴多字节不乱。

---

## Phase 3 — 选区 + scrollback(自写 render 的核心补完,gpui-terminal 缺的就是这块)

**目的:** 把 gpui-terminal 没做、而 WindTerm 替代品必须有的两件事补上。模型层已支持,只写 GPUI wiring。

**做什么**
- `selection.rs`:
  - `on_mouse_down` 记起点 → `on_mouse_move` 拖动更新 → 用屏幕坐标换算成 grid `Point`,驱动 `alacritty_terminal::Selection`(Simple/Semantic/Lines 三种,先做 Simple + 双击 Semantic 选词 + 三击选行)。
  - `render.rs` 读 selection range,画高亮背景。
  - 选中后复制:取 `Term` 的 selection text → 写系统剪贴板(GPUI clipboard API)。
  - 中键粘贴 / Ctrl-Shift-V:剪贴板 → backend.write(注意 bracketed paste:`Term` 报告 `BRACKETED_PASTE` mode 时包 `ESC[200~`/`ESC[201~`)。
- `scrollback.rs`:
  - `on_scroll_wheel` / PageUp/PageDown → 调 `Term::scroll_display(Scroll::Delta/PageUp/...)`,改 `display_offset`。
  - `render.rs` 按当前 `display_offset` 取要显示的行(不是永远画屏底)。
  - 有新输出时按惯例跳回屏底(除非用户正在向上翻);可加滚动条指示。

**必须做对**
- selection 的坐标换算要把 `display_offset` 算进去(翻上去之后选区对应的是 scrollback 里的行)。
- 选区高亮 + 光标 + 文本三者 z-order / 颜色优先级别打架。
- 翻 scrollback 时**后台 drain 不能停**,数据继续进 Term 的 history,只是不跟着跳。

**验收标准**
- [ ] 鼠标拖动选中文本,高亮正确;双击选词、三击选行。
- [ ] 选中即复制 / 快捷键复制,粘贴到别处内容对。
- [ ] 向 `vim`/支持 bracketed paste 的程序粘贴多行,不触发自动缩进灾难(验 bracketed paste)。
- [ ] 滚轮/PageUp 能翻看历史输出;有新输出时正确跳回屏底。
- [ ] 翻到历史中间时选区仍对应正确的历史行。

---

## Phase 4 — SSH backend(russh + 专用 tokio runtime)

**目的:** `Ssh` backend 接进同一个 `PtyBackend`,裸窗口里跑通一条 SSH 会话。

**做什么**
- `ssh.rs`:russh `Handler` 实现 + 一条**专用 OS 线程跑 tokio runtime**,russh 整个生命周期关在里面。
- `Ssh` backend:
  - tokio 侧:`channel.wait().await` 取 `ChannelMsg::Data` → `flume_tx.send()`(出);drain 反向 flume → `channel.data().await`(入)。
  - **接 `ChannelMsg::ExtendedData`(stderr),别只接 Data。**
  - resize → `channel.window_change(cols, rows, 0, 0)`。
- `Session` 模型:`id / host / port / 认证信息 + 一条 russh 连接句柄`。**一个 Session 一条连接**,后面 SFTP 复用它。

**版本敏感缝(动手前先核实当前 russh 版本)**
- `check_server_key` 的 key 类型;先实现成接受/打印指纹,TOFU 后做。
- `AuthResult.success()` vs 裸 bool。
- Handler trait 的 async 形态。

**验收标准**
- [ ] 裸窗口连上一台真实 SSH 主机,显示远端 shell。
- [ ] 远端 `top` 实时刷新(再次验唤醒在 ssh 路径也对)。
- [ ] 远端 `stty size` == 本地可见行列;resize 窗口后远端 `vim` 跟着重排(验 window_change)。
- [ ] stderr 输出可见(如 `ls /nonexistent`)。
- [ ] 断开连接有干净的错误提示,不 panic。

---

## Phase 5 — 引入 gpui-component 外壳(DockArea + TerminalPanel)

**目的:** 这才第一次引入 `gpui-component`,且只做外壳。terminal/ 一行都不改。

**做什么**
- `main.rs`:root 套 **gpui-component 的 theme provider**(组件依赖它的 Theme context;终端自己的 ANSI 调色板独立,两套别混)。
- `panels/terminal.rs`:`TerminalPanel { terminal: Entity<TerminalView> }`
  - `Render` 只把内层 entity 嵌进去,**不写逻辑**。
  - **`Focusable::focus_handle` 必须 delegate 给内层 TerminalView 的 handle**(返回自己的 handle = 按键全丢)。
  - 实现 `Panel` trait(panel_name / title 显示 host;其余 seam 先 minimal)。
- `workspace.rs`:`DockArea` 装一个 TerminalPanel 当中央 tab。
- 对 `Panel` / `Focusable` / `add_panel` 的具体签名,**先看 gpui-component 当前版本 examples/**。

**验收标准**
- [ ] 终端现在跑在 dock 的 panel 里,外观正常。
- [ ] **焦点仍在终端**:点 panel 后键盘直接生效(验 focus 委托)。
- [ ] Phase 1–4 的所有行为(刷新/选区/scrollback/输入/resize/SSH)在 panel 里**完全不退化**。
- [ ] dock 拉伸 panel 时,终端 resize + window_change 仍正确(resize 责任在 TerminalView,不在 dock)。

---

## Phase 6 — 会话列表 + 多 tab + 后台 drain

**做什么**
- `panels/session_list.rs`:左侧 dock,列出已配置主机,点击 `emit OpenSession`。
- `workspace.rs`:`cx.subscribe` 接 `OpenSession` → 新建 `TerminalView` → 包成 `TerminalPanel` → `add_panel` 到中央做新 tab。
- 多会话并存。

**必须做对**
- **后台 tab 的终端照常 drain**:entity 被 dock 持有就活着,`cx.spawn` 的 drain task 不停,数据持续进 model,只是不重绘,切回来即最新。
- flume 用 unbounded 或 drain task 无条件消费,**别让"没人看"时把 channel 堵死**反压到 ssh 线程。

**验收标准**
- [ ] 开多个会话成多个 tab,互不干扰。
- [ ] 在后台 tab 跑 `ping` / 长输出,切回来内容是连续的、最新的(验后台 drain)。
- [ ] 关闭 tab 时对应 SSH 连接干净释放(无僵尸线程/连接泄漏)。

---

## Phase 7 — SFTP 面板(复用同一连接)

**做什么**
- `panels/sftp.rs`:`SftpPanel`,在**对应 Session 的同一条 russh 连接**上开 sftp subsystem channel(**不要为 SFTP 另开一条连接**)。
- 文件列表浏览 / 下载 / 上传最小可用。
- 同样:SftpView 自包含 entity,SftpPanel adapter 只做 focus 委托 + 标题。

**验收标准**
- [ ] SFTP 面板与终端共用一条连接(`netstat`/服务端看只有一条会话)。
- [ ] 列目录、下载、上传可用。

---

## Phase 8 — Serial backend(同一个 PtyBackend)

**做什么**
- `Serial` backend:`tokio-serial`(async,**同样的阻抗问题**),走与 SSH 相同的 tokio+flume 桥模式。
- 因为 `PtyBackend` 已抽象,TerminalView / TerminalPanel **一行不用改**,只新增一个 backend + 一种"打开 serial 会话"的入口。

**验收标准**
- [ ] 连一个真实串口设备(波特率可配),收发正常。
- [ ] 与 SSH 会话能并存于不同 tab。

---

## 完成态自检清单(全绿才算重写成功)

- [ ] `terminal/` 内 `grep -r gpui_component` 无结果(边界守住)。
- [ ] **未依赖 `gpui-terminal`,也未 vendor Zed terminal 代码**(直接 `alacritty_terminal` + 自写 render,授权干净)。
- [ ] 全工程无任何 `invoke`/command 风格间接层(无残留 Tauri 心智模型)。
- [ ] 跨上下文 channel 仅存在于 `bridge.rs`;`Arc<FairMutex<Term>>` 仅作为 alacritty 既定模式存在,无其他滥用 `Arc<Mutex>`。
- [ ] local / ssh / serial 三后端共用同一 `PtyBackend`,TerminalView 对后端无感。
- [ ] 输出实时刷新无需鼠标触发(唤醒);`cat` 大文件不冻(notify 节流);选区/scrollback 可用;vim 方向键正常(DECCKM);resize 远端跟随(window_change);panel 内焦点正确(focus 委托)。

---

## 给执行 agent 的开场指令(建议这样起手)

> 按本计划从 Phase 1 开始,逐阶段实现。终端模型用 `alacritty_terminal` 直连 + 自写 render,
> **不要用 gpui-terminal、不要 vendor Zed terminal(GPL)**。每阶段开始先 `cargo build` 确认上阶段干净;
> 每阶段结束按"验收标准"逐条自检并 commit。遇到任何 GPUI / alacritty_terminal / gpui-component / russh
> 的不确定 API,先查该 crate 当前安装版本的 examples/ 和 docs.rs,**不要凭记忆写**。
> 不达验收标准不要进入下一阶段;连续编译失败 3 次就停下来报告现状,不要把代码越改越乱。
