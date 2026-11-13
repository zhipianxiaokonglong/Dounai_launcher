//! 让子进程不弹出控制台窗口。
//!
//! uv、python、curl 都是控制台程序。从 GUI 进程直接 spawn 时，Windows 会给每个
//! 子进程分配一个新的控制台窗口——一次环境准备可能连开好几个黑窗，用户会以为
//! 程序出了故障，或者以为有命令在后台乱跑。
//!
//! `CREATE_NO_WINDOW`（0x0800_0000）让子进程不分配控制台，stdout/stderr 仍走
//! 管道，读输出完全不受影响。
//!
//! 这里写常量而不是引入 `windows-sys`：core 的全部依赖都是可跨平台的纯 Rust
//! crate，为了一个 flag 拉进整包 Windows 绑定不划算；该值自 Windows NT 起未变。

use std::process::Command;

/// 在 Windows 上禁用控制台窗口；其它平台是空操作。
#[cfg(windows)]
pub fn hide_console(command: &mut Command) {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    command.creation_flags(CREATE_NO_WINDOW);
}

#[cfg(not(windows))]
pub fn hide_console(_command: &mut Command) {}
