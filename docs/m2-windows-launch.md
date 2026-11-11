# Windows 启动面：无控制台、可写数据根与可移植资源根（2026-11-11）

M2 Windows Alpha 的第一次真机双击/快捷方式启动暴露了两类用户可见问题：
**双击弹出命令行窗口**，以及**某些启动方式下进程直接退出（看起来像"被 Windows 权限拦住"）**。
本文记录现象、可复现根因、本轮修复点与验证命令，并明确哪些项仍属 M3/M4。

对应代码：`apps/desktop-gpui/src/bin/chat.rs`、`apps/desktop-gpui/Cargo.toml`、
`tools/package-dev.ps1`。

## 1. 用户症状

| # | 症状 | 复现条件 |
|---|---|---|
| 1 | 双击 `chat.exe` 弹出一个黑色命令行窗口 | 任意双击启动（release 构建） |
| 2 | 进程一闪而过 / 窗口不出现，stderr 有 `[chat] 无法创建会话目录: 拒绝访问。(os error 5)`，exit code 1 | 桌面快捷方式"起始位置"为空（CWD = `C:\Windows\System32`），或 exe 放在 `Program Files`、只读共享盘等不可写目录 |
| 3 | 打包目录拷到别的机器后 sidecar 起不来（工具调用全部 aborted） | 把 `dist/dshr-dev` 整目录拷走，原构建机源码树 `C:\Projects\dshr` 不存在 |
| 4 | 带 `bash`/`pwsh` 的 sidecar 工具固定约 5 秒后失败 | 工具实际耗时长于 5s（`ToolDefinition.timeout_ms` 默认 120s） |
| 5 | SmartScreen / Defender 弹窗 | **未在本机复现**。当前二进制无数字签名，弹窗属未签名 dev 构建的**预期**现象，见 §5 |

症状 2 的完整可复现命令（修复前）：

```powershell
Start-Process target\release\chat.exe -WorkingDirectory C:\Windows\System32 `
  -RedirectStandardError .tmp\launch-err.txt -RedirectStandardOutput .tmp\launch-out.txt -Wait
Get-Content .tmp\launch-err.txt
# [chat] 无法创建会话目录: 拒绝访问。(os error 5)
```

## 2. 根因

| 症状 | 根因 | 本轮确认证据 |
|---|---|---|
| 1 | PE Subsystem = 3 (Console)：控制台子系统进程被双击时由系统分配控制台窗口 | `(Get-Item target\release\chat.exe).VersionInfo` 无法给出子系统；用 PE 头 `OptionalHeader.Subsystem` 读出 3（修复后为 2 = GUI） |
| 2 | 会话/状态目录是 **CWD 相对**的 `.tmp/sessions`（`DSHR_SESSIONS_DIR` 缺省时），CWD 不可写就 `create_dir_all` 失败并 `exit(1)` | 上节命令稳定复现 `os error 5`；把 CWD 换成可写目录即恢复 |
| 3 | `spawn_sidecar()` 用编译期 `env!("CARGO_MANIFEST_DIR")` 拼仓库根，打包后仍指向构建机源码树 | 旧代码 spawn_sidecar 的 root 常量；拷贝目录后 sidecar 脚本路径不存在 |
| 4 | ChatExecutor 对 sidecar 工具写死 `Duration::from_secs(5)`，与工具声明的 `timeout_ms` 无关 | 旧代码 `host.invoke_as(..., Duration::from_secs(5))`；`crates/tool-runtime/src/lib.rs` 默认 `timeout_ms = 120_000` |

**关于"被 Windows 权限拦住"**：本机能稳定复现的"权限拦住"就是症状 2 的 `os error 5`
（普通用户 token 下往 System32 建目录被拒），**不是** UAC 提权拦截或 Defender 拦截。
进程始终以当前用户普通权限启动，没有请求提升。

## 3. 修复点

### A. Release 隐藏控制台，CLI 输出仍可用

- 文件顶部：`#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]`
  —— release 是 GUI 子系统（双击不弹窗），debug 仍保留控制台方便 `cargo run`。
