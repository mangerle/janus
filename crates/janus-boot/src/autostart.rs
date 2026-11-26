//! 跨平台开机自启管理（Linux XDG Autostart / Windows HKCU Run 注册表）。
//!
//! # 设计原理（Why）
//! - Linux：按 XDG Autostart 规范读写 `$XDG_CONFIG_HOME/autostart/com.github.mangerle.janus.desktop`，
//!   普通用户权限即可生效，且与托盘以普通用户运行的模型一致。
//! - Windows：读写 `HKCU\Software\Microsoft\Windows\CurrentVersion\Run` 下的 `Janus Boot` 值，
//!   标准用户权限即可设置（代价是用户启用 UAC 时登录拉起 `janus-boot tray` 会弹一次 UAC 确认）。

/// 查询当前用户是否已开启开机自启。
///
/// # Errors
/// 无法定位用户配置目录或读取系统自启状态失败时返回错误。
pub(crate) fn is_enabled() -> anyhow::Result<bool> {
    #[cfg(target_os = "linux")]
    {
        linux::is_enabled()
    }
    #[cfg(target_os = "windows")]
    {
        windows_impl::is_enabled()
    }
    #[cfg(not(any(target_os = "linux", target_os = "windows")))]
    {
        Err(anyhow::anyhow!("此平台暂不支持开机自启"))
    }
}

/// 开启或关闭开机自启（以托盘模式 `tray` 启动当前可执行文件）。
///
/// # Errors
/// 无法定位当前可执行文件、无权限写入自启目录或注册表失败时返回错误。
pub(crate) fn set_enabled(enabled: bool) -> anyhow::Result<()> {
    #[cfg(target_os = "linux")]
    {
        linux::set_enabled(enabled)
    }
    #[cfg(target_os = "windows")]
    {
        windows_impl::set_enabled(enabled)
    }
    #[cfg(not(any(target_os = "linux", target_os = "windows")))]
    {
        let _ = enabled;
        Err(anyhow::anyhow!("此平台暂不支持开机自启"))
    }
}

#[cfg(target_os = "linux")]
mod linux {
    use std::fs;
    use std::io::{ErrorKind, Write as _};
    use std::path::{Path, PathBuf};

    use anyhow::Context;

    use crate::config::xdg_config_root;

    const DESKTOP_FILENAME: &str = "com.github.mangerle.janus.desktop";

    fn autostart_dir() -> anyhow::Result<PathBuf> {
        Ok(xdg_config_root()?.join("autostart"))
    }

    pub(super) fn is_enabled() -> anyhow::Result<bool> {
        is_enabled_in(&autostart_dir()?)
    }

    pub(super) fn set_enabled(enabled: bool) -> anyhow::Result<()> {
        let dir = autostart_dir()?;
        let exe = std::env::current_exe().context("无法定位当前可执行文件路径")?;
        set_enabled_in(&dir, &exe, enabled)
    }

    fn is_enabled_in(dir: &Path) -> anyhow::Result<bool> {
        let path = dir.join(DESKTOP_FILENAME);
        match fs::read_to_string(&path) {
            Ok(content) => {
                // 若桌面文件显式声明 Hidden=true 或 X-GNOME-Autostart-enabled=false，视为未开启
                let disabled = content.lines().any(|line| {
                    let trimmed = line.trim();
                    trimmed.eq_ignore_ascii_case("Hidden=true")
                        || trimmed.eq_ignore_ascii_case("X-GNOME-Autostart-enabled=false")
                });
                Ok(!disabled)
            }
            Err(e) if e.kind() == ErrorKind::NotFound => Ok(false),
            Err(e) => Err(e).with_context(|| format!("读取自启配置文件失败：{}", path.display())),
        }
    }

    fn set_enabled_in(dir: &Path, exe: &Path, enabled: bool) -> anyhow::Result<()> {
        let path = dir.join(DESKTOP_FILENAME);
        if !enabled {
            match fs::remove_file(&path) {
                Ok(()) => return Ok(()),
                Err(e) if e.kind() == ErrorKind::NotFound => return Ok(()),
                Err(e) => {
                    return Err(e)
                        .with_context(|| format!("移除自启配置文件失败：{}", path.display()));
                }
            }
        }

        fs::create_dir_all(dir).with_context(|| format!("创建自启目录失败：{}", dir.display()))?;
        let content = render_desktop_entry(exe);
        let mut tmp = tempfile::NamedTempFile::new_in(dir)
            .with_context(|| format!("创建临时自启文件失败：{}", dir.display()))?;
        tmp.write_all(content.as_bytes())
            .with_context(|| format!("写入临时自启文件失败：{}", tmp.path().display()))?;
        tmp.persist(&path)
            .map_err(|e| e.error)
            .with_context(|| format!("写入自启配置文件失败：{}", path.display()))?;
        Ok(())
    }

    /// 按 XDG Desktop Entry 规范转义 `Exec` 字段中的可执行路径。
    fn escape_exec_arg(path: &Path) -> String {
        let raw = path.to_string_lossy();
        let mut out = String::with_capacity(raw.len() + 2);
        out.push('"');
        for ch in raw.chars() {
            if matches!(ch, '"' | '`' | '$' | '\\') {
                out.push('\\');
            }
            out.push(ch);
        }
        out.push('"');
        out
    }

