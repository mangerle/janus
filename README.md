<div align="center">
  <picture>
    <source media="(prefers-color-scheme: dark)" srcset="./logo/janus-lockup-dark.svg" />
    <img src="./logo/janus-lockup.svg" alt="Janus Boot" width="480" />
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

  <p>One-click UEFI boot switching between Windows and Linux — tray-resident, purely native, no <code>efibootmgr</code>/<code>bcdedit</code>.</p>
</div>

---

**Janus Boot** (the two-faced god of doorways and transitions) writes the UEFI firmware variable **BootNext** and reboots, so the next boot goes straight into the OS you picked — no frantic ESC/F8/F12 mashing at power-on. It is a small Rust workspace: a CLI for scripting and a system-tray app for daily use.

Everything talks to the firmware through **native platform APIs**:

- **Linux** — reads and writes the kernel's `efivarfs` (`/sys/firmware/efi/efivars/`) directly, including the `FS_IMMUTABLE_FL` dance newer kernels require. The tray runs on tray-icon's **ksni** backend (pure D-Bus `StatusNotifierItem`), so there are **no GTK/libappindicator system dependencies** at all.
- **Windows** — `GetFirmwareEnvironmentVariableW` / `SetFirmwareEnvironmentVariableW` after enabling `SeSystemEnvironmentPrivilege` via `AdjustTokenPrivileges`, with UAC elevation handled automatically (`runas` + `TokenElevation` checks).

## Why Janus Boot

