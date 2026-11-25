//! Windows 后端：通过 Win32 固件环境变量 API 读写 UEFI 变量。
//!
//! 流程：
//! 1. 打开当前进程令牌，调用 `AdjustTokenPrivileges` 启用
//!    `SE_SYSTEM_ENVIRONMENT_NAME`（`SeSystemEnvironmentPrivilege`）特权；
//! 2. 用 `GetFirmwareEnvironmentVariableW` 读取 BootOrder / BootXXXX，
//!    用 `SetFirmwareEnvironmentVariableW` 写入 BootNext；
//! 3. GUID 必须使用带花括号的字符串形式 `{8be4df61-...}`。
//!
//! # 设计原理（Why）
//! 选用 windows-rs 原生 API 而非 shelling out 到 `bcdedit`：bcdedit 的输出
//! 格式随 Windows 版本漂移且需解析本地化文本，原生 API 是稳定的系统契约；
//! 代价是必须自行处理特权启用与缓冲区扩容。
//!
//! 注意：该 API 要求调用进程具有管理员权限（标准用户没有
//! `SeSystemEnvironmentPrivilege`），上层会通过 UAC（runas）自动提权。

#[cfg(target_os = "windows")]
mod imp {
    use std::sync::atomic::{AtomicBool, Ordering};

