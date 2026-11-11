# Node 运行时分发与 Sidecar 隔离（2026-11-11）

补 `PLAN.md` §5.5（Node 运行时与依赖分发）与 §7（Sidecar OS 级隔离）的 M2 交付：

- 随包分发固定版本的 Node（不依赖用户环境、离线可运行）；
- Sidecar 放进 Windows Job Object：进程数/内存/可选 CPU 时间上限，关掉即杀干净；
- 子进程环境变量清洗（不继承宿主凭据）；
- 独立 profile 目录；
- 插件安装支持 npm `pack` 产物 `.tgz`（离线解包，不联网）。

对应代码：`crates/sidecar-guard`（新 crate）、`crates/profile/src/{archive,install}.rs`、
`tools/vendor-node.mjs`、`tools/package-dev.ps1`。

## 1. Node 捆绑策略（§5.5）

| 决策项 | 当前做法 | 状态 |
|---|---|---|
| Node 版本 | 捆绑固定版本 **v24.21.0**，与 DSH 桌面版 `0.2.2-rc.1` 自带的运行时同源 | ✅ |
| 版本来源 | **不联网下载**：从本机 DSH 安装 `resources/runtime/primary-runtime/dependencies/node/bin/node.exe` 复制 | ✅ |
| 产物位置 | `runtime/node/node.exe` + `runtime/node/RUNTIME.json`（gitignored，不提交） | ✅ |
| 解析顺序 | `DSHR_NODE` → `runtime/node`（随包）→ `PATH` | ✅ Rust 侧 `resolve_node()` |
| 包管理 | 用 Node 自带能力与 profile 内目录；**pnpm 不捆绑**（见 §5 风险） | ⚠️ 未完成 |
| 离线 | 插件以 `.tgz`/目录离线安装；运行时不需要 registry | ✅ 安装侧 |
| 首次启动无 Node | 解析失败返回 `None`，chat 应进入 Native-only 模式并提示（`PLAN` §5.5） | 接线待 Lead |

### vendor 流程（离线，可复现）

```powershell
node tools/vendor-node.mjs --check   # 只检查 runtime/node 是否就绪
node tools/vendor-node.mjs           # 自动探测本机 DSH 安装并复制
node tools/vendor-node.mjs --force   # 升级/换版本时覆盖
```

探测顺序（`tools/vendor-node.mjs`）：

1. `--source <path>` / `DSHR_NODE_SOURCE` / `DSHR_NODE`；
2. `DSHR_ASAR` 同级的 `resources/runtime/primary-runtime/dependencies/node/bin/node.exe`；
3. `%LOCALAPPDATA%\Programs\DeepSeek Harness\resources\runtime\primary-runtime\dependencies\node\bin\node.exe`；
4. `%ProgramFiles%\DeepSeek Harness\resources\...`；
5. `PATH` 上的 `node`（兜底，仅开发机）。

产物 `runtime/node/RUNTIME.json` 记录 `node` 版本、来源路径、平台/架构与 vendored 时间，
用于安装确认与排障：

```json
{ "schemaVersion": 1, "node": "v24.21.0", "source": "…\\primary-runtime\\dependencies\\node\\bin\\node.exe",
  "platform": "win32", "arch": "x64", "vendoredAt": "2026-11-11T12:38:47.948Z" }
```

`.gitignore` 已忽略 `/runtime/` 与 `/dist/`：90 MB 的二进制不进仓库，换机器重新 vendor。

### Rust 侧解析 API（`crates/sidecar-guard/src/node.rs`）

```rust
pub fn resolve_node() -> Option<NodeRuntime>;
pub fn resolve_node_with(dshr_node: Option<&str>, runtime_dir: Option<&str>,
    current_exe: Option<&Path>, cwd: Option<&Path>, path_env: Option<&OsStr>) -> Option<NodeRuntime>;

pub struct NodeRuntime { pub program: PathBuf, pub source: NodeSource }
pub enum NodeSource { Env, Vendored, Path }   // describe() 用于日志
```

查找逻辑：`DSHR_NODE` 非空即用；否则 `DSHR_RUNTIME_DIR` → 宿主 exe 的祖先目录 →
当前目录，接受 `runtime/node/node.exe`、`node/node.exe`、`node/bin/node.exe` 三种布局；
最后扫 `PATH`（跳过空条目）。`resolve_node_with` 可注入环境，测试不改进程全局变量。

## 2. Sidecar OS 级隔离（§7）

### Windows Job Object

`crates/sidecar-guard/src/process.rs`：

