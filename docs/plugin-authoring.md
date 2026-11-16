# 外部插件作者指南（ADC / CPP-v1，M3 Preview）

> 适用对象：想给 dshr 写插件的**外部作者**。本文只依赖公开规范与模板，**不要求读主程序 `crates/` 源码**。
> 状态：M3 Preview（G2 交付）。协议权威文档：[`spec/cpp-v1.md`](spec/cpp-v1.md) §12；元素词汇表：[`spec/adc-v1.md`](spec/adc-v1.md) §6；信任链：[`spec/plugin-trust.md`](spec/plugin-trust.md)。

---

## 1. 插件形态：先选一种

| 形态 | 运行时 | 适合 | 本文 |
|---|---|---|---|
| **external-process（推荐首次尝试）** | 独立子进程，NDJSON over stdio（`cpp-process/v1`） | 任意语言/任意构建产物；崩溃不拖垮宿主；有内存/超时/配额隔离 | ✅ 本文 |
| rust-static / wasm-component | 进程内 / WASM | 官方或深度集成插件；WASM 主机 M3 尚未就绪 | 规范见 [`cpp-v1.md`](spec/cpp-v1.md) §3 |
| DSH JS 插件 | Node sidecar | 复用 DSH 生态的 JS 工具包 | 走 sidecar 加载，另见 [`spec/dsh-compat-mapping.md`](spec/dsh-compat-mapping.md) |

本文走 **external-process**：一个可执行文件 + 一份 manifest，宿主拉起进程、按行交换 JSON。
它提供两类落点：

1. **Tool**：模型可调用的工具（名字/描述/JSON Schema/结果），会被注册进工具表。
2. **Panel**：挂在 ADC-v1 slot 上的只读面板（例如 `workspace.panel`），元素树由 `ui.render` 提供。

> **M3 Preview 边界**：`chat --install-plugin` 安装链路已可跑（[`../tools/authoring-check.ps1`](../tools/authoring-check.ps1) 实测）；external-process 的**装载/调用/卸载**已接进 chat 产品路径（启动时按 profile enabled 条目装载、`chat --selftest-external-plugin` 覆盖安装→装载→调用→卸载→重载），宿主库集成测试（`cargo test -p dshr-plugin-host`）覆盖超时/内存/崩溃隔离。**Panel 的产品渲染**（`ui.render` → GPUI）仍在后续轮次；工具调用链路可用。

## 2. 5 分钟快速开始

```powershell
# 0) 前提：Rust stable（edition 2021）、本机已能离线构建 dshr
# 1) 从模板生成插件（仓库内只允许生成到 .tmp/）
node tools/new-plugin.mjs --out .tmp/my-first-plugin --id demo.echo --name "Demo Echo"

# 2) 构建（建议把 target 放到插件目录外，避免 target/ 被计入包摘要）
cd .tmp/my-first-plugin
$env:CARGO_TARGET_DIR = "<repo>\.tmp\authoring-target\demo-echo"
& "<repo>\tools\build-env.cmd" cargo build --release
Copy-Item "$env:CARGO_TARGET_DIR\release\demo-echo.exe" "bin\demo-echo.exe"

# 3) 打包（校验清单、计算 dshr-trust/v1 摘要、写 dshr-trust.json）
node "<repo>\tools\plugin-pack.mjs" --plugin . --out ..\pkg-demo-echo

# 4) 安装（未签名包首次安装需确认；预览会展示权限）
"<repo>\target\debug\chat.exe" --install-plugin ..\pkg-demo-echo --yes-install

# 5) 看审计
Get-Content "<profile>\install-audit.jsonl"
```

完整 E2E（生成 → 构建 → 打包 → 安装 → 升级 → 降级拒绝）由 [`tools/authoring-check.ps1`](../tools/authoring-check.ps1) 跑。

## 3. 插件目录结构

```text
my-plugin/
├── package.json      # { "name": "<plugin_id>", "version": "0.1.0" }（安装器用它确定包名/版本）
├── plugin.toml       # CPP-v1 manifest：plugin.id/version/api、runtime、permissions
├── bin/
│   └── my-plugin.exe # [runtime].entry 指向的包内可执行文件（构建后复制进来）
├── src/ …            # 你的源码（随包分发与否不影响协议）
└── dshr-trust.json   # 可选：由 plugin-pack 生成；不参与摘要
```