    use windows::core::{w, PCWSTR};
    use windows::Win32::Foundation::{
        GetLastError, ERROR_ENVVAR_NOT_FOUND, ERROR_INSUFFICIENT_BUFFER, ERROR_NOT_ALL_ASSIGNED,
        HANDLE, LUID,
    };
    use windows::Win32::Security::{
        AdjustTokenPrivileges, LookupPrivilegeValueW, LUID_AND_ATTRIBUTES, SE_PRIVILEGE_ENABLED,
        SE_SYSTEM_ENVIRONMENT_NAME, TOKEN_ADJUST_PRIVILEGES, TOKEN_PRIVILEGES, TOKEN_QUERY,
    };
    use windows::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};
    use windows::Win32::System::WindowsProgramming::{
        GetFirmwareEnvironmentVariableW, SetFirmwareEnvironmentVariableW,
    };

    use janus_core::{
        collect_boot_entries, encode_boot_next, parse_boot_next_var, to_wide_null, BootEntry,
        BootId, BootManager, JanusError, OwnedHandle, Result,
    };

    /// UEFI 全局变量命名空间 GUID 的编译期 UTF-16 常量，避免每次读写变量重复堆分配。
    const EFI_VARIABLE_GUID_WIDE: PCWSTR = w!("{8be4df61-93ca-11d2-aa0d-00e098032b8c}");

    /// 基于 Win32 固件环境变量 API 的启动管理器。
    #[derive(Debug, Clone, Default)]
    pub struct WindowsBootManager;

    impl WindowsBootManager {
        /// 使用当前系统的 UEFI 固件接口。
        pub fn for_running_system() -> Result<Self> {
            Ok(Self)
        }
    }

    /// 进程级特权缓存：SeSystemEnvironmentPrivilege 启用一次后在进程生命周期内持续有效，
    /// 避免遍历 BootXXXX 时反复执行 OpenProcessToken + AdjustTokenPrivileges 系统调用。
    static PRIVILEGE_ENABLED: AtomicBool = AtomicBool::new(false);

    /// 启用 `SeSystemEnvironmentPrivilege` 特权（读写固件变量必需）。
    fn enable_firmware_env_privilege() -> Result<()> {
        if PRIVILEGE_ENABLED.load(Ordering::Acquire) {
            return Ok(());
        }
        unsafe {
            let mut raw_token = HANDLE::default();
            OpenProcessToken(
                GetCurrentProcess(),
                TOKEN_QUERY | TOKEN_ADJUST_PRIVILEGES,
                &mut raw_token,
            )
            .map_err(|e| JanusError::Elevation(format!("OpenProcessToken 失败：{e}")))?;
            let token = OwnedHandle(raw_token);

            let mut luid = LUID::default();
            LookupPrivilegeValueW(PCWSTR::null(), SE_SYSTEM_ENVIRONMENT_NAME, &mut luid)
                .map_err(|e| JanusError::Elevation(format!("LookupPrivilegeValueW 失败：{e}")))?;

            let tp = TOKEN_PRIVILEGES {
                PrivilegeCount: 1,
                Privileges: [LUID_AND_ATTRIBUTES {
                    Luid: luid,
                    Attributes: SE_PRIVILEGE_ENABLED,
                }],
            };
            AdjustTokenPrivileges(token.0, false, Some(&tp), 0, None, None)
                .map_err(|e| JanusError::Elevation(format!("AdjustTokenPrivileges 失败：{e}")))?;

            // AdjustTokenPrivileges 即便返回成功，也可能未实际授予全部特权
            if GetLastError() == ERROR_NOT_ALL_ASSIGNED {
                return Err(JanusError::Elevation(
                    "当前用户没有 SeSystemEnvironmentPrivilege 特权（请以管理员身份运行）".into(),
                ));
            }
        }
        PRIVILEGE_ENABLED.store(true, Ordering::Release);
        Ok(())
    }

    /// 读取一个固件环境变量。变量不存在返回 `None`。
    fn get_var(name: &str) -> Result<Option<Vec<u8>>> {
        enable_firmware_env_privilege()?;
        let name_w = to_wide_null(name);

        // 启动项变量通常不到 1KB；缓冲区不足时按 ERROR_INSUFFICIENT_BUFFER 扩容重试
        let mut size: usize = 1024;
        loop {
            let mut buf = vec![0u8; size];
            let n = unsafe {
                GetFirmwareEnvironmentVariableW(
                    PCWSTR(name_w.as_ptr()),
                    EFI_VARIABLE_GUID_WIDE,
                    Some(buf.as_mut_ptr() as *mut core::ffi::c_void),
                    buf.len() as u32,
                )
            };
            if n > 0 {
                buf.truncate(n as usize);
                return Ok(Some(buf));
            }
            let err = unsafe { GetLastError() };
            if err == ERROR_ENVVAR_NOT_FOUND {
                return Ok(None);
            }
            if err == ERROR_INSUFFICIENT_BUFFER && size < 32768 {
                size *= 4;
                continue;
            }
            return Err(JanusError::EfiVar(format!(
                "读取 {name} 失败（Win32 错误码 {}）",
                err.0
            )));
        }
    }

    /// 写入一个固件环境变量；数据为空时删除该变量。
    fn set_var(name: &str, data: &[u8]) -> Result<()> {
        enable_firmware_env_privilege()?;
        let name_w = to_wide_null(name);
        unsafe {
            SetFirmwareEnvironmentVariableW(
                PCWSTR(name_w.as_ptr()),
                EFI_VARIABLE_GUID_WIDE,
                Some(data.as_ptr() as *const core::ffi::c_void),
                data.len() as u32,
            )
            .map_err(|e| JanusError::EfiVar(format!("写入 {name} 失败：{e}")))
        }
    }

    impl BootManager for WindowsBootManager {
        fn list_entries(&self) -> Result<Vec<BootEntry>> {
            collect_boot_entries(get_var)
        }

        fn boot_next(&self) -> Result<Option<BootId>> {
            match get_var("BootNext")? {
                Some(data) => parse_boot_next_var(&data).map(Some),
                None => Ok(None),
            }
        }

        fn set_boot_next(&self, id: BootId) -> Result<()> {
            set_var("BootNext", &encode_boot_next(id))
        }

        fn clear_boot_next(&self) -> Result<()> {
            // SetFirmwareEnvironmentVariableW 传空数据即删除变量
            set_var("BootNext", &[])
        }
    }
}

// 非 Windows 平台的占位实现，保证 workspace 可编译。
#[cfg(not(target_os = "windows"))]
mod imp {
    use janus_core::{BootEntry, BootId, BootManager, JanusError, Result};

    const UNSUPPORTED: &str = "janus 的 Win32 固件变量后端仅支持 Windows";

    /// 非 Windows 平台的占位实现。
    #[derive(Debug, Clone, Default)]
    pub struct WindowsBootManager;

    impl WindowsBootManager {
        pub fn for_running_system() -> Result<Self> {
            Err(JanusError::Other(UNSUPPORTED.into()))
        }
    }

    impl BootManager for WindowsBootManager {
        fn list_entries(&self) -> Result<Vec<BootEntry>> {
            Err(JanusError::Other(UNSUPPORTED.into()))
        }
        fn boot_next(&self) -> Result<Option<BootId>> {
            Err(JanusError::Other(UNSUPPORTED.into()))
        }
        fn set_boot_next(&self, _id: BootId) -> Result<()> {
            Err(JanusError::Other(UNSUPPORTED.into()))
        }
        fn clear_boot_next(&self) -> Result<()> {
            Err(JanusError::Other(UNSUPPORTED.into()))
        }
    }
}

pub use imp::WindowsBootManager;