| 限制 | Win32 实现 | 默认值（`GuardLimits::sidecar_default()`） |
|---|---|---|
| 关 job 即杀全部进程 | `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE` | 始终开启 |
| 活跃进程数上限 | `JOB_OBJECT_LIMIT_ACTIVE_PROCESS` + `ActiveProcessLimit` | 16 |
| 内存上限 | `JOB_OBJECT_LIMIT_JOB_MEMORY` + `JobMemoryLimit`（job 总提交内存） | 1 GiB |
| CPU 时间上限（可选） | `JOB_OBJECT_LIMIT_JOB_TIME` + `PerJobUserTimeLimit`（100ns） | 不限 |
| 失败处理 | `AssignProcessToJobObject` 失败 → 杀子进程并返回 `GuardError::Windows{call,code,message}` | — |

```rust
pub struct GuardLimits {
    pub max_processes: Option<u32>,
    pub max_memory_bytes: Option<u64>,
    pub max_cpu_time: Option<Duration>,
}
pub fn spawn_guarded(command: Command, cwd: impl AsRef<Path>,
    env: &[(String, String)], limits: &GuardLimits) -> Result<GuardedChild, GuardError>;
pub fn spawn_guarded_with(command: Command, cwd: impl AsRef<Path>,
    env: &[(String, String)], limits: &GuardLimits, allowlist: &[&str]) -> Result<GuardedChild, GuardError>;
pub struct GuardedChild;
impl GuardedChild { fn id(&self)->u32; fn child(&self)->&Child; fn child_mut(&mut self)->&mut Child;
                   fn kill(&mut self)->io::Result<()>; fn wait(&mut self)->io::Result<ExitStatus>;
                   fn try_wait(&mut self)->io::Result<Option<ExitStatus>>; }
```

- `GuardedChild::drop`：`TerminateJobObject` → 关闭 job 句柄（KILL_ON_JOB_CLOSE 兜底）→ `wait` 回收。
- 非 Windows：`cfg(not(windows))` 退化为普通 `spawn`（Linux/macOS 隔离在 M4，不假装有）。
- `cargo test -p dshr-sidecar-guard` 覆盖：真实 `cmd /c echo` 子进程读 stdout、
  `Drop` 后进程确实消失（`tasklist` 校验）、CPU tick 换算、环境清洗、Node 解析。

### 环境变量清洗

```rust
pub fn is_secret_env(key: &str, allowlist: &[&str]) -> bool;
pub fn sanitize_env(vars: &[(String, String)], allowlist: &[&str]) -> Vec<(String, String)>;
pub fn inherited_env(allowlist: &[&str]) -> Vec<(String, String)>;
```

- 剔除（大小写不敏感）：`DEEPSEEK_API_KEY`、`*_KEY`、`*_TOKEN`、`*_SECRET`；
- `allowlist` 命中的名字保留（调用方显式审计过的变量）；
- `spawn_guarded` 的策略：**先 `env_clear()`**，再写入清洗后的宿主环境，最后叠加
  `env` 参数 —— 显式传入的变量不过滤（代表显式授权，例如 sidecar 自己的
  `DSHR_DSH_PLUGINS`）。宿主 `std::env::set_var` 设置的变量仍会作为"清洗后的继承
  环境"传给子进程，所以现有 chat 的插件传参方式不需要改。

### 独立 profile 目录

```rust
pub fn ensure_profile_dir(root: impl AsRef<Path>) -> io::Result<ProfileDirs>;
pub struct ProfileDirs { pub root: PathBuf, pub node_modules: PathBuf, pub logs: PathBuf, pub tmp: PathBuf }
```

布局：`<root>/{node_modules,logs,tmp}`；幂等创建。sidecar 的插件安装、日志与一次性
文件集中在同一目录，便于整体清理与单独审计（PLAN §7「每个插件一个数据目录」的
最小落点；当前仍是**所有插件共用**一个 node_modules，逐插件数据目录未做）。

### 安全边界（必须写进用户文档的口径）

- **Job Object 不是权限沙箱**：它限制资源与进程生命周期，不限制文件/网络访问；
  Node 插件仍以当前用户身份执行任意代码（`PLAN` §7「Sidecar 不是沙箱」）。
- **不承诺记录进程副作用**：审计只记录"调用与结果"，无法证明插件在 Node 里做了什么。
- **低完整性/受限 token 未实现**：Windows 上仅 Job Object；restricted token 与
  AppContainer 在 M4 安全加固里做。