**硬性规则**（`plugin-pack` 与宿主都会校验）：

- `package.json` 的 `name`/`version` 必须与 `plugin.toml` 的 `[plugin].id`/`[plugin].version` 一致；
- `[runtime].kind = "external-process"`，`[runtime].entry` 必须是**包内相对路径**且用 `/` 分隔；Windows 下必须是 `.exe`（如 `bin/my-plugin.exe`）。拒绝 `..`、绝对路径、符号链接（spec §12.6）；
- `plugin.toml` 只允许 manifest schema 列出的字段（未知字段拒绝），见 [`../schema/cpp-v1/manifest.schema.json`](../schema/cpp-v1/manifest.schema.json)；
- 编码：`package.json` 允许 UTF-8 BOM（宿主与 `plugin-pack` 会去掉），但 `plugin.toml` 必须是 **UTF-8 无 BOM**（TOML 解析器可能拒绝 BOM）；`dshr-trust.json` 由 `plugin-pack` 生成，始终无 BOM；
- 工具/面板**不在** `plugin.toml` 里声明（external-process 在 `process.ready` 里内联声明），见 §5/§6。

`plugin.toml` 最小示例：

```toml
[plugin]
id = "demo.echo"
version = "0.1.0"
api = "cordis-portable/v1"
description = "Echo tool + workspace.panel demo"

[runtime]
kind = "external-process"
entry = "bin/demo-echo.exe"
max_memory_mb = 128     # 1–512，映射到 Job Object 内存上限
timeout_ms = 60000      # 单次调用/握手兜底（spec §12.4）

[permissions]
workspace_read = false
workspace_write = false
network = false
process_spawn = false
credential_use = false
```

> 权限字段只声明插件**想要**什么；真正的文件/工具权限仍由宿主与 Agent 策略裁决。安装预览会把权限展示给用户。

## 4. 用 SDK 写工具

SDK crate：`dshr-plugin-sdk`（0.1.0，**自包含、无仓库内依赖**）。生成器会把 SDK 复制到样例的
`<out>/sdk/`，模板用 `dshr-plugin-sdk = { path = "sdk" }`；在 SDK 正式发布到 registry 之前，
这是"内部作者 Preview"口径（见 [ADR-0012](decisions/0012-m3-gate-closure.md)），发布后改成
`dshr-plugin-sdk = "0.1"` 即可。
入口是一个 `ChildSession`：读 stdin 的 NDJSON、写 stdout，SDK 负责握手/取消/错误帧。

工具名必须匹配 `[a-z][a-z0-9_]*`（≤ 64 字节）且**全局唯一**；建议带插件前缀（模板生成器会派生 `<crate>_echo`）。
描述 ≤ 1 KB；`input_schema`/`output_schema` 是**内联** JSON 对象（各 ≤ 8 KiB，不读文件路径）。

```rust
use dshr_plugin_sdk::{CancelFlag, ChildSession, ReadyDecls, ToolCall, ToolDecl, ToolError, ToolResult};

fn main() -> Result<(), dshr_plugin_sdk::SessionError> {
    // 1) 声明工具：名字 + 描述 + 内联 JSON Schema（≤ 8 KiB）。
    let echo = ToolDecl::new("echo", "Echo the input text back")
        .with_input_schema(serde_json::json!({
            "type": "object",
            "required": ["text"],
            "properties": { "text": { "type": "string", "maxLength": 4096 } }
        }))
        .with_output_schema(serde_json::json!({ "type": "object" }))
        .with_timeout_ms(30_000)   // 默认 120 s；宿主 clamp 到 1–600 s
        .with_cancelable(true)     // 默认 true
        .with_presentation("generic"); // generic/terminal/diff/search/read/web；未知值退回 generic

    let decls = ReadyDecls::new(vec![echo], Vec::new()); // (tools, panels)

    // 2) 处理调用：闭包自动实现 ToolHandler。
    //    闭包参数/返回类型必须写出来（blanket impl 推断不出）。
    ChildSession::serve(decls, |call: &ToolCall, cancel: &CancelFlag| -> Result<ToolResult, ToolError> {
        if cancel.is_cancelled() {
            return Err(ToolError::Cancelled);
        }
        match call.tool.as_str() {
            "echo" => {
                let text = call
                    .argument("text")
                    .and_then(|value| value.as_str())
                    .ok_or_else(|| ToolError::invalid_arguments("missing string `text`"))?;
                Ok(ToolResult::text(text))
            }
            // SDK 已按 process.ready 的声明预检工具名（未声明直接回 E_SCHEMA，不会进这里）；
            // 这个分支只是防御性代码。
            other => Err(ToolError::invalid_arguments(format!("unknown tool `{other}`"))),
        }
    })
}
```

