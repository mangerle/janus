//! Linux 后端：直接读写内核 efivarfs（`/sys/firmware/efi/efivars/`）。
//!
//! 关键细节：
//! - efivarfs 中每个变量对应一个文件，内容前 4 字节为变量属性位
//!   （NV | BS | RT），之后才是变量数据；
//! - BootXXXX 的数据是 EFI_LOAD_OPTION 结构，描述符为 UTF-16LE 编码；
//! - 较新的内核会给变量文件设置 immutable（`FS_IMMUTABLE_FL`）标志，
//!   必须先用 `FS_IOC_SETFLAGS` ioctl 清除该标志才能写入/删除，
//!   否则会报 `Operation not permitted`。
//!
//! # 设计原理（Why）
//! 直接用 std::fs 而非 efivar crate：efivarfs 的读写路径足够简单（读文件去
//! 属性头 / 写文件补属性头），自实现换来两点收益——单测可用临时目录完整
//! 覆盖写入路径（无需真机固件），以及 immutable 标志处理策略完全可控。
//! 代价是需要自己维护 ioctl 常量（选用 asm-generic 稳定 ABI 编号）。

#[cfg(target_os = "linux")]
mod imp {
    use std::fs::{self, File, OpenOptions};
    use std::io::Write;
    use std::os::unix::io::AsRawFd;
    use std::path::{Path, PathBuf};

    use janus_core::{
        collect_boot_entries, encode_boot_next, parse_boot_next_var, BootEntry, BootId,
        BootManager, JanusError, Result, EFI_VARIABLE_BOOT_ACCESS, EFI_VARIABLE_GUID,
    };

    /// 运行系统的 efivarfs 挂载点。
    const EFIVARS_DIR: &str = "/sys/firmware/efi/efivars";

    // asm-generic/ioctls.h 中 _IOR('f', 1, long) / _IOW('f', 2, long) 的编译期编码，
    // 按当前目标架构的 c_long 字节数自动适配 32 位与 64 位 Linux（0x8004_6601 / 0x8008_6601）
    const FS_IOC_LONG_SIZE: libc::c_ulong = std::mem::size_of::<libc::c_long>() as libc::c_ulong;
    const FS_IOC_GETFLAGS: libc::c_ulong =
        (2 << 30) | (FS_IOC_LONG_SIZE << 16) | ((b'f' as libc::c_ulong) << 8) | 1;
    const FS_IOC_SETFLAGS: libc::c_ulong =
        (1 << 30) | (FS_IOC_LONG_SIZE << 16) | ((b'f' as libc::c_ulong) << 8) | 2;
    /// ext/ethtool 头文件中的 FS_IMMUTABLE_FL
    const FS_IMMUTABLE_FL: libc::c_ulong = 0x0000_0010;

    /// 基于 efivarfs 的启动管理器。
    #[derive(Debug, Clone)]
    pub struct LinuxBootManager {
        efivars_dir: PathBuf,
    }

    impl LinuxBootManager {
        /// 使用运行系统的 efivarfs（`/sys/firmware/efi/efivars`）。
        ///
        /// # Errors
        /// 目录不存在（BIOS/Legacy 启动或 efivarfs 未挂载）时返回
        /// [`JanusError::NotUefiSystem`]。
        pub fn for_running_system() -> Result<Self> {
            let dir = Path::new(EFIVARS_DIR);
            if !dir.is_dir() {
                return Err(JanusError::NotUefiSystem(
                    "未找到 /sys/firmware/efi/efivars（系统可能以 BIOS/Legacy 模式启动，或 efivarfs 未挂载）"
                        .into(),
                ));
            }
            Ok(Self {
                efivars_dir: dir.to_path_buf(),
            })
        }

        /// 使用指定目录作为变量存储（供单元测试与模拟）。
        pub fn with_efivars_dir(dir: impl Into<PathBuf>) -> Self {
            Self {
                efivars_dir: dir.into(),
            }
        }

        fn var_path(&self, name: &str) -> PathBuf {
            self.efivars_dir.join(format!("{name}-{EFI_VARIABLE_GUID}"))
        }

        fn check_dir(&self) -> Result<()> {
            if !self.efivars_dir.is_dir() {
                return Err(JanusError::NotUefiSystem(format!(
                    "efivarfs 目录不存在：{}",
                    self.efivars_dir.display()
                )));
            }
            Ok(())
        }

        fn map_io(e: std::io::Error) -> JanusError {
            if e.kind() == std::io::ErrorKind::PermissionDenied {
                JanusError::PermissionDenied
            } else {
                JanusError::Io(e)
            }
        }