    fn render_desktop_entry(exe: &Path) -> String {
        let exec = escape_exec_arg(exe);
        // 不写入 TryExec：XDG 规范中 TryExec 不支持引号包裹含空格路径，省略可避免误判失效
        format!(
            "[Desktop Entry]\n\
             Type=Application\n\
             Version=1.0\n\
             Name=Janus Boot\n\
             GenericName=UEFI Boot Switcher\n\
             Comment=写入 UEFI BootNext 并重启，一键进入另一个系统\n\
             Exec={exec} tray\n\
             Icon=com.github.mangerle.janus\n\
             Terminal=false\n\
             X-GNOME-Autostart-enabled=true\n"
        )
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn linux_autostart_enable_disable_roundtrip() {
            let dir = tempfile::tempdir().expect("创建临时目录失败");
            let exe = Path::new("/opt/janus-boot app/janus-boot");

            assert!(!is_enabled_in(dir.path()).unwrap());

            set_enabled_in(dir.path(), exe, true).unwrap();
            assert!(is_enabled_in(dir.path()).unwrap());

            let content = fs::read_to_string(dir.path().join(DESKTOP_FILENAME)).unwrap();
            assert!(content.contains(r#"Exec="/opt/janus-boot app/janus-boot" tray"#));

            set_enabled_in(dir.path(), exe, false).unwrap();
            assert!(!is_enabled_in(dir.path()).unwrap());
            // 重复关闭应静默成功
            set_enabled_in(dir.path(), exe, false).unwrap();
        }

        #[test]
        fn linux_autostart_respects_hidden_flag() {
            let dir = tempfile::tempdir().expect("创建临时目录失败");
            fs::write(
                dir.path().join(DESKTOP_FILENAME),
                "[Desktop Entry]\nHidden=true\n",
            )
            .unwrap();
            assert!(!is_enabled_in(dir.path()).unwrap());
        }
    }
}

#[cfg(target_os = "windows")]
mod windows_impl {
    use anyhow::Context;
    use janus_core::to_wide_null;
    use windows::core::{w, PCWSTR};
    use windows::Win32::Foundation::{ERROR_FILE_NOT_FOUND, ERROR_SUCCESS};
    use windows::Win32::System::Registry::{
        RegDeleteKeyValueW, RegGetValueW, RegSetKeyValueW, HKEY_CURRENT_USER, REG_SZ, RRF_RT_REG_SZ,
    };

    const RUN_SUBKEY: PCWSTR = w!(r"Software\Microsoft\Windows\CurrentVersion\Run");
    const VALUE_NAME: PCWSTR = w!("Janus Boot");
    const LEGACY_VALUE_NAME: PCWSTR = w!("Janus");

    pub(super) fn is_enabled() -> anyhow::Result<bool> {
        let status = unsafe {
            RegGetValueW(
                HKEY_CURRENT_USER,
                RUN_SUBKEY,
                VALUE_NAME,
                RRF_RT_REG_SZ,
                None,
                None,
                None,
            )
        };
        if status == ERROR_SUCCESS {
            return Ok(true);
        }
        if status != ERROR_FILE_NOT_FOUND {
            return Err(anyhow::anyhow!(
                "查询 Windows 启动项注册表失败（错误码 {}）",
                status.0
            ));
        }

        // 兼容重命名前写入的旧值，避免升级后误报为“未开启”
        let legacy_status = unsafe {
            RegGetValueW(
                HKEY_CURRENT_USER,
                RUN_SUBKEY,
                LEGACY_VALUE_NAME,
                RRF_RT_REG_SZ,
                None,
                None,
                None,
            )
        };
        match legacy_status {
            ERROR_SUCCESS => Ok(true),
            ERROR_FILE_NOT_FOUND => Ok(false),
            status => Err(anyhow::anyhow!(
                "查询 Windows 旧启动项注册表失败（错误码 {}）",
                status.0
            )),
        }
    }

    pub(super) fn set_enabled(enabled: bool) -> anyhow::Result<()> {
        if !enabled {
            delete_run_value(VALUE_NAME)?;
            delete_run_value(LEGACY_VALUE_NAME)?;
            return Ok(());
        }

        let exe = std::env::current_exe().context("无法定位当前可执行文件路径")?;
        let command = format!("\"{}\" tray", exe.display());
        let wide = to_wide_null(&command);
        let byte_len = std::mem::size_of_val(wide.as_slice()) as u32;
        let status = unsafe {
            RegSetKeyValueW(
                HKEY_CURRENT_USER,
                RUN_SUBKEY,
                VALUE_NAME,
                REG_SZ.0,
                Some(wide.as_ptr() as *const core::ffi::c_void),
                byte_len,
            )
        };
        if status != ERROR_SUCCESS {
            return Err(anyhow::anyhow!(
                "写入 Windows 启动项注册表失败（错误码 {}）",
                status.0
            ));
        }
        if let Err(e) = delete_run_value(LEGACY_VALUE_NAME) {
            log::warn!("删除 Windows 旧自启项失败（新自启项已生效，忽略）：{e:#}");
        }
        Ok(())
    }

    /// 删除指定名称的 HKCU Run 值；值本就不存在时视为成功。
    fn delete_run_value(name: PCWSTR) -> anyhow::Result<()> {
        let status = unsafe { RegDeleteKeyValueW(HKEY_CURRENT_USER, RUN_SUBKEY, name) };
        if status == ERROR_SUCCESS || status == ERROR_FILE_NOT_FOUND {
            return Ok(());
        }
        Err(anyhow::anyhow!(
            "删除 Windows 启动项注册表失败（错误码 {}）",
            status.0
        ))
    }
}