- `main()` 最先判断本次是否命令行用法（任意 `--` 参数，含 `--selftest*`、`--live*`、
  `--export-session`、`--retention-plan`、`--install-plugin`、`--seed-perf-session`、
  显式 `--console`），是则 `win_console::attach_parent_console()`：
  1. `FreeConsole()` 清掉可能的残留；
  2. `AttachConsole(ATTACH_PARENT_PROCESS)` 挂到 PowerShell/cmd 的控制台；
  3. 仅当 `STD_OUTPUT/STD_ERROR/STD_INPUT` 句柄当前无效时，打开 `CONOUT$`/`CONIN$` 并
     `SetStdHandle` 装回（已被 `-RedirectStandardOutput` 重定向的继承句柄保持不动）；
  4. `SetConsoleOutputCP(65001)`，修中文乱码（中文系统默认 936，会把 UTF-8 字节显示成乱码）。
- 双击启动时父进程是 explorer，`AttachConsole` 失败 → 不显示控制台、继续开窗。

### B. 数据根与 CWD 解耦

`prepare_data_env()` 在 `install_crash_hook()` **之前**解析数据根（hook 通过
`DSHR_CRASH_LOG` 写日志），优先级：

1. `DSHR_HOME`（非空，显式覆盖；创建失败也尊重用户选择）；
2. exe 所在目录 —— 仅当存在 `sidecar/`（打包布局）**且目录可写**；
3. `%LOCALAPPDATA%\dshr`（默认；不存在则创建）；
4. 兜底：可写的 exe 目录 → 可写的当前目录 → 系统临时目录，并在 stderr 说明。

未被用户显式设置时，注入默认环境变量（显式值永远优先，自检/测试不受影响）：

| 环境变量 | 默认值 |
|---|---|
| `DSHR_SESSIONS_DIR` | `<root>\sessions` |
| `DSHR_PROFILE_DIR` | `<root>\profile` |
| `DSHR_SPILL_DIR` | `<root>\spill` |
| `DSHR_CRASH_LOG` | `<root>\crash.log` |

开发机从 `target/release` 运行时 exe 旁没有 `sidecar/`，数据落 `%LOCALAPPDATA%\dshr`；
打包目录（exe 旁有 `sidecar/` 且可写）则数据就地在包内 —— 都不再依赖 CWD。

### C. sidecar/资源根改为 exe 相对

`app_root()`：`DSHR_APP_ROOT` → exe 旁含 `sidecar/` 的目录 → 编译期仓库根（仅开发机兜底）。
`spawn_sidecar()`、`--selftest-dsh`、`--selftest-dsh-write`、`--install-plugin` 统一使用它。
`dist/dshr-dev` 整目录拷走后不再需要 `C:\Projects\dshr`。

`runtime/node` 的解析本来就在 `crates/sidecar-guard` 里按"exe → 祖先目录"查找，无需改动。

### D. sidecar 工具超时用工具声明

`ChatExecutor` 的 `ToolOrigin::Sidecar` 调用改为：

- 超时取 `ToolDefinition.timeout_ms`（对齐 DSH 的 per-tool `timeoutMs`）；
- `DSHR_TOOL_TIMEOUT_MS` 可整体覆盖（诊断/慢机器）；
- 统一 clamp 到 **1s..=600s**（0/超大值不会让界面像卡死）。
- native 路径不变。

### E. 打包与说明

`tools/package-dev.ps1`：包内新增 `RUNNING.txt`（双击启动、数据默认目录、
未签名 dev 构建被 SmartScreen/Defender 拦截时的说明与 `Unblock-File` 建议、CLI 自检入口），
`MANIFEST.txt` 注明 release `chat.exe` 是 GUI 子系统、无控制台；debug 打包会额外提示会弹窗。

## 4. 复现与验证

```powershell
# 1) 构建
tools\build-env.cmd cargo build --release -p dshr-desktop-gpui --bin chat

# 2) 单测 / 静态检查（新增：数据根选择、CLI 参数识别、超时 clamp）
tools\build-env.cmd cargo test -p dshr-desktop-gpui
tools\build-env.cmd cargo clippy -p dshr-desktop-gpui --all-targets -- -D warnings
tools\build-env.cmd cargo fmt -p dshr-desktop-gpui

# 3) CLI 仍能打印（release GUI 子系统下 AttachConsole 生效）
target\release\chat.exe --selftest        # 期望最后一行 [selftest] PASS

# 4) 不可写 CWD 不再退出（清理 DSHR_* 后从 System32 启动）
Start-Process target\release\chat.exe -WorkingDirectory C:\Windows\System32 `
  -RedirectStandardError .tmp\launch2-err.txt -RedirectStandardOutput .tmp\launch2-out.txt
# 期望：进程存活并出现 Zed::Window 顶层窗口，stderr 不再有 os error 5；
#       会话目录被创建在 %LOCALAPPDATA%\dshr\sessions