### 4.1 慢工具与取消

SDK 不会在计算中途打断你——长任务要自己检查取消标记，并在 `timeout_ms` 内返回：

```rust
// 同一个 handler 的 match 分支：
"echo_slow" => {
    let ms = call.argument("ms").and_then(|value| value.as_u64()).unwrap_or(1_000);
    let deadline = std::time::Instant::now() + std::time::Duration::from_millis(ms);
    while std::time::Instant::now() < deadline {
        if cancel.is_cancelled() {
            return Err(ToolError::Cancelled);
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    Ok(ToolResult::text(format!("slept {ms} ms")))
}
```

### 4.2 返回值与错误

`ToolResult`（对齐 [`cpp-v1.md`](spec/cpp-v1.md) §7.3）：

```json
{
  "content": [{ "type": "text", "text": "3 rules failed" }],
  "value": { "rules_failed": 3 },
  "meta": null,
  "is_error": false
}
```

- `content` 非空、**模型可见**；`text` / `json` / `image`（`image` 必须引用附件 id，不能是 URL）。
- `value` 是结构化返回值（展示/回放用）；`meta` 是展示元数据。
- `is_error: true` 表示**工具执行失败**（模型可见）；协议错误用 `E_*`，二者不要混用。
- 单值 ≤ 1 MB；超出应改为附件引用 `{"$attachment": "<id>", "size": 4194304}`（宿主侧还有 8 MB 上限）。
- 便捷构造：`ToolResult::text(..)`、`ToolResult::json(..)`、`ToolResult::error(..)`、`.with_value(..)`、`.with_meta(..)`。
- SDK 在调用 handler 前按 `process.ready` 的声明预检工具名：未声明的 tool 直接回 `E_SCHEMA`，不会进 handler。
- 结果帧超过 1 MiB（`MAX_FRAME_BYTES`）时 SDK 自动改发 `E_TOO_LARGE`；大结果请改附件引用。

`ToolError` 与错误码映射：

| `ToolError` | 错误码 | 何时用 |
|---|---|---|
| `InvalidArguments(msg)` | `E_SCHEMA` | 参数缺失/不符合你的 input schema |
| `Cancelled` | `E_CANCELLED` | 收到取消后主动放弃 |
| `TooLarge(msg)` | `E_TOO_LARGE` | 结果超出宿主上限 |
| `Failed(msg)` / `Internal(msg)` | `E_INTERNAL` | 工具执行失败（`Failed` 请写可读原因） |

### 4.3 取消与超时

- 宿主取消时回调线程会收到 `cancel.is_cancelled() == true`；**收到取消后 MUST 在 5 s 内返回**，否则宿主标记 `aborted` 并丢弃迟到结果。
- 工具自身的 `timeout_ms` 是硬上限：超时宿主杀进程（Job Object），会话不产生脏写；所以长时间任务要自己分片并检查取消。
- 已结算的调用不可再取消（`E_ALREADY_SETTLED`）。

## 5. 用 SDK 写面板（ADC-v1）

面板 = `PanelDecl` + 一个 view + `ui.render` 返回的**元素树**。