- **One click = switch + reboot.** The tray menu writes BootNext and reboots in a single action; the polkit password box (Linux) or MessageBox confirmation (Windows) is your undo button.
- **Native, not shelled out.** No `efibootmgr`, no `bcdedit` — every byte is parsed from the `EFI_LOAD_OPTION` binary structure (UTF-16LE descriptors) in `janus-core`.
- **Signed self-updates.** `janus-boot update` verifies an Ed25519-signed manifest ([shipup](https://github.com/mangerle/shipup)) before downloading, then double-checks SHA-256 + package signature before replacing the binary. A broken update that crashes at startup is rolled back automatically.
- **`--dry-run` everything.** A built-in mock backend lets you demo the tray and CLI without touching real firmware.
- **Customizable switch list.** A graphical settings window lets you choose which boot entries appear and rename them; unconfigured entries stay visible, and the CLI and tray share the same settings.
- **Single binary.** Statically linked (graphical settings included), ~17 MB release binary with LTO; no hard-linked system libraries on Linux — the settings window uses the desktop's OpenGL at runtime.

## Install

**From GitHub Releases** (recommended):

Grab `janus-boot_<version>_linux_amd64.tar.gz` or `janus-boot_<version>_windows_x64.zip` from the [latest release](https://github.com/mangerle/janus/releases/latest), unpack, and put `janus-boot` on your `PATH`.

**From source:**

```sh
cargo install --git https://github.com/mangerle/janus
# or clone + cargo build --release
```

> Linux requires a UEFI-booted system (BIOS/Legacy gets a friendly error). Windows builds need Rust 1.96+.
>
> On Debian/Ubuntu, the package and executable are named `janus-boot` to avoid
> colliding with the WebRTC `janus` package. Existing display settings are
> migrated automatically on first launch.

## Usage

```
janus-boot                             No args: resident tray app (same as janus-boot tray)
janus-boot list                        List boot entries (BootOrder order)
janus-boot next <target>               Set BootNext (no reboot)
janus-boot clear                       Clear BootNext
janus-boot reboot [--into <target>]    Set BootNext and reboot now
janus-boot settings                    Graphical settings: visibility, display names, and autostart
janus-boot tray                        Explicitly start the resident tray app
janus-boot autostart [on|off|status]   Manage launch at login (defaults to status)
janus-boot update [--check] [--yes]    Self-update (signed manifest)
janus-boot --dry-run ...               Mock backend, never touches firmware
janus-boot --all ...                   Ignore display settings; show every entry with firmware names
```

`<target>` is a `BootXXXX` id (hex: `1`, `0x0002`, `Boot0002`) or a case-insensitive name substring (`ubuntu`, `windows`).

```console
$ janus-boot list
编号         名称                                 状态
Boot0000   ubuntu                             活跃
Boot2001   EFI USB Device                     活跃
Boot0001   Windows Boot Manager               活跃

$ janus-boot reboot --into "Windows Boot Manager"
已设置下次启动：Boot0001（Windows Boot Manager）。
```

### Tray

The tray menu is rebuilt after every action so the "current next boot" line stays truthful:

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

On **Linux** the tray runs unprivileged in your desktop session; every switch goes through `pkexec` (the polkit password box *is* the confirmation), which calls a hidden `janus-boot _apply` helper as root. On **Windows** the tray re-executes itself elevated once at startup (single UAC prompt) and shows a `MessageBoxW` yes/no dialog before rebooting. To silence the UAC prompt entirely, create a Task Scheduler entry with *Run with highest privileges*.

### Display settings

"设置" in the tray menu (or `janus-boot settings`) opens a graphical window where you
can choose which boot entries appear in the tray and CLI lists, give them
custom display names, and toggle launch at login (writes an XDG autostart
entry on Linux or `HKCU\...\Run` on Windows). The settings file lives at:

- Linux: `$XDG_CONFIG_HOME/janus-boot/config.toml` (defaults to `~/.config/janus-boot/config.toml`)
- Windows: `%APPDATA%\janus-boot\config.toml`

Unconfigured entries stay visible with their firmware names, so newly added
operating systems show up automatically. Hiding only affects list display;
entries remain addressable by ID (e.g. `janus-boot next Boot0003`), and `--all`
temporarily ignores all display settings. The tray menu is rebuilt as soon as
settings are saved.

The current version is shown in the window header together with a
"检查更新" (check for updates) button; checking, downloading, signature
verification and installation all happen in the window. Restart Janus Boot
(including the tray) afterwards; if the install directory is not writable,
the window tells you to rerun with administrator/root privileges.

```toml
[entries.Boot0001]
name = "Work Windows"

[entries.Boot0003]
hidden = true
```

### Self-update

```console
$ janus-boot update
当前版本 v0.1.0，正在检查更新…
发现新版本：v0.2.0（发布于 2026-11-21）
更新说明：
…
  下载中   100.0%
  正在校验 SHA-256…
  正在校验 Ed25519 签名…
更新完成：v0.2.0 已安装，下次运行 janus-boot 即生效。
```

The manifest (`latest.json`) published with each release is signed with the project's Ed25519 key; its public half is compiled into `janus-updater`. Unknown or tampered manifests are rejected before any download.

## How it works

Writing BootNext needs privileged firmware access, which is where the classic three pitfalls live — and how janus-boot handles them:

1. **Privilege.** Linux: writes to `efivarfs` need root; the CLI re-executes itself via `pkexec` (fallback `sudo`), the tray delegates to a root helper per action. Windows: the process enables `SeSystemEnvironmentPrivilege` with `AdjustTokenPrivileges` and elevates through UAC once.
2. **UTF-16LE descriptors.** `BootXXXX` descriptions live inside the binary `EFI_LOAD_OPTION` structure; `janus-core` parses `u32 attributes | u16 path_len | UTF-16LE(\0-terminated) | device path | optional data`, with lossy conversion for invalid sequences.
3. **Immutable efivarfs files.** Recent kernels mark variable files `FS_IMMUTABLE_FL`, so a plain write fails with `EPERM`. The Linux backend clears the flag via the `FS_IOC_GETFLAGS`/`FS_IOC_SETFLAGS` ioctls before every write or delete.

## Architecture

```
crates/janus-boot (bin)          CLI orchestration + privilege policy + reboot
        │ Box<dyn BootManager>
crates/janus-core           BootEntry / BootManager trait / EFI_LOAD_OPTION
        │                   parsing (UTF-16LE) / mock backend
        ├── crates/janus-backend-linux    efivarfs + FS_IMMUTABLE_FL handling
        ├── crates/janus-backend-windows  AdjustTokenPrivileges + firmware env API
        ├── crates/janus-privilege        pkexec/sudo (Linux) · UAC runas (Windows)
        ├── crates/janus-tray             tray-icon: ksni (Linux) · tao (Windows)
        └── crates/janus-updater          shipup wrapper: verify/download/install/rollback
```

The release pipeline (`.github/workflows/release.yml`) builds both platforms on `v*` tags, generates a categorized changelog with [git-cliff](https://git-cliff.org), and signs the update manifest with `shipup-cli` before publishing the GitHub Release.

## Development

```sh
cargo test                                        # 20 unit tests
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all -- --check

# Validate the Windows-only code from a Linux box (no Windows machine needed)
rustup target add x86_64-pc-windows-gnu
cargo check --workspace --target x86_64-pc-windows-gnu

# Regenerate the logo lockups (path-based; fontTools required)
python3 logo/gen-lockup.py
```

**Releasing:** bump `version` in the root `Cargo.toml`, merge, then `git tag v0.x.y && git push --tag`. The release workflow validates the tag against the workspace version and refuses to publish if the `SHIPUP_PRIVATE_KEY` secret is missing. The signing private key lives only in that secret (and the maintainer's local, git-ignored `keys/` directory).

## Known limitations

- UEFI-booted machines only (BIOS/Legacy is detected and reported).
- The Windows backend is cross-compile-checked and CI-tested on `windows-latest`, but real-hardware feedback is still welcome.
- The Linux tray needs a `StatusNotifierItem`-capable shell (Ubuntu/KDE ship one by default) and a polkit agent in the session.
- A few distros mount efivarfs `0400`; in that case `janus-boot list` auto-elevates.

## License

[MIT](./LICENSE)
