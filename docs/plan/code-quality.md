# 代码质量与合并门禁计划

> 2026-11-22 完成评估，整改进行中。源码盘点基线为 `bfd1035`。

先建立可复现的合并检查，再按改动影响分批拆分。优先理清桌面与 headless 共享的运行逻辑，以及 DSH 宿主和 Sidecar 之间的请求归属。保留现有 CLI、Rust 公开接口、会话日志、RPC 语义和插件行为。若整改需要修改公开契约，先更新对应规范、Schema 和契约用例。 [工程质量门槛](../PLAN.md#102-工程质量门槛) · [规范索引](../spec/README.md)

## 现状基线

统计 Git 已跟踪文件的物理行数，含空行、注释和内联测试，末尾换行不额外计一行。Rust 源统计 workspace 成员的 `src/`，集成测试另计。Node 统计下列第一方目录中的 JavaScript 文件，排除 vendored `node_modules` 和构建产物。

| 范围 | 文件数 | 总行数 | 超过 500 行 | 超过 1,000 行 | 超过 2,000 行 |
| --- | ---: | ---: | ---: | ---: | ---: |
| Workspace Rust `src/` | 106 | 86,384 | 49 | 25 | 5 |
| Workspace Rust `tests/` | 20 | 10,842 | 10 | 3 | 0 |
| Sidecar 实现与探针 | 23 | 4,882 | 2 | 1 | 0 |
| Sidecar 单测 | 2 | 716 | 0 | 0 | 0 |
| `tools/`、`schema/` 下的 Node 脚本 | 13 | 4,152 | 2 | 1 | 0 |

- **已有检查。** Windows 跑 workspace 测试、Sidecar 单测、conformance 和性能门禁。三平台跑契约、核心 crate 测试、桌面构建、headless 集成测试及打包自检。核心 crate 列表由 CI 手写，目前遗漏 workspace 成员 `dshr-compat-acp`。 [Windows conformance](../../.github/workflows/conformance.yml) · [跨平台工作流](../../.github/workflows/cross-platform.yml)
- **缺少的合并检查。** Rust 工具链已固定并包含 rustfmt、Clippy，两条工作流均未调用它们，也没有文件长度或依赖方向检查。官方 JS 单测入口枚举两个固定文件，拆出新测试后需要接入现有入口。GitHub 查询显示默认分支 `master` 未启用保护，当前没有作用于该分支的规则。 [Rust 工具链](../../rust-toolchain.toml) · [Conformance 入口](../../tools/conformance.mjs) · [分支规则](https://api.github.com/repos/oxyflour/dshr/rules/branches/master)
- **本地实测。** 在 `bfd1035` 的源码快照上用 Rust 1.99.1 执行 `cargo fmt --all -- --check`，返回 1，共 33 个文件、362 处格式差异。Node 语法检查通过 38/38 个第一方脚本。Conformance quick 通过 83 个 Schema 用例和 21 个 Node 用例，42 个 conformance ID 取得通过结果，554 个已实现 ID 未在 quick 模式执行。 [Conformance 用法](../conformance.md)
- **Clippy 抽查。** 评估期间在 Windows 当前工作树执行 `cargo clippy --locked --offline --workspace --all-targets`，退出码为 0。按诊断位置与内容去重后有 51 条警告，桌面 crate 占 50 条，`conversation-model` 占 1 条。最常见的是 `result_large_err`，共 37 条。阶段 1 在 CI 的干净检出上重建正式基线。

## 整改优先级与模块职责

| 优先级 | 现状与证据 | 拆分后的职责 |
| --- | --- | --- |
| P0 桌面入口与共享运行时 | `chat.rs` 17,062 行，内联测试前约 15,248 行。在同一文件里，`ChatApp` 管理窗口和会话交互，`ChatState` 编排运行，`ChatExecutor` 路由工具和审批，`ChatSessionAccess` 提供会话访问，还包含约 4,400 行诊断、终端命令及相关逻辑。Headless 直接从桌面入口导入并构造 `ChatState`。 [桌面入口](../../apps/desktop-gpui/src/bin/chat.rs) · [Headless 入口](../../apps/desktop-gpui/src/bin/chat/headless.rs) | 桌面入口装配并分发启动模式。GPUI 模块管理视图、焦点、输入草稿和窗口事件。共享的应用运行模块编排会话运行、工具路由、审批接入和取消，供桌面与 headless 调用。CLI 处理参数及输入输出，诊断按用途独立。会话持久化继续使用 `session-store`，审批继续经过 `tool-runtime`。 |
| P1 DSH 宿主与官方 Sidecar | Rust 的 `compat-dsh-host/src/lib.rs` 有 2,200 行，`SidecarHost` 同时管理进程、握手和注册、批调用与结算、Host 回调校验。JS 的 `sidecar-dsh.mjs` 有 1,967 行，入口持有会话和修订注册表、请求状态、文件适配及执行钩子。 [Rust 宿主](../../crates/compat-dsh-host/src/lib.rs) · [官方 Sidecar](../../sidecar/dsh-official/sidecar-dsh.mjs) | Rust 分出进程与传输、注册与预设、调用与结算、回调校验与路由，保留 `SidecarHost` 公共入口。JS 单独组织文件适配、工具注册和工具执行，入口负责装配与消息分发。调用身份与回调授权由 Rust 宿主统一裁决，Sidecar 管理 Cordis 插件及桥接状态。 |
| P1 ADC 渲染 | `ui-gpui/src/adc.rs` 有 3,147 行，其中内联测试前约 2,286 行。递归分发、布局、交互控件与 Action 编码、图表及可访问性检查都在同一文件。 [ADC 渲染器](../../crates/ui-gpui/src/adc.rs) | 渲染入口统一处理节点分发和树遍历，按布局、交互、数据展示归属控件。共用 Action 编码和 a11y 规则。词汇表、校验及注册生命周期继续交给 `ui-contributions`。 |
| P2 Agent Core、插件生命周期、仓库工具 | `agent-core/src/lib.rs` 有 2,600 行，其中内联测试前约 1,220 行，包含模型与工具接口、预算、重复调用提醒和 Turn Loop。`plugin-host/src/fiber.rs` 有 2,206 行，其中内联测试前约 1,239 行，包含 Fiber 状态、Service 表、Effect 栈和载入/卸载。`tools/conformance.mjs` 有 1,434 行，包含用例选择、执行、输出解析和报告。 [Agent Core](../../crates/agent-core/src/lib.rs) · [Fiber](../../crates/plugin-host/src/fiber.rs) · [Conformance 工具](../../tools/conformance.mjs) | 分离 Agent 接口与 Turn 编排，通过重导出保留公开类型路径。按生命周期、Service 和 Effect 的所有权拆分 Fiber，保留加载、撤销和回滚顺序。按运行与报告分离 conformance 工具，保留命令、退出码和报告格式。这批任务先按风险拆，测试集中带来的体积单独处理。 |

Cargo 元数据表明，桌面应用直接依赖 18 个 workspace 库，只有桌面应用和 `ui-gpui` 直接依赖 GPUI。这条边界已经存在，适合首先纳入自动检查。应用内部拆分先复用同 crate 模块和现有服务接口。对于测试为主的文件，先按场景归档用例，再评估生产代码职责。 [系统所有权](../PLAN.md#21-状态所有权必须唯一)

## 拟定门禁

| 检查 | 首次启用 | 收紧条件 |
| --- | --- | --- |
| Rust 格式 | 用固定工具链提交一次纯格式清理，CI 执行 `cargo fmt --all -- --check`。 | 使用与本地一致的命令。格式提交单独验证，不把改动混入模块迁移。 |
| 文件长度 | 检查 Git 跟踪的第一方 workspace Rust、Sidecar JS、`tools/` 和 `schema/` 的 Node 脚本。实现文件超过 500 行提示职责复核，新建实现文件超过 1,000 行失败。独立测试文件及仅在测试配置下编译的专用测试模块上限 1,500 行。 | 清理格式后为超限存量逐文件记录上限。普通 PR 只能降低上限，降至阈值即移除记录。拆出的新文件按通用上限验收。临时放宽单独审查，记录路径、上限、原因、负责人和到期日，到期即失败。 |
| 依赖方向 | 用 `cargo metadata --locked --no-deps --format-version 1` 检查普通和构建依赖，含各 target 的声明。只有 `desktop-gpui` 和 `ui-gpui` 可直接依赖 GPUI。`compat-dsh-protocol` 和 `ui-contributions` 保持没有 workspace 普通或构建依赖。`plugin-api` 可使用 `tool-runtime`，禁止依赖宿主及 WASM 实现。测试依赖单独列入报告。 | 桌面运行模块抽出后补检查，禁止它引用 GPUI 类型或视图模块。Sidecar 拆出模块后补检查，禁止实现模块反向导入进程入口。 |
| Rust lint | Windows 先执行 `cargo clippy --locked --workspace --all-targets`，记录第一方诊断。有编译错误或 Clippy 错误即失败，历史警告先报告。对已清零的 crate 单独启用 `-D warnings`。 | 优先清理影响正确性和资源生命周期的警告，扩大严格检查范围。在各平台 runner 上验证对应条件编译的目标，完成后启用 workspace 严格检查。 |
| Node 与契约测试 | 使用 CI 现有的 Node 22 对第一方 JS 跑 `node --check`，并复用现有 Sidecar 单测、Schema、文档链接和 conformance 检查。 | 把 Sidecar 单测发现统一到一个入口，保证新增测试被普通 CI 和 conformance 执行。外部官方包用例通过专项脚本记录版本、平台和执行数。 |

第一阶段给 PR 提供一个固定名为 `code quality` 的检查，在 PR 上始终产出结果。等正常 PR 通过、故意制造的违规能阻断，且 push 与 PR 的差异基准验证完成，再由仓库管理员给 `master` 配置必过检查。保留现有 conformance、跨平台和打包流水线。记录正常 PR 和失败 PR，用来验证合并确实受到约束。

## 实施顺序与完成条件

| 阶段 | 工作与依赖 | 完成条件 |
| --- | --- | --- |
| 1. 建基线并启用门禁 | 先做纯格式提交，再加 Node 检查入口、文件上限、Cargo 依赖断言及 `code quality` CI。记录 Clippy 基线，补齐跨平台核心测试遗漏的成员。 | 本地和 CI 输出一致。试验 PR 分别引入格式偏差、新超长文件、存量超长文件增长、错误依赖及 JS 语法错误，均可定位文件和规则并失败。验证删除、重命名、新增测试和纯文档变更。记录耗时并确认必过规则生效。 |
| 2. 拆桌面入口与共享运行时 | 依赖阶段 1。先迁出诊断和 CLI 分发逻辑，再按现有调用路径拆出会话运行、工具执行和会话访问。最后梳理 `ChatApp` 中的视图状态与投影，复用已有 `chat/` 子模块。各批保留可运行的 `dshr`。 | `chat.rs` 降至 1,000 行以内。Headless 通过独立的共享运行入口，运行模块与 GPUI 解耦。桌面和 headless 的会话续跑、写入审批、取消、日志恢复、退出码和标准输出有回归证据。 |
| 3. 拆 DSH 宿主与 Sidecar | 依赖阶段 1。与阶段 2 的应用运行时修改错开合并。先拆 Rust 进程传输和注册表，再拆 JS 文件适配与调用执行，最后收拢结算及回调授权。 | Rust、JS 两个入口各降至 1,000 行以内。现有批调用结果顺序、超时隔离、取消和迟到结果、generation 切换、预设绑定、工作区及会话授权全部复验。官方 `rc.1`、`rc.5` 分别按来源锁运行专项用例。 |
| 4. 拆 ADC 与其余热点 | 依赖阶段 1。桌面动作接线在阶段 2 后复验。先拆 ADC 布局、控件和 Action 编码，再处理 Agent Core、Fiber 及 conformance 脚本。 | ADC 渲染入口降至 1,000 行以内。ADC 点击、键盘、校验和性能用例通过。Agent Turn 事件、Fiber 清理顺序和 conformance 行为复验。其他超限条目逐项记录减少量或留存理由，按 crate 推进严格 lint。 |

每个重构 PR 记录变更前后的职责、依赖方向、热点行数与验证结果。应用运行时的会话与工具链路、Sidecar 回调、插件卸载和 GPUI 交互分别验收。已有用例能证明行为时沿用其 conformance ID，只有缺少关键断言时才补用例。 [注册表](../conformance.d/registry.json)

## 验收命令

将新增源码加入 Git 索引后执行质量检查。默认以 `HEAD` 比较文件上限，可用 `--base <git-ref>` 指定 PR 基准。测试专用 Rust 模块在文件首行声明 `#![cfg(test)]`。Windows 的 Cargo 构建、测试和 Clippy 使用 `tools\build-env.cmd`，Linux 和 macOS 直接调用 Cargo。 [质量工作流](../../.github/workflows/code-quality.yml) · [文件长度基线](../../tools/code-quality-baseline.json)

```powershell
node tools/check-code-quality.mjs
node --test tools/check-code-quality.test.mjs
cargo fmt --all -- --check
tools\build-env.cmd cargo clippy --locked --workspace --all-targets
node tools/conformance.mjs --quick --compact
node tools/check-links.mjs
tools\build-env.cmd cargo test --locked --workspace
```

| 改动范围 | 补充验收 |
| --- | --- |
| 桌面运行时、CLI | `tools\build-env.cmd cargo test --locked -p dshr-desktop-gpui --bin dshr --test headless_cli`，并运行 `dshr --selftest-approval`、`--selftest-cancel`、`--selftest-crash-recovery` 及现有 Windows 键盘 smoke。 [桌面 CI 命令](../../.github/workflows/cross-platform.yml) |
| DSH 宿主、Sidecar | `tools\build-env.cmd cargo test --locked -p dshr-compat-dsh-host --test l1_roundtrip --test parallel_batch --test l2_host_services`，`dshr --selftest-sidecar-recovery`，以及准备依赖后运行 `tools/official-l2-gate.ps1` 和 `tools/official-rc5-gate.ps1`。 [官方包复验方法](dsh-official-plugin-compatibility.md#验收命令) |
| ADC、Agent、Fiber | `tools\build-env.cmd cargo test --locked -p dshr-ui-gpui -p dshr-ui-contributions -p dshr-agent-core -p dshr-plugin-host`，`dshr --selftest-adc-panel`，由现有 CI 的 G4 复验渲染性能。 [性能门禁](../m4-perf.md) |

## 进度

- 2026-11-22 完成源文件盘点、关键模块职责与依赖检查，核对本地格式、Node 和 quick conformance 基线。
- 2026-11-22 提交独立格式清理（`f3e8628`），Rust 格式检查通过。加上本地质量入口、`code quality` 工作流、Sidecar 测试发现和跨平台 `dshr-compat-acp` 测试。临时仓库通过三组用例，覆盖格式、长度、依赖、JS 语法、限时例外、重命名和删除。GitHub PR 与 `master` 必过规则尚待实跑验证。
- 2026-11-22 独立构建目录的 workspace 测试找出 RPC 类型数量断言过时。保留未知类型拒绝测试，改为直接比较协议枚举和 Schema，补齐跨平台 `dshr-compat-dsh-fs` 和 `dshr-sidecar-guard`。两个契约 crate 通过严格 Clippy。
- 2026-11-22 迁出桌面 CLI 分发和诊断，入口从格式化后的 18,431 行降至 14,090 行。协议、桌面和 headless 三个目标共 69 个测试通过。审批、取消、崩溃恢复、Sidecar 恢复及 CPP 组件自检全部通过。共享会话运行、工具执行和 GPUI 视图待拆分。