```rust
use dshr_plugin_sdk::{
    CancelFlag, ChildSession, PanelDecl, ReadyDecls, ToolCall, ToolError, ToolResult,
};

let panel = PanelDecl::new(
    "workspace.panel",              // slot_id：必须是 ADC-v1 slot 目录里的 id
    "demo.echo.panel",              // registration id（同作用域唯一）
    "Echo Panel",                   // label
    "demo.echo.panel.view",         // view 名：ui.render 用它分派
)
.with_location("right")             // left/right/bottom/center
.with_order(100.0);                 // list slot 排序（越小越靠前）

ChildSession::serve_with_views(
    ReadyDecls::new(Vec::new(), vec![panel]),
    |view: &str, _state_seq: u64| -> Result<serde_json::Value, ToolError> {
        match view {
            "demo.echo.panel.view" => Ok(panel_tree()),
            other => Err(ToolError::internal(format!("unknown view `{other}`"))),
        }
    },
    |_call: &ToolCall, _cancel: &CancelFlag| -> Result<ToolResult, ToolError> {
        Ok(ToolResult::text("panel-only plugin: no tools declared"))
    },
)?;

fn panel_tree() -> serde_json::Value {
    serde_json::json!({
        "type": "Stack",
        "props": { "gap": 8, "padding": 12 },
        "children": [
            { "type": "Text", "props": { "value": "Hello from demo.echo", "style": "title" } },
            { "type": "Badge", "props": { "label": "external-process", "tone": "info" } },
            {
                "type": "Button",
                "props": {
                    "label": "复制插件 ID",
                    "a11yLabel": "复制插件 ID demo.echo",   // 交互元素运行时必须有
                    "onPress": { "copy": "demo.echo" }
                }
            }
        ]
    })
}
```

要点：

- **slot 只能选宿主预置的**（v1 首批 12 个，见 [`spec/adc-v1.md`](spec/adc-v1.md) §5 与 [`../schema/adc-v1/slots.v1.json`](../schema/adc-v1/slots.v1.json)）；插件不能声明新 slot。
- **元素只能是冻结的 26 个**；未知元素运行时降级为 `Placeholder` 并诊断，但作者期 lint（[`../schema/adc-v1/element.schema.json`](../schema/adc-v1/element.schema.json)）会判 invalid——先按 schema 写对。
- **交互元素（Button/Input/…）必须给 `a11yLabel`**（运行时拒绝并记 `ADC-ELEM-A11Y-001`）。
- 配额：树深 ≤ 8、节点 ≤ 200、文本 ≤ 8 KB；元素不会触发网络，图片只能引用附件。
- 动作只允许 `call` / `command` / `openExternal`（仅 https，需用户确认）/ `copy`。
- `ui.action` 会把动作回传给插件；`openExternal` 由宿主处理，插件不要自己打开链接。

## 6. 构建、打包与信任

### 6.1 构建

```powershell
$env:CARGO_TARGET_DIR = "<缓存目录>\my-plugin"   # 放插件目录外，避免 target/ 进包
cargo build --release
Copy-Item "$env:CARGO_TARGET_DIR\release\my-plugin.exe" "bin\my-plugin.exe"
```

模板的 SDK 依赖是 `<out>/sdk` 的相对 path（生成器复制自包含 SDK）；**样例工程不引用本仓库绝对路径**，
可以整体拷到任意目录离线构建。SDK 正式发布到 registry 后改成 `dshr-plugin-sdk = "0.1"`。
不要依赖宿主进程内的任何 Rust 类型——跨进程只有 JSON。

### 6.1b 离线 SDK 渠道（M4 W1）

没有 registry 时用可校验的离线包分发 SDK：

```powershell
node tools/sdk-pack.mjs --out dist/sdk            # 生成 dist/sdk/sdk-manifest.json + SHA256SUMS + dshr-plugin-sdk-0.1.0/
node tools/sdk-pack.mjs --verify dist/sdk         # 逐文件哈希校验；篡改即非零退出
node tools/new-plugin.mjs --out .tmp/my-plugin --sdk dist/sdk/dshr-plugin-sdk-0.1.0
```

- `sdk-manifest.json` 记录 `name/version/package_dir/files[{path,size,sha256}]`；
  `SHA256SUMS` 是 `sha256sum` 兼容的校验和列表。
- 生成器仍把 SDK 拷到 `<plugin>/sdk/`，作者的 `Cargo.toml` 只有 `path = "sdk"`，
  所以离线包可以整体拷到无仓库、无网络的环境。
- `--verify` 严格比对清单（缺文件/多文件/哈希或大小不符都失败）；分发前先跑一遍。
- registry、CA/证书体系与自动更新仍属 M4 W7，不在本渠道范围内。

