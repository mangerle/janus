<div align="center">
  <picture>
    <source media="(prefers-color-scheme: dark)" srcset="./logo/janus-lockup-dark.svg" />
    <img src="./logo/janus-lockup.svg" alt="Janus" width="480" />
  </picture>

  <p>
    <a href="./README.md"><strong>English</strong></a> ·
    <a href="./README_zh-CN.md"><strong>简体中文</strong></a>
  </p>

  <p>
    <a href="./LICENSE"><img alt="License" src="https://img.shields.io/badge/license-MIT-blue" /></a>
    <img alt="Rust" src="https://img.shields.io/badge/rust-1.96%2B-orange" />
    <img alt="Platform" src="https://img.shields.io/badge/platform-Windows%20%7C%20Linux-informational" />
    <a href="https://github.com/mangerle/janus/actions/workflows/ci.yml"><img alt="CI" src="https://github.com/mangerle/janus/actions/workflows/ci.yml/badge.svg" /></a>
    <img alt="Status" src="https://img.shields.io/badge/status-alpha-red" />
  </p>

  <p>主页：<a href="https://mangerle.github.io/janus/">mangerle.github.io/janus</a></p>

  <p>Windows ↔ Linux 双系统一键切换：写 UEFI BootNext 并重启，常驻托盘，纯原生 API，不依赖 <code>efibootmgr</code>/<code>bcdedit</code>。</p>
</div>

---

**Janus**（双面神，掌管门与 transitions）写入 UEFI 固件变量 **BootNext** 后立即重启，下次开机直接进入你选定的系统——不必在按下电源键后狂按 ESC/F8/F12。它是一个小型 Rust workspace：CLI 用于脚本化操作，托盘用于日常一键切换。

所有固件交互都走**平台原生 API**：

- **Linux** —— 直接读写内核 `efivarfs`（`/sys/firmware/efi/efivars/`），包括较新内核要求的 `FS_IMMUTABLE_FL` 清除流程。托盘基于 tray-icon 的 **ksni** 后端（纯 D-Bus `StatusNotifierItem`），**没有任何 GTK/libappindicator 系统库依赖**。
- **Windows** —— 通过 `AdjustTokenPrivileges` 启用 `SeSystemEnvironmentPrivilege` 后调用 `GetFirmwareEnvironmentVariableW` / `SetFirmwareEnvironmentVariableW`，UAC 提权自动处理（`runas` + `TokenElevation` 检测）。

## 为什么做 janus