# 5) 双击等价：正常 CWD 启动，枚举该 PID 顶层窗口，断言无 ConsoleWindowClass
#    （验证脚本见本轮任务 task-1 验收记录；窗口类名应为 Zed::Window）
```

PE 子系统可用 PowerShell 读 PE 头验证（修复后应为 2 = Windows GUI）：

```powershell
$bytes = [System.IO.File]::ReadAllBytes('target\release\chat.exe')
$pe = [BitConverter]::ToInt32($bytes, 0x3C)
$subsystem = [BitConverter]::ToUInt16($bytes, $pe + 0x5C)   # OptionalHeader.Subsystem
$subsystem   # 2 = GUI, 3 = Console
```

本轮实测（2026-11-11，Windows 11 x64）：

- PE `Subsystem = 2`（修复前为 3）；`--selftest` 直接运行时输出以 `[selftest] PASS` 结束，exit 0。
- 正常 CWD 启动 release exe，枚举该 PID 顶层窗口：`Zed::Window|visible=True`、
  `Zed::PlatformWindow|visible=False`、`MSCTFIME UI`、`IME`；**没有 `ConsoleWindowClass`**。
- `-WorkingDirectory C:\Windows\System32`（无 `DSHR_*` 环境变量）启动：进程存活并开出
  `Zed::Window`，数据根按优先级落到可写目录，不再因 `os error 5` 退出。
- 用中完整性级别的 exe 副本从 System32 启动：`%LOCALAPPDATA%\dshr\sessions`、
  `...\profile`、`...\workspace` 正常创建；在隐藏控制台窗口中运行 `--selftest`，
  控制台屏幕缓冲区里能看到 `[selftest] PASS`（证明 `AttachConsole` 路径生效）。

## 5. 仍属 M3/M4 的项

- **代码签名**（`PLAN.md` §8）：消除 SmartScreen/Defender 警告需要证书与时间戳服务；
  本机未复现 SmartScreen 弹窗，如实记录为"未签名 dev 构建的预期现象"。
- **安装器/卸载器**（NSIS/MSIX）：装到 `Program Files` 时 exe 目录不可写 —— 本轮的
  数据根回退正好覆盖该布局；还需开始菜单快捷方式、默认"起始位置"、升级/卸载。
- **受限 token / AppContainer**：把工具与 sidecar 放进更小权限的沙箱（当前是同一用户 token +
  Job Object 资源限制）。
- **per-tool 超时透传**：本轮的读取端（`definition.timeout_ms` + clamp + env 覆盖）已就位，
  但 sidecar 注册协议暂未把插件声明的 `timeoutMs` 带进 Rust `ToolDefinition`（现在用默认 120s），
  属 M3 协议扩展。

## 6. 边界与注意事项

- debug 构建仍是 Console 子系统：`cargo run` 不会丢日志；release 用 `--console` 也能强制挂回父控制台。
- GUI 双击不调用 `AttachConsole`；只有命令行用法才重建控制台。
- 数据根取决于布局：同一个 exe 在 `dist/dshr-dev`（包内有 `sidecar/`）与单独拷到 `%LOCALAPPDATA%`
  时数据根不同（包内 vs `%LOCALAPPDATA%\dshr`）。需要固定位置时显式设 `DSHR_HOME`。
- 工具工作区的默认值仍是可写的 CWD（`DSHR_WORKSPACE` 可覆盖）；CWD 不可写（如 System32）时
  自动改用 `<数据根>\workspace`，否则沙箱初始化会以 `os error 5` panic，表现为"启动即退出"。
- **完整性级别与验证环境**：exe 文件可能带有低完整性标签（`icacls chat.exe` 出现
  `Mandatory Label\Low Mandatory Level`，本仓库工作区当前就有 DSH 沙箱会话残留的这种标签）。
  **文件标签不会改变进程令牌**：从资源管理器/普通终端启动时仍以当前用户的 Medium 完整性运行
  （本轮实测带该标签的 release exe 能写 `%LOCALAPPDATA%`、`--selftest` 正常）。只有从
  **低完整性父进程**（如 DSH 受限沙箱里的 shell）启动时，子进程才继承低完整性：写不了
  `%LOCALAPPDATA%`（数据根回退到可写 exe 目录）、`AttachConsole` 可能返回
  `ERROR_ACCESS_DENIED`。遇到这种验证失败时，换普通终端或用 `%TEMP%` 里的副本复验，
  不要据此判断产品在普通用户环境下不可用。