- **安装确认**：CLI `--install-plugin` 本身就是显式动作；GUI 安装流程必须展示
  「该插件可在你的用户权限下执行任意代码」并要求确认（`PLAN` §7 第 3 条，UI 未接）。
- **不执行 npm 生命周期脚本**：当前安装只解包/复制，从不运行 `npm`；将来接 pnpm
  安装时必须"显式确认后执行并写审计日志"（`PLAN` §5.5）。

## 3. 插件安装：`.tgz` 离线解包（Gate 4 补齐）

`crates/profile/src/install.rs::install_package` 现在接受两种来源：

1. **目录**（原有路径）：复制到 `<sidecar>/node_modules/<name>`；
2. **`.tgz`**（npm pack 产物）：解包到系统临时目录 → 走完全相同的校验/复制流程 →
   临时目录 Drop 清理。

```rust
pub fn install_package(sidecar_dir: &Path, package: &Path, profile: &mut Profile)
    -> Result<InstallReport, String>;
// chat --install-plugin 的接线不变：传文件路径即可
```

### 校验与安全边界

- `package.json`：容忍 UTF-8 BOM；`name` 必填且不含 `..`/绝对路径/反斜杠；
- `dsh` 字段（若存在，官方 tool 包没有）：必须是对象；`dsh.bundle` 是对象，
  `dsh.bundle.patch` 必须是相对路径且文件真实存在；`dsh.client` 是对象，
  `client.platform` 字符串、`client.inject` 数组；
- tar 条目：拒绝绝对路径、`..`、反斜杠、Windows 保留设备名（CON/NUL/COM1…）、
  符号链接、硬链接、设备文件；
- 大小限制：≤ 20000 条目、单文件 ≤ 64 MiB、解压总量 ≤ 512 MiB；
- 头 checksum 校验；支持 PAX `path=` 与 GNU 长文件名（npm pack 可能产生）；
- 不联网、不执行包内脚本、不做依赖解析（包必须已完整）。

### 依赖说明

`flate2 = "=1.1.11"` 已在 `Cargo.lock` 中（锁定版本、本机 registry 缓存已有），
已验证 `cargo test --offline` 可构建；`tar` 不在锁里，因此**自己写了最小 tar reader**
（`crates/profile/src/archive.rs`，约 300 行 + 测试），不新增任何外部依赖。

### 用法

```powershell
# 目录安装（原有）
tools\build-env.cmd cargo run -p dshr-desktop-gpui --bin chat -- --install-plugin path\to\my-tool

# npm pack 产物（离线）
npm pack @deepseek-ai/dsh-tool-fs            # 得到 dsh-tool-fs-0.2.2-rc.1.tgz
tools\build-env.cmd cargo run -p dshr-desktop-gpui --bin chat -- --install-plugin dsh-tool-fs-0.2.2-rc.1.tgz
```

安装后重启 chat 生效（Loader 只读模式下拒绝安装）。依赖闭包仍需提前存在于
`node_modules`（官方包用 `tools/dsh-vendor.mjs` 从本机 DSH 抽取），这是"不做 npm
网络解析"的代价。

## 4. chat.rs 接线建议（Lead 执行）

`chat.rs` 与 `compat-dsh-host` 不在本轮写域，下面给出可直接落地的缝：

```rust
// Cargo.toml
// [dependencies.dshr-sidecar-guard]
// path = "../../crates/sidecar-guard"

// spawn_sidecar() 里解析 Node（None → Native-only，打印提示）
let node = dshr_sidecar_guard::resolve_node();
if node.is_none() {
    eprintln!("[chat] 未找到 Node 运行时：先运行 `node tools/vendor-node.mjs`，或用 DSHR_NODE 指定");
}
let program = node.as_ref().map(|n| n.program.clone()).unwrap_or_else(|| PathBuf::from("node"));

// 交进程的守护启动（Command 已带 program/args/stdio 配置）
let mut command = std::process::Command::new(program);
command.arg(&script).arg(&plugin).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::inherit());
let child = dshr_sidecar_guard::spawn_guarded(
    command,
    &working_dir,
    &[],                                     // 继承环境已自动清洗
    &dshr_sidecar_guard::GuardLimits::sidecar_default(),
)?;
```

`SidecarHost::spawn` 目前内部固定 `Command::new("node")`，建议在 `compat-dsh-host`
新增一个最小入口（二选一）：

- `SidecarHost::spawn_with(program: &Path, script, plugin, limits: &GuardLimits)`：
  crate 依赖 `dshr-sidecar-guard`，内部把 spawn 换成 `spawn_guarded`；
