//! Windows 进程托管边界。桌面壳其余模块不直接调用 Win32 API。

#[cfg(windows)]
pub mod win;