        /// 读取一个 UEFI 变量（返回去掉 4 字节属性头后的数据）。
        /// 变量不存在时返回 `None`。
        fn read_var(&self, name: &str) -> Result<Option<Vec<u8>>> {
            let raw = match fs::read(self.var_path(name)) {
                Ok(raw) => raw,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
                Err(e) => return Err(Self::map_io(e)),
            };
            if raw.len() < 4 {
                return Err(JanusError::EfiVar(format!(
                    "{name}：数据不足（缺少 4 字节属性头）"
                )));
            }
            Ok(Some(raw[4..].to_vec()))
        }

        /// 写入一个 UEFI 变量（自动补 4 字节属性头，并处理 immutable 标志）。
        fn write_var(&self, name: &str, attrs: u32, data: &[u8]) -> Result<()> {
            self.check_dir()?;
            let path = self.var_path(name);
            let mut payload = Vec::with_capacity(4 + data.len());
            payload.extend(attrs.to_le_bytes());
            payload.extend(data);

            // 先尝试独占创建，已存在时再清除 immutable 并截断覆盖，消除 exists() 检查的 TOCTOU
            let mut file = match OpenOptions::new().write(true).create_new(true).open(&path) {
                Ok(f) => f,
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                    // 内核的防误删保护：先清除 immutable 标志，否则 EPERM
                    clear_immutable(&path);
                    OpenOptions::new()
                        .write(true)
                        .truncate(true)
                        .open(&path)
                        .map_err(Self::map_io)?
                }
                Err(e) => return Err(Self::map_io(e)),
            };
            file.write_all(&payload).map_err(Self::map_io)?;
            Ok(())
        }

        /// 删除一个 UEFI 变量（不存在时静默成功）。
        fn delete_var(&self, name: &str) -> Result<()> {
            self.check_dir()?;
            let path = self.var_path(name);
            clear_immutable(&path);
            match fs::remove_file(&path) {
                Ok(()) => Ok(()),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
                Err(e) => Err(Self::map_io(e)),
            }
        }