### 6.2 打包与 `dshr-trust.json`

```powershell
node tools/plugin-pack.mjs --plugin <插件目录> --out <输出包目录>            # 未签名
node tools/plugin-pack.mjs --plugin <插件目录> --out <输出包目录> --key <private.pem> --key-id <key_id>  # ed25519 签名
```

打包器会：校验 `package.json`/`plugin.toml` 一致性与 `entry` 存在 → 复制内容（跳过 `dshr-trust.json`、`.git/`、`node_modules/`、点开头目录）→ 按规范算法计算摘要 → 写 `dshr-trust.json`；带 `--key/--key-id` 时追加 `signature`。

摘要算法（[`spec/plugin-trust.md`](spec/plugin-trust.md) §3，**逐字节可复现**）：

1. 只收普通文件（符号链接/硬链接/设备文件跳过）；
2. 跳过 `dshr-trust.json`、`.git/`、`node_modules/`、任何 `.` 开头的目录；
3. 相对路径用 `/` 分隔，按 UTF-8 **字节序升序**排序；
4. 每个文件依次 `path_utf8 + 0x00 + size_decimal_utf8 + 0x00 + file_bytes`；
5. SHA-256 → 64 位小写 hex；`dshr-trust.json` 自身不参与。

常见坑：

- **摘要排除 `dshr-trust.json` 自身**：它在打包最后写入；不要改了包内文件后复用旧 `dshr-trust.json`（重跑 `plugin-pack`）。
- **BOM**：`package.json` 带 UTF-8 BOM 可被宿主/打包器容忍；`plugin.toml` 必须无 BOM（TOML 解析器可能拒绝）；`dshr-trust.json` 由打包器生成，始终无 BOM。
- **Windows entry**：必须是包内相对路径的 `.exe`（`bin/my-plugin.exe`，`/` 分隔）；`plugin-pack` 拒绝 `..`、绝对路径、符号链接。
- **别把 `target/` 打进包**：用 `CARGO_TARGET_DIR` 指到插件目录外，否则构建产物会进摘要。

`dshr-trust.json`（[`../schema/cpp-v1/trust.schema.json`](../schema/cpp-v1/trust.schema.json)）：

```json
{
  "schema_version": 1,
  "plugin_id": "demo.echo",
  "version": "0.1.0",
  "package": { "digest_sha256": "<64 位小写 hex>" }
}
```

**可选签名**（M4 W1 起支持密钥吊销/时间窗/轮换）：

```powershell
# 1) 生成作者密钥对（private.pem / public.hex / trust-key.json）
node tools/plugin-pack.mjs --generate-key .tmp/author-keys --key-id author-2026

# 2) 把公钥登记到本机信任库（宿主的 trust-keys.json）
target\debug\chat.exe --trust-key add author-2026 <public_key_hex>

# 3) 打包时签名；公钥会打印出来，与 trust-key.json 核对
node tools/plugin-pack.mjs --plugin <插件目录> --out <输出包目录> --key .tmp/author-keys/private.pem --key-id author-2026
```

```text
签名输入 = "dshr-trust/v1\n<plugin_id>\n<version>\n<digest_sha256_hex>"
```

ed25519 签名的 `value_hex`（128 位小写 hex）与 `key_id` 写进 `signature` 字段；宿主用**本地** `<profile>/trust-keys.json` 验签。签名有效即可免首装确认；签名缺失/无效/未知密钥/吊销/过期/未生效的处理见规范 §4。**私钥永远不要进包、不要提交**（`.gitignore` 也应排除 `*.pem`）。

### 6.3 安装、升级、卸载

```powershell
# 安装（未签名包必须 --yes-install 确认；预览展示包名/版本/来源/摘要/签名状态/权限）
target\debug\chat.exe --install-plugin <包目录> --yes-install

# 可选环境变量（测试/多 profile）
$env:DSHR_PROFILE_DIR = "<profile>"     # 默认 .tmp/profile
$env:DSHR_SIDECAR_DIR = "<sidecar>"     # 默认 <app_root>/sidecar/dsh-official
```