- **一次点击 = 切换 + 重启。** 托盘菜单把"写 BootNext"和"重启"合并成一个动作；polkit 密码框（Linux）或确认对话框（Windows）就是你的反悔按钮。
- **原生实现，不借助外部命令。** 不调用 `efibootmgr`、不调用 `bcdedit`——每个字节都在 `janus-core` 里按 `EFI_LOAD_OPTION` 二进制结构解析（UTF-16LE 描述符）。
- **签名自更新。** `janus update` 先校验 Ed25519 签名清单（[shipup](https://github.com/mangerle/shipup)）再下载，替换二进制前还做 SHA-256 + 包签名双重校验。升级后若连续启动崩溃，会自动回滚。
- **全程 `--dry-run`。** 内置 Mock 后端，托盘和 CLI 都可以零固件风险地演示。
- **可定制的切换列表。** 图形设置窗口可选择显示哪些启动项、自定义显示名称；未配置项默认全部显示，CLI 与托盘共用同一份设置。
- **单文件。** 静态链接（含图形设置窗口），LTO 后 Release 二进制约 17 MB；Linux 端无硬链接系统库，设置窗口按需使用桌面 OpenGL。

## 安装

**从 GitHub Releases**（推荐）：

从[最新发布页](https://github.com/mangerle/janus/releases/latest)下载 `janus_<版本>_linux_amd64.tar.gz` 或 `janus_<版本>_windows_x64.zip`，解压后把 `janus` 放进 `PATH`。

**从源码构建：**

```sh
cargo install --git https://github.com/mangerle/janus
# 或克隆仓库后 cargo build --release
```

> Linux 要求以 UEFI 模式启动（BIOS/Legacy 会得到友好提示）；Windows 构建需要 Rust 1.96+。

## 使用

```
janus                             无参数：常驻系统托盘（等价于 janus tray）
janus list                        列出启动项（按 BootOrder 顺序）
janus next <目标>                  设置 BootNext（不重启）
janus clear                       清除 BootNext
janus reboot [--into <目标>]       设置 BootNext 并立即重启
janus settings                    打开图形设置：显示项、自定义名称与开机自启
janus tray                        显式启动系统托盘（常驻）
janus autostart [on|off|status]   管理开机自启（默认查看当前状态）
janus update [--check] [--yes]    自更新（签名清单校验）
janus --dry-run ...               Mock 后端，不触碰真实固件
janus --all ...                   忽略显示设置，展示全部启动项与固件原名
```

`<目标>` 可以是 `BootXXXX` 编号（十六进制：`1`、`0x0002`、`Boot0002`），也可以是名称子串（不区分大小写：`ubuntu`、`windows`）。

```console
$ janus list
编号         名称                                 状态
Boot0000   ubuntu                             活跃
Boot2001   EFI USB Device                     活跃
Boot0001   Windows Boot Manager               活跃

$ janus reboot --into "Windows Boot Manager"
已设置下次启动：Boot0001（Windows Boot Manager）。
```

### 托盘

托盘菜单在每次动作后重建，"当前下次启动"信息行始终如实反映固件状态：

```
当前下次启动：未设置
────────────────────────────────────
重启并进入：            → Boot0001  Windows Boot Manager
                        Boot0002  ubuntu
仅下次启动              → …
清除 BootNext
立即重启
────────────────────────────────────
设置
退出
```

**Linux** 上托盘以普通用户运行在你的桌面会话中；每次切换都经 `pkexec`（polkit 密码框*本身就是*确认步骤）调起隐藏的 `janus _apply` 特权 helper。**Windows** 上托盘启动时经 UAC 重执行一次（整个会话只弹一次密码），重启前用 `MessageBoxW` 弹"是/否"确认。想彻底免弹窗，可在任务计划程序中勾选*使用最高权限运行*让托盘静默提权启动。

### 显示设置

托盘菜单的“设置”或 `janus settings` 会打开图形设置窗口：勾选要显示在托盘与 CLI
列表中的启动项、填写自定义显示名称，以及开关「开机自动启动托盘」（Linux 写入
XDG autostart 目录，Windows 写入当前用户 `HKCU\...\Run` 启动项）。设置文件位置：

- Linux：`$XDG_CONFIG_HOME/janus/config.toml`（未设置时 `~/.config/janus/config.toml`）
- Windows：`%APPDATA%\janus\config.toml`

未配置的启动项默认显示并使用固件原名，新出现的启动项自动可见。隐藏只影响列表
展示，按编号（如 `janus next Boot0003`）仍可直接指定；`--all` 可临时忽略全部设置。
保存后托盘会立即重建菜单。

窗口右上角显示当前版本并提供“检查更新”：检查、下载、验签与安装都在窗口内完成，
成功后重启 janus（含托盘）生效；安装目录不可写时会提示改用管理员/root 运行。

```toml
[entries.Boot0001]
name = "工作 Windows"

[entries.Boot0003]
hidden = true
```

**Linux** 上托盘以普通用户运行在你的桌面会话中；每次切换都经 `pkexec`（polkit 密码框*本身就是*确认步骤）调起隐藏的 `janus _apply` 特权 helper。**Windows** 上托盘启动时经 UAC 重执行一次（整个会话只弹一次密码），重启前用 `MessageBoxW` 弹"是/否"确认。想彻底免弹窗，可在任务计划程序中勾选*使用最高权限运行*让托盘静默提权启动。

### 自更新

```console
$ janus update
当前版本 v0.1.0，正在检查更新…
发现新版本：v0.2.0（发布于 2026-11-21）
更新说明：
…
  下载中   100.0%
  正在校验 SHA-256…
  正在校验 Ed25519 签名…
更新完成：v0.2.0 已安装，下次运行 janus 即生效。
```

每个 Release 随附的清单（`latest.json`）都用项目专属 Ed25519 密钥签名，公钥编译在 `janus-updater` 内。伪造或被篡改的清单在任何下载发生之前就会被拒绝。

## 工作原理

写 BootNext 需要特权固件访问，这里集中了经典的三个"坑"——以及 janus 的处理方式：

1. **权限。** Linux：写 `efivarfs` 需要 root；CLI 通过 `pkexec`（回退 `sudo`）重执行自身，托盘按动作委托给 root helper。Windows：进程用 `AdjustTokenPrivileges` 启用 `SeSystemEnvironmentPrivilege`，并经 UAC 提权一次。
2. **UTF-16LE 描述符。** `BootXXXX` 的名称存放在二进制 `EFI_LOAD_OPTION` 结构里；`janus-core` 按 `u32 attributes | u16 path_len | UTF-16LE(\0 结尾) | 设备路径 | 附加数据` 解析，非法序列做 lossy 容错。
3. **efivarfs 的 immutable 保护。** 较新内核给变量文件加了 `FS_IMMUTABLE_FL`，直接写会 `EPERM`。Linux 后端在每次写入/删除前先用 `FS_IOC_GETFLAGS`/`FS_IOC_SETFLAGS` ioctl 清除该标志。

## 架构

```
crates/janus (bin)          CLI 编排 + 权限策略 + 重启
        │ Box<dyn BootManager>
crates/janus-core           BootEntry / BootManager trait / EFI_LOAD_OPTION
        │                   解析（UTF-16LE）/ Mock 后端
        ├── crates/janus-backend-linux    efivarfs 读写 + FS_IMMUTABLE_FL 处理
        ├── crates/janus-backend-windows  AdjustTokenPrivileges + 固件环境变量 API
        ├── crates/janus-privilege        pkexec/sudo (Linux) · UAC runas (Windows)
        ├── crates/janus-tray             tray-icon: ksni (Linux) · tao (Windows)
        └── crates/janus-updater          shipup 封装：验签/下载/安装/回滚
```

发布流水线（`.github/workflows/release.yml`）在 `v*` 标签上构建双平台产物，用 [git-cliff](https://git-cliff.org) 生成分类更新日志，再经 `shipup-cli` 签名清单后发布 GitHub Release。

## 开发

```sh
cargo test                                        # 20 个单元测试
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all -- --check

# 在 Linux 上验证 Windows 专属代码（无需 Windows 机器）
rustup target add x86_64-pc-windows-gnu
cargo check --workspace --target x86_64-pc-windows-gnu

# 重新生成 lockup logo（纯路径输出；需要 fontTools）
python3 logo/gen-lockup.py
```

**发布流程：** 更新根 `Cargo.toml` 的 `version`，合并后 `git tag v0.x.y && git push --tag`。发布流水线会校验标签与 workspace 版本一致，且缺少 `SHIPUP_PRIVATE_KEY` Secret 时拒绝发布。签名私钥只存在于该 Secret（以及维护者本地已 gitignore 的 `keys/` 目录）。

## 已知限制

- 仅支持 UEFI 启动的机器（BIOS/Legacy 会被识别并给出提示）。
- Windows 后端已做交叉编译检查并在 `windows-latest` CI 实测，欢迎真机反馈。
- Linux 托盘需要桌面环境支持 `StatusNotifierItem`（Ubuntu/KDE 默认满足），且会话中需有 polkit agent。
- 个别发行版以 `0400` 权限挂载 efivarfs，此时 `janus list` 会自动提权。

## License

[MIT](./LICENSE)
