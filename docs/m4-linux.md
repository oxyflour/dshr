# M4 Linux 核心 crate 编译（G1 部分证据）

G1 要求 Windows/macOS/Linux 管线。本机没有 Linux 桌面，但用 WSL Ubuntu 22.04 对
**非 GPUI 的核心 crate** 做了一次真实 Linux 编译，发现并修掉一个 Linux-only 编译错误。

## 1. 环境

| 项 | 值 |
|---|---|
| 发行版 | Ubuntu 22.04（WSL2，内核 6.18） |
| Rust | 1.99.1（rustup，与 `rust-toolchain.toml` 一致） |
| Node | v22.14.0（仅备用；本次只做 `cargo check`） |
| Cargo 源 | rsproxy 镜像 |
| 工作区 | `/tmp/dshr-core`：复制 `Cargo.toml`/`Cargo.lock`/`crates`，移除 `ui-gpui` 与 `apps` 成员（避免拉 GPUI） |

## 2. 命令

```text
cargo check -p dshr-tool-runtime -p dshr-plugin-api -p dshr-session-store \
  -p dshr-profile -p dshr-compat-dsh-protocol -p dshr-compat-dsh-host
```

（`dshr-compat-dsh-host` 依赖 `dshr-sidecar-guard`，因此后者也被真实编译。）

## 3. 发现的 Linux-only 错误与修复

`crates/sidecar-guard/src/process.rs` 的非 Windows `spawn` 接收 `command: Command` 后
调用 `command.spawn()`；Linux 下 `Command::spawn` 需要 `&mut self`，原签名没有 `mut`，
Windows 因走 Job Object 分支不触发。修复：

```rust
pub(super) fn spawn(
    mut command: Command,
    _limits: &GuardLimits,
) -> Result<GuardedChild, GuardError> { ... }
```

修复后上述 `cargo check` **通过**（仅 `cpu_limit_ticks` 在非 Windows 未使用的 warning）。

## 4. 边界

- 只验证**核心 crate 编译**，没有跑完整 Linux 测试（部分测试依赖 `.exe`/Windows 路径）。
- **没有**编译 `dshr-ui-gpui` / `dshr-desktop-gpui`；GPUI 需要 X11/Wayland/字体/Vulkan 等
  系统库，macOS 也没有 runner。G1 仍不标完成。
- `cross-platform.yml` 的 `core-rust` job 已包含这些 crate（另加 `dshr-sidecar-guard`），
  在 Ubuntu runner 上应能复现本结果；Actions 尚未实跑。