- **包落点**：`<sidecar>/node_modules/<name>`；安装元数据 `<dest>/.dshr-install.json`；profile 条目 `<profile>/plugins.json`。
- **审计**：`<profile>/install-audit.jsonl`，一行一条；接受与拒绝都写（`decision: accepted|rejected`）。
- **升级**：再次 `--install-plugin` 更高版本即可（原地替换；profile 条目复用）。
- **降级拒绝**：候选版本低于已安装版本 → `E_TRUST_DOWNGRADE`（退出码 1）；显式 `--allow-downgrade` 才允许，且审计 `downgrade: true`。
- **篡改拒绝**：摘要不匹配 → `E_INTEGRITY_MISMATCH`（退出码 1）。
- **未确认**：不带 `--yes-install` 且需要确认 → 退出码 2，不安装。
- **卸载（M4 W1）**：`target\debug\chat.exe --uninstall-plugin <包名>`。先 rename 插件目录到同父目录的 `.dshr-remove-*` 再删除（失败恢复原名），随后移除 `<profile>/plugins.json` 同名条目并写 `install-audit.jsonl`（`action: "uninstall"`）。目录已删但 profile 落盘失败时返回错误，重跑即可收敛。

**信任密钥库管理**（`<profile>/trust-keys.json`，变更写 `trust-key-audit.jsonl`）：

```powershell
target\debug\chat.exe --trust-key list
target\debug\chat.exe --trust-key add <key_id> <public_key_hex> [--note "作者 2026"]
target\debug\chat.exe --trust-key revoke <key_id>
target\debug\chat.exe --trust-key remove <key_id>
target\debug\chat.exe --trust-key rotate <old_key_id> <new_key_id> <public_key_hex>
```

- `revoke` 保留公钥与吊销时间，旧钥签名的包立即被 `E_TRUST_KEY_REVOKED` 拒绝；`remove` 硬删除（`E_TRUST_UNKNOWN_KEY`）。
- `rotate` 先登记新钥、再吊销旧钥；作者用新钥重签并重新安装即可，不需要删 profile。
- 密钥可选 `not_before_ms` / `not_after_ms`；安装路径按当前时间检查（`E_TRUST_KEY_NOT_YET_VALID` / `E_TRUST_KEY_EXPIRED`）。

**本地开发免签名流程**：直接打包不带 `dshr-trust.json` 的目录（或不用 plugin-pack，把源码目录当包），安装时 `--yes-install` 确认即可——宿主标记 `unsigned` 并审计，但仍会复制与装载。**不要**用这种方式分发。

## 7. 测试与排错

| 现象 | 先看什么 |
|---|---|
| 插件启动即失败 | 宿主 10 s 内没收到 `process.ready`：检查 `entry` 路径、exe 是否可执行、是否先读了 stdin 才写 ready（SDK 已处理握手顺序） |
| 工具没出现 | `process.ready.tools[]` 是否声明；工具名是否 `[a-z][a-z0-9_]*` 且全局唯一（冲突 → `E_TOOL_CONFLICT`） |
| stdout 出现乱码/协议错误 | 你是不是用了 `println!`/第三方库往 stdout 打日志？**stdout 只写协议**，日志一律 `eprintln!` |
| 调用无响应 | handler 是否阻塞超过 `timeout_ms`？宿主 15 s 发一次 `process.ping`，阻塞主线程不影响 ping（SDK 单独线程），但工具会被超时杀掉 |
| 面板空白 | `ui.render` 的 view 名是否与 `PanelDecl.view` 一致；元素树是否通过 `schema/adc-v1/element.schema.json`；slot 是否在目录内 |
| 安装被拒 | 错误码：`E_TRUST_MISSING`（可确认）、`E_INTEGRITY_MISMATCH`（包被改过，重新打包）、`E_TRUST_MANIFEST`（清单/trust 不一致）、`E_TRUST_DOWNGRADE`（版本回退） |

自测命令：

```powershell
node tools/plugin-pack.mjs --verify <包目录> --digest <hex>   # 摘要可复现
node tools/sdk-pack.mjs --out dist/sdk && node tools/sdk-pack.mjs --verify dist/sdk  # SDK 离线包
target\debug\chat.exe --install-plugin <包目录> --yes-install  # 未签名安装链路
target\debug\chat.exe --install-plugin <签名包目录>            # 已验证签名免确认
target\debug\chat.exe --uninstall-plugin <包名>                # 卸载 + 审计
target\debug\chat.exe --selftest-plugin-lifecycle              # 安装→卸载→密钥轮换产品路径
```

