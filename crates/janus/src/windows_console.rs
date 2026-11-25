//! Windows GUI 子系统下的控制台附加与错误弹窗。
//!
//! # 设计原理（Why）
//! 托盘形态要求双击或快捷方式启动时完全不出现控制台黑窗，因此主程序以
//! `windows` 子系统链接（见 `main.rs` 的 `windows_subsystem` 属性）。命令行
//! 形态则通过 `AttachConsole(ATTACH_PARENT_PROCESS)` 挂回父终端：父进程没有
//! 控制台（资源管理器/快捷方式）时附加失败，也不创建新控制台。
//!
//! 代价：进程不再是控制台子进程，cmd/PowerShell 不会等待其退出（输出仍写入
//! 同一控制台）——这是换取托盘零黑窗的必要权衡；错误在无控制台场景改用
//! 弹窗兜底，避免托盘启动失败时静默无感知。

use windows::core::{w, PCWSTR};
use windows::Win32::Storage::FileSystem::{
    CreateFileW, FILE_ATTRIBUTE_NORMAL, FILE_GENERIC_READ, FILE_GENERIC_WRITE, FILE_SHARE_READ,
    FILE_SHARE_WRITE, OPEN_EXISTING,
};
use windows::Win32::System::Console::{
    AttachConsole, GetStdHandle, SetStdHandle, ATTACH_PARENT_PROCESS, STD_ERROR_HANDLE, STD_HANDLE,
    STD_INPUT_HANDLE, STD_OUTPUT_HANDLE,
};
use windows::Win32::UI::WindowsAndMessaging::{MessageBoxW, MB_ICONERROR, MB_OK, MB_SETFOREGROUND};

/// 尝试附加到父进程（终端）的控制台；成功返回 true。
/// 父进程无控制台（资源管理器双击、快捷方式启动）时返回 false，且不新建窗口。
pub(crate) fn attach_parent_console() -> bool {
    // SAFETY: 仅调用控制台/文件 Win32 API，句柄由系统管理；
    // 失败路径全部静默降级为"无控制台"，不产生未定义行为
    unsafe {
        if AttachConsole(ATTACH_PARENT_PROCESS).is_err() {
            return false;
        }
        ensure_std_handle(STD_OUTPUT_HANDLE, w!("CONOUT$"));
        ensure_std_handle(STD_ERROR_HANDLE, w!("CONOUT$"));
        ensure_std_handle(STD_INPUT_HANDLE, w!("CONIN$"));
        true
    }
}

/// 标准句柄缺失时重新打开控制台设备并回填。
///
/// GUI 子系统进程即使附加了控制台，标准句柄仍可能为空，不处理会导致
/// `println!`/日志写入静默失败。已存在的句柄（含 cmd 重定向）保持原样，
/// 避免覆盖用户的重定向目标。
fn ensure_std_handle(which: STD_HANDLE, device: PCWSTR) {
    // SAFETY: GetStdHandle/CreateFileW/SetStdHandle 均为纯 Win32 调用；
    // 返回的句柄交由进程标准句柄表持有，无需在此关闭
    unsafe {
        if GetStdHandle(which).is_ok() {
            return;
        }
        let access = FILE_GENERIC_READ.0 | FILE_GENERIC_WRITE.0;
        let share = FILE_SHARE_READ | FILE_SHARE_WRITE;
        if let Ok(handle) = CreateFileW(
            device,
            access,
            share,
            None,
            OPEN_EXISTING,
            FILE_ATTRIBUTE_NORMAL,
            None,
        ) {
            let _ = SetStdHandle(which, handle);
        }
    }
}

/// 无控制台时以弹窗展示错误，保证托盘启动失败等致命问题用户可感知。
pub(crate) fn show_error_dialog(message: &str) {
    let message_w = janus_core::to_wide_null(message);
    let title_w = janus_core::to_wide_null("Janus 错误");
    // SAFETY: 传入的字符串缓冲区在调用期间保持存活且以 NUL 结尾
    unsafe {
        MessageBoxW(
            None,
            PCWSTR(message_w.as_ptr()),
            PCWSTR(title_w.as_ptr()),
            MB_OK | MB_ICONERROR | MB_SETFOREGROUND,
        );
    }
}