- 或 `SidecarHost::spawn_command(command: Command, limits: &GuardLimits)`：由 chat
  构造完整 Command（含 env/stdio），Host 只负责协议握手。
  后一种对 `compat-dsh-host` 的改动更小，也不把 guard 类型泄漏到协议层。

另建议启动时 `ensure_profile_dir(DSHR_PROFILE_DIR)`，把插件 profile 与日志放进
独立目录（当前默认 `.tmp/profile` 也可直接作为 root）。

## 5. 开发分发打包（`tools/package-dev.ps1`）

```powershell
pwsh tools/package-dev.ps1              # release 构建 + 打包
pwsh tools/package-dev.ps1 -SkipBuild   # 复用 target/release/chat.exe
pwsh tools/package-dev.ps1 -Configuration debug
# Windows PowerShell 5.1（本机没有 pwsh 时）：
powershell -NoProfile -ExecutionPolicy Bypass -File tools/package-dev.ps1 -Configuration debug -SkipBuild
```

产物 `dist/dshr-dev.zip`（gitignored）：

```text
chat.exe
sidecar/            # 含 dsh-official/node_modules（vendored 官方插件）与 cordis sidecar
runtime/node/       # node.exe + RUNTIME.json
docs/               # M0–M2 文档
README.md
MANIFEST.txt        # builtAt / configuration / gitCommit / node 版本
```

前置：`node tools/vendor-node.mjs`、`node tools/dsh-vendor.mjs` 各跑一次（都是本地
操作，不联网）。脚本只写 `dist/`；不做签名、安装器、自动更新或发布。
验证：解开 zip 后 `chat.exe --selftest`（无 Node 时仍应 Native-only 可用）。

## 6. 未做项与风险

| 项 | 状态 | 说明 |
|---|---|---|
| pnpm 捆绑 | ❌ M3/M4 | 本机 DSH 自带 pnpm 11.28.5，但复制它会连带 `dist/node_modules` 闭包；需要单独评估体积与离线 lockfile |
| 逐插件数据目录 | ❌ | 当前所有插件共用一个 `node_modules`；`PLAN` §7 第 2 条要按插件隔离 |
| 受限 token/低完整性 | ❌ | 仅 Job Object；不是完整 OS 沙箱 |
| 安装审计日志 | ❌ | 安装确认 UI、`install` 审计事件未接（chat 接线后补） |
| npm 生命周期脚本 | ❌ 不做 | 当前安装从不执行脚本；将来接 pnpm 时按"显式确认 + 审计"实现 |
| 依赖树解析 | ❌ 不做 | `.tgz` 安装要求依赖已存在（官方包靠 `dsh-vendor.mjs`） |
| m2-plugins.md 安装章节 | ⚠️ 待 Lead | 该文档不在本轮写域；`.tgz` 用法以上文 §3 为准 |
| macOS/Linux 隔离 | ❌ M4 | 非 Windows 仅普通 spawn，不粉饰为沙箱 |

## 7. 验收命令与结果（2026-11-11）

```powershell
tools\build-env.cmd cargo test -p dshr-sidecar-guard -p dshr-profile
node --check tools/vendor-node.mjs
node tools/vendor-node.mjs --check
```

结果：

- `dshr-profile`：**24 passed**（含 tar/tgz 解包、路径逃逸/链接拒绝、PAX 长路径、
  `dsh` 字段校验、目录安装既有用例、默认 profile 写工具开关）；
- `dshr-sidecar-guard`：**13 passed**（Job Object spawn/Drop 杀进程、环境清洗、
  allowlist、profile 目录、Node 解析三布局与 PATH 兜底）、doc-test **1 passed**；
- `node --check tools/vendor-node.mjs`：通过；本机 vendor 实测 Node **v24.21.0**
  （89.2 MiB，来源 `%LOCALAPPDATA%\Programs\DeepSeek Harness\resources\runtime\primary-runtime\dependencies\node\bin\node.exe`）；
- `cargo test ... --offline` 同时验证了 flate2 离线可用（未访问网络）。
- `tools/package-dev.ps1 -Configuration debug -SkipBuild` **实测通过**：
  `dist/dshr-dev.zip` 46.1 MiB（staged 130.6 MiB，1329 个条目），已核对
  `chat.exe` / `sidecar/dsh-official/sidecar-dsh.mjs` / `runtime/node/node.exe` /
  `runtime/node/RUNTIME.json` / `docs/m2-runtime.md` / `MANIFEST.txt` 均在包内。