        /// 按 BootOrder 顺序列出启动项；缺失或无法解析的条目静默跳过。
        fn list_entries_ordered(&self) -> Result<Vec<BootEntry>> {
            self.check_dir()?;
            collect_boot_entries(|name| self.read_var(name))
        }
    }

    impl BootManager for LinuxBootManager {
        fn list_entries(&self) -> Result<Vec<BootEntry>> {
            self.list_entries_ordered()
        }

        fn boot_next(&self) -> Result<Option<BootId>> {
            self.check_dir()?;
            match self.read_var("BootNext")? {
                Some(data) => parse_boot_next_var(&data).map(Some),
                None => Ok(None),
            }
        }

        fn set_boot_next(&self, id: BootId) -> Result<()> {
            self.write_var("BootNext", EFI_VARIABLE_BOOT_ACCESS, &encode_boot_next(id))
        }

        fn clear_boot_next(&self) -> Result<()> {
            self.delete_var("BootNext")
        }
    }

    /// 尽力清除 efivarfs 变量文件的 immutable 标志。
    ///
    /// 较新内核给 efivarfs 文件加了 `FS_IMMUTABLE_FL`，直接写入/删除会得到
    /// EPERM；这里通过 `FS_IOC_GETFLAGS` / `FS_IOC_SETFLAGS` ioctl 清除。
    /// ioctl 不支持或不生效时静默忽略，由后续的写入步骤暴露真实错误。
    fn clear_immutable(path: &Path) {
        let Ok(file) = File::open(path) else {
            return;
        };
        let fd = file.as_raw_fd();
        let mut flags: libc::c_ulong = 0;
        unsafe {
            if libc::ioctl(fd, FS_IOC_GETFLAGS, &mut flags) != 0 {
                return; // 文件系统不支持该 ioctl（如 tmpfs 的某些场景）
            }
            if flags & FS_IMMUTABLE_FL != 0 {
                let new_flags = flags & !FS_IMMUTABLE_FL;
                let _ = libc::ioctl(fd, FS_IOC_SETFLAGS, &new_flags);
            }
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use janus_core::{encode_load_option, LOAD_OPTION_ACTIVE};
        use std::fs;

        /// 在临时目录中写一个模拟的 efivarfs 变量文件（含 4 字节属性头）。
        fn write_var_file(dir: &Path, name: &str, attrs: u32, data: &[u8]) {
            let mut v = attrs.to_le_bytes().to_vec();
            v.extend(data);
            fs::write(dir.join(format!("{name}-{EFI_VARIABLE_GUID}")), v).unwrap();
        }

        #[test]
        fn list_entries_follows_boot_order_and_skips_invalid() {
            let dir = tempfile::tempdir().unwrap();
            write_var_file(
                dir.path(),
                "BootOrder",
                0x7,
                &[0x02, 0x00, 0x01, 0x00, 0x04, 0x00], // 2, 1, 4
            );
            write_var_file(
                dir.path(),
                "Boot0001",
                0x7,
                &encode_load_option(LOAD_OPTION_ACTIVE, "Windows Boot Manager", &[1, 2, 3, 4]),
            );
            write_var_file(
                dir.path(),
                "Boot0002",
                0x7,
                &encode_load_option(LOAD_OPTION_ACTIVE, "ubuntu", &[]),
            );
            // Boot0003：未列入 BootOrder，应被忽略
            write_var_file(
                dir.path(),
                "Boot0003",
                0x7,
                &encode_load_option(LOAD_OPTION_ACTIVE, "orphan", &[]),
            );
            // Boot0004：在 BootOrder 中但数据损坏，应被跳过
            write_var_file(dir.path(), "Boot0004", 0x7, &[0xAB, 0xCD, 0xEF]);

            let mgr = LinuxBootManager::with_efivars_dir(dir.path());
            let entries = mgr.list_entries().unwrap();
            let names: Vec<&str> = entries.iter().map(|e| e.name.as_str()).collect();
            assert_eq!(names, vec!["ubuntu", "Windows Boot Manager"]);
            assert_eq!(entries[0].id, BootId::new(0x0002));
            assert!(entries.iter().all(|e| e.active));
        }

        #[test]
        fn set_and_clear_boot_next() {
            let dir = tempfile::tempdir().unwrap();
            let mgr = LinuxBootManager::with_efivars_dir(dir.path());

            assert_eq!(mgr.boot_next().unwrap(), None);

            mgr.set_boot_next(BootId::new(0x0002)).unwrap();
            let path = dir.path().join(format!("BootNext-{EFI_VARIABLE_GUID}"));
            let raw = fs::read(&path).unwrap();
            assert_eq!(raw, vec![0x07, 0x00, 0x00, 0x00, 0x02, 0x00]); // 属性头 + u16 LE
            assert_eq!(mgr.boot_next().unwrap(), Some(BootId::new(0x0002)));

            // 覆盖已有文件（真实环境中文件带 immutable 标志的路径）
            mgr.set_boot_next(BootId::new(0x0001)).unwrap();
            assert_eq!(mgr.boot_next().unwrap(), Some(BootId::new(0x0001)));

            mgr.clear_boot_next().unwrap();
            assert_eq!(mgr.boot_next().unwrap(), None);
            assert!(!path.exists());
        }

        #[test]
        fn clear_boot_next_without_existing_file_is_ok() {
            let dir = tempfile::tempdir().unwrap();
            let mgr = LinuxBootManager::with_efivars_dir(dir.path());
            mgr.clear_boot_next().unwrap();
        }

        #[test]
        fn write_var_truncates_shorter_overwrite() {
            let dir = tempfile::tempdir().unwrap();
            let mgr = LinuxBootManager::with_efivars_dir(dir.path());
            mgr.write_var("TestVar", 0x7, &[1, 2, 3, 4, 5, 6]).unwrap();
            mgr.write_var("TestVar", 0x7, &[9]).unwrap();
            let raw = fs::read(dir.path().join(format!("TestVar-{EFI_VARIABLE_GUID}"))).unwrap();
            assert_eq!(raw, vec![0x07, 0x00, 0x00, 0x00, 9]);
        }

        #[test]
        fn non_uefi_directory_reports_friendly_error() {
            let mgr = LinuxBootManager::with_efivars_dir("/nonexistent-efivars-xyz");
            assert!(matches!(
                mgr.list_entries(),
                Err(JanusError::NotUefiSystem(_))
            ));
        }

        #[test]
        fn corrupted_boot_next_returns_parse_error() {
            let dir = tempfile::tempdir().unwrap();
            write_var_file(dir.path(), "BootNext", 0x7, &[0x01]);
            let mgr = LinuxBootManager::with_efivars_dir(dir.path());
            assert!(matches!(mgr.boot_next(), Err(JanusError::Parse { .. })));
        }
    }
}

// 在非 Linux 平台（如交叉编译检查）时提供空实现，保证 workspace 可编译。
#[cfg(not(target_os = "linux"))]
mod imp {
    use janus_core::{BootEntry, BootId, BootManager, JanusError, Result};

    const UNSUPPORTED: &str = "janus 的 efivarfs 后端仅支持 Linux";

    /// 非 Linux 平台的占位实现。
    #[derive(Debug, Clone, Default)]
    pub struct LinuxBootManager;

    impl LinuxBootManager {
        pub fn for_running_system() -> Result<Self> {
            Err(JanusError::Other(UNSUPPORTED.into()))
        }
    }

    impl BootManager for LinuxBootManager {
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

pub use imp::LinuxBootManager;