手工看握手（把一行 `process.hello` 喂给 stdin，stdout 应回一行 `process.ready`；进程读到 EOF 即退出）：

```powershell
'{"version":"cpp-process/v1","kind":"process.hello","payload":{"protocol":"cpp-process/v1","host_version":"0.1.0","plugin_id":"<id>","generation":1}}' | .\bin\<name>.exe
```

协议帧速查（全文 [`spec/cpp-v1.md`](spec/cpp-v1.md) §12.2）：

```text
host → process.hello  { protocol, host_version, plugin_id, generation, manifest }
child → process.ready { protocol, plugin_id, tools[], panels[] }
host → tool.invoke    { tool, arguments, timeout_ms, cancelable }
child → tool.result   { content[], value?, meta?, is_error }
host → tool.cancel / process.ping → child → process.pong
host → process.shutdown → child → process.bye → exit
```

## 8. "作者未读主程序源码"验收清单

发布前逐条勾选（也用于 G2 验收）：

- [ ] 只按本文 + `docs/spec/cpp-v1.md` §12 + `docs/spec/adc-v1.md` §6 实现，没有读 `crates/` 源码；
- [ ] `node tools/new-plugin.mjs` 能生成可构建项目；用 `--sdk` 指向 `tools/sdk-pack.mjs` 的离线包也能构建；
- [ ] `node tools/sdk-pack.mjs --verify <sdk-dist>` 通过，篡改 SDK 后非零退出；
- [ ] `cargo build --release` 通过，`bin/<name>.exe` 存在；
- [ ] `plugin.toml` 与 `package.json` 的 id/version 一致；
- [ ] `node tools/plugin-pack.mjs` 成功并打印 64 位摘要；
- [ ] `--generate-key` + `--key/--key-id` 产出 `signature`，`chat --install-plugin` 免 `--yes-install` 安装且元数据 `signature.status=verified`；
- [ ] `chat --install-plugin <pkg> --yes-install` 成功，`<sidecar>/node_modules/<name>` 与 `<profile>/plugins.json` 有包；
- [ ] `<profile>/install-audit.jsonl` 有 `decision: "accepted"`；
- [ ] 工具可被调用并返回符合 §7.3 的结果；
- [ ] panel 的 `ui.render` 元素树通过 ADC-v1 schema，交互元素带 `a11yLabel`；
- [ ] 篡改包内容后重装被 `E_INTEGRITY_MISMATCH` 拒绝；
- [ ] 降级安装被 `E_TRUST_DOWNGRADE` 拒绝；
- [ ] 升级安装成功，审计新增 accepted；
- [ ] `chat --uninstall-plugin <name>` 后包目录与 profile 条目被移除，审计新增 `action: "uninstall"`；
- [ ] 密钥轮换后旧钥签名的包被 `E_TRUST_KEY_REVOKED` 拒绝，新钥重签可安装。

## 9. 参考

- 协议：[`spec/cpp-v1.md`](spec/cpp-v1.md) §3（manifest）、§7（Tool）、§12（external-process）
- 元素与 slot：[`spec/adc-v1.md`](spec/adc-v1.md) §5/§6；[`../schema/adc-v1/slots.v1.json`](../schema/adc-v1/slots.v1.json)、[`../schema/adc-v1/element.schema.json`](../schema/adc-v1/element.schema.json)
- 信任与安装：[`spec/plugin-trust.md`](spec/plugin-trust.md)
- 模板：[`../templates/external-tool-plugin/`](../templates/external-tool-plugin/)；生成器：[`../tools/new-plugin.mjs`](../tools/new-plugin.mjs)；打包器：[`../tools/plugin-pack.mjs`](../tools/plugin-pack.mjs)；离线 SDK：[`../tools/sdk-pack.mjs`](../tools/sdk-pack.mjs)
- 错误码：`E_PROTOCOL`/`E_VERSION`/`E_SCHEMA`/`E_TIMEOUT`/`E_CANCELLED`/`E_QUOTA`/`E_INTERNAL`（协议层）；`E_TRUST_*`（安装层）
