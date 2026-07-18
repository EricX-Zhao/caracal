//! 图标：以 gpui-component-assets 自带的 lucide 子集（IconName）为基础，
//! 对集合里没有精确对应的图标用本项目自定义 SVG 补充（assets/icons/）。
//!
//! 自定义 SVG 通过 `Icon::empty().path("icons/xxx.svg")` 引用，其余走 IconName。
//!
//! nyaterm 用 react-icons，部分图标（key-round/lock/history/zap/send/circle-dot）
//! 在自带集合里没有精确对应，按语义就近映射如下（右侧为所选 IconName）：
//!   securityAuth(key-round) → CircleUser   认证/身份
//!   lock                    → EyeOff       隐私/锁定
//!   commandHistory(history) → Undo2        回溯箭头，视觉最接近 history
//!   quickCmd(zap/bolt)      → Asterisk
//!   serialSend(send)        → ArrowRight
//!   recording(circle-dot)   → Play         录制/回放
//!   syncBackup(backup)      → Inbox        归档
//!   sessions(server)        → HardDrive

use gpui_component::{Icon, IconName};

/// 便捷构造：语义图标 → 可作为 `.child(...)` 的 Icon 元素。
/// 需要控制颜色时 `icon(AppIcon::X).text_color(cx.theme().primary)`。
#[inline]
pub fn icon(a: AppIcon) -> Icon {
    match a {
        // 自定义 SVG 图标（上游 lucide 集合里没有精确对应）
        AppIcon::Upload => Icon::empty().path("icons/upload.svg"),
        AppIcon::UploadFolder => Icon::empty().path("icons/folder-up.svg"),
        AppIcon::Download => Icon::empty().path("icons/download.svg"),
        AppIcon::NewFile => Icon::empty().path("icons/file-plus.svg"),
        AppIcon::NewFolder => Icon::empty().path("icons/folder-plus.svg"),
        AppIcon::Refresh => Icon::empty().path("icons/refresh-cw.svg"),
        AppIcon::Delete => Icon::empty().path("icons/trash-2.svg"),
        AppIcon::Pencil => Icon::empty().path("icons/pencil.svg"),
        AppIcon::Sessions => Icon::empty().path("icons/server.svg"),
        // 其余走上游 IconName 映射
        _ => Icon::new(IconName::from(a)),
    }
}

/// 活动栏 / 面板里用到的语义图标。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AppIcon {
    // 左栏
    FileExplorer,
    Network,
    SecurityAuth,
    SyncBackup,
    // 右栏顶部
    Sessions,
    CommandHistory,
    ResourceMonitor,
    AiAssistant,
    // 右栏底部
    QuickCmd,
    SerialSend,
    Record,
    Lock,
    Settings,
    // 文件浏览器工具条
    NewFile,
    NewFolder,
    Upload,
    UploadFolder,
    Download,
    Delete,
    Up,
    Refresh,
    Search,
    Plus,
    Terminal,
    Folder,
    // NyaTerm-style panel icons
    LocalTerminal,
    QuickConnect,
    MoreVertical,
    Copy,
    Pencil,
    Sort,
    ChevronRight,
    ChevronDown,
    Telnet,
    SerialPort,
    // 标题栏窗口控制按钮（Linux/Windows 专用；macOS 用系统原生交通灯按钮）
    WindowMinimize,
    WindowMaximize,
    WindowRestore,
    WindowClose,
}

impl From<AppIcon> for IconName {
    fn from(i: AppIcon) -> Self {
        use AppIcon::*;
        match i {
            FileExplorer | Folder => IconName::Folder,
            Network => IconName::Network,
            SecurityAuth => IconName::CircleUser,
            SyncBackup => IconName::Inbox,
            CommandHistory => IconName::Undo2,
            ResourceMonitor => IconName::ChartPie,
            AiAssistant => IconName::Bot,
            QuickCmd => IconName::Asterisk,
            SerialSend => IconName::ArrowRight,
            Record => IconName::Play,
            Lock => IconName::EyeOff,
            Settings => IconName::Settings,
            NewFile => IconName::File,
            NewFolder => IconName::FolderClosed,
            Delete => IconName::Delete,
            Up => IconName::ArrowUp,
            Refresh => IconName::Redo,
            Search => IconName::Search,
            Plus => IconName::Plus,
            Terminal => IconName::SquareTerminal,
            // NyaTerm-style icons - use available alternatives or custom SVG
            LocalTerminal => IconName::SquareTerminal,
            QuickConnect => IconName::Asterisk,
            MoreVertical => IconName::EllipsisVertical,
            Copy => IconName::Copy,
            Sort => IconName::ChevronsUpDown,
            ChevronRight => IconName::ChevronRight,
            ChevronDown => IconName::ChevronDown,
            Telnet => IconName::Network,
            SerialPort => IconName::Cpu,
            WindowMinimize => IconName::WindowMinimize,
            WindowMaximize => IconName::WindowMaximize,
            WindowRestore => IconName::WindowRestore,
            WindowClose => IconName::WindowClose,
            // Upload / Download / Pencil / Sessions are handled by custom SVG in
            // `icon()`, not reachable here.
            Upload | UploadFolder | Download | Pencil | Sessions => unreachable!(),
        }
    }
}