# DSH 插件兼容映射

dshr 通过 Node sidecar 复用部分 DSH 工具包。兼容性取决于包版本、配置、所需服务和实际调用路径。
dshr 自有插件使用 [CPP-v1](cpp-v1.md)，界面扩展使用 [ADC-v1](adc-v1.md)。

新会话工作区旁的「创造模式」选择可覆盖的 `cordis` 预设，支持静态子插件组合、修订和后台配置热重载。dshr 提供 ADC-v1 预览与 CPP-v1 试用工具。
`creator_inspect` 提供 dshr 的 CPP-v1 清单和可构建的交互面板示例。`creator_build_plugin` 通过 Cargo 构建 Rust 外部进程包，并为构建提供同版本 SDK。`creator_try_plugin` 校验声明的工作区面板并支持在会话内重新试用同一包。DSH Desktop `0.2.2-rc.5` 在最近一次符合条件的 `cordis_run` 卡片内挂载 `tool.view.cordis`。dshr 的 CPP-v1 试用面板挂载在对应插件最近一次成功的 `creator_try_plugin` 卡片内。Windows x64 可按需安装随发行配套的 Cargo、链接器和离线 SDK 依赖。官方 `cordisInspect` 已验证 Host 工具 Schema 检查，并提供协商服务元数据查询。原版完整的创造工具组合、Cordis Client 插槽、动态运行器和插件管理仍需适配。 [Agent 预设契约](agent-presets.md) · [dshr 创造模式契约](creator-mode.md) · [对话窗口验收](../plan/creator-mode.md#对话窗口显示复验)
具名 headless 的 DSH 清单和补丁规则见 [Headless CLI 规范](headless-profile.md#7-dsh-profile-声明兼容)。

## 兼容矩阵

矩阵按版本记录运行结果和源码证据。**已验证执行**表示所列操作返回预期结果或完成相应的文件、会话或存储变更。
**仅验证注册**表示观察到了对应的 `tool.register`。**部分可用**表示包内有已验证操作和已知缺口。
**静态声明兼容**表示 dshr 识别相应声明并实现所列行为。**需要适配**表示当前宿主缺少源码所需的服务、执行语义或装载入口。证据栏注明实际验证方式。

所有行均使用 `@deepseek-ai/` 包名前缀。桌面默认启动官方 DSH sidecar，并按已启用的 profile 条目注册工具。
无打包默认配置时，基础 profile 包含 `dsh-tool-workspace-dependencies` 和 `dsh-tool-fs`。Windows x64 调试包为首次启动的桌面 profile 提供 27 个 Windows 已验收的 `rc.5` 工具包及 4 个可配置的配套插件。打包可从官方安装档抽取依赖，也可使用 npm 锁定的官方包目录。两种来源都须通过同一份版本和文件散列校验。Windows 校验也接受来源锁明确列出的 npm 清单散列。`dsh-subprocess-local` 的 npm 清单额外声明 `postinstall`，安装时禁用脚本，入口 JS 使用同一散列。
GitHub 跨平台工作流的 `desktop-package-windows-latest` 必须带上 `rc.5` 依赖、默认 profile 和 PowerShell 7。归档验证在缺少默认 profile、依赖缺失、散列不符或 PowerShell 无法启动时失败。`desktop-windows-latest` 提供供调试使用的单个可执行文件。`-BaseSidecar` 用于显式构建基础调试包。已有 `profile/plugins.json` 或 Loader 配置优先。
调试包的默认配置使用前台 PowerShell 和 Native 执行器。持久 PowerShell 和 PTC 在列表中可选，启用任一套实现时关闭对应的互斥项。前台 PowerShell、后台任务和终端优先使用调试包内的 PowerShell 7。未提供包内 PowerShell 时，宿主沿用 `PATH` 查找。网页工具依赖可用凭据，Office 工具通过 Office Skill 激活。 [Windows 打包说明](../m4-package.md#1-命令)
Windows 上直接启动 Cargo 调试版时，可先将调试包资源暂存到可执行文件旁的 `dshr-debug-assets`。暂存脚本校验官方依赖来源，复制调试包资源，并把模板缺项补进 `target/debug/profile/plugins.json`，保留已有条目与开关。开发版使用这个独立 profile，会话和其他数据继续使用原数据根。`DSHR_APP_ROOT`、`DSHR_HOME`、`DSHR_PROFILE_DIR` 和 Loader 仍可显式覆盖对应配置。未暂存时，Cargo 调试版沿用基础资源与 profile。验收时直接打开 `target/debug/dshr.exe`，插件页应显示暂存的工具，且可启停和调用。 [暂存命令](../m4-package.md#直接启动-cargo-调试版)
其他包需先准备 Sidecar 可解析的包及依赖，再在 `plugins.json` 或 Loader 派生的 profile 中显式配置。
[基础插件清单](../../crates/profile/src/lib.rs#L256) · [Windows 调试包插件清单](../../sidecar/dsh-official/windows-debug-profile.json) · [Loader 支持范围](../m3-loader.md)

### 0.2.2-rc.5

2026-11-22 清点本机 Windows 官方安装包 `app.asar` 内 `dsh/node_modules/@deepseek-ai/dsh-tool-*` 的 29 个包。29 个包均有执行证据，其中 27 个经过 Windows 专项验收，前台和持久 Bash 经过 Linux 宿主验收。部分可用的工具包在各行标明尚待适配的能力。 [官方工具目录](https://github.com/deepseek-ai/deepseek-harness/blob/dsh-v0.2.2-rc.5/docs/tool-catalog.md) · [分阶段实施计划](../plan/dsh-official-plugin-compatibility.md)

| 包 | 状态 | 操作、依赖和限制 | 证据 |
|---|---|---|---|
| `dsh-tool-workspace-dependencies` | 已验证执行 | `load_workspace_dependencies` 返回 Python 路径。用本机 DSH 的 `primary-runtime` 作为 `config.source` | [自动验收](#rc5-自动验收) |
| `dsh-tool-fs` | 已验证执行 | `read`、`write`、`edit`、`apply_patch` 和 `read_image` 通过。相对路径跟随会话目录。写入需协商 `fs.write` 并通过宿主策略。读图需配置 `readImage: true`、协商图片附件服务，并使用支持图片输入的模型 | [自动验收](#rc5-自动验收) · [文件与图片契约](sidecar-rpc.md#75-文件写入回调fswrite) |
| `dsh-tool-todo` | 已验证执行 | `todo_write` 写入 `todo/write` 会话事件。显式配置 `allowParallelInProgress` 布尔值并协商 v2 `service.proxy` | [自动验收](#rc5-自动验收) |
| `dsh-tool-working-directory` | 已验证执行 | 全局和预设中的 `working_directory` 可切换会话目录。两个会话分别解析同名文件，重启后恢复保存的目录。显式加入插件并协商 v2 `service.proxy` | [自动验收](#rc5-自动验收) |
| `dsh-tool-str-replace-editor` | 已验证执行 | `view` 支持普通文件和目录。`create`、`str_replace`、`insert` 经批准后写入，拒绝时文件内容保持原样。该包接收绝对路径 | [自动验收](#rc5-自动验收) |
| `dsh-tool-agent-team` | 已验证执行 | 团队任务板范围固定于当前会话树。已验证依赖、认领、循环与过期版本拒绝、完成和重启。通过 profile 显式启用 | [阶段 5 报告](../plan/dsh-official-plugin-compatibility-stage-5.md) |
| `dsh-tool-ask-user` | 已验证执行 | Windows 桌面运行时验证前台答复、跳过和取消，headless 返回非交互错误。已接入桌面应答界面，图形交互待复验。`timed` 模式的迟到答复投递待适配 | [自动验收](#rc5-自动验收) · [提问契约](sidecar-rpc.md#79-官方工具提问) |
| `dsh-tool-bash` | 已验证执行 | Linux 宿主验证前台目录、退出码、超时和取消，以及显式后台启动和输出领取。Linux 图形桌面与 macOS 待实测 | [阶段 2 报告](../plan/dsh-official-plugin-compatibility-stage-2.md) · [阶段 4 报告](../plan/dsh-official-plugin-compatibility-stage-4.md) |
| `dsh-tool-bash-persistent` | 已验证执行 | Linux 宿主使用实际 PTY 验证同一会话跨调用保留变量、跨会话拒绝和关闭。需要新版 Node 和平台 PTY 附加组件 | [阶段 4 报告](../plan/dsh-official-plugin-compatibility-stage-4.md) |
| `dsh-tool-call-timeout-policy` | 已验证执行 | 官方 `tools/execute` 钩子在实际执行中触发声明的协作超时，Rust 保留宿主预算和取消 | [阶段 6 报告](../plan/dsh-official-plugin-compatibility-stage-6.md) |
| `dsh-tool-cordis` | 部分可用 | 列举本机 Host provider，验证当前调用范围的工具 Schema 查询及非法输入拒绝。Host 协商服务元数据可查看，Client provider 和原版动态运行器待适配 | [阶段 6 报告](../plan/dsh-official-plugin-compatibility-stage-6.md) |
| `dsh-tool-executor-native` | 已验证执行 | 装载官方 Native 执行器，复用 Rust 步骤注册和调用授权。验证工具结果和官方超时钩子 | [阶段 6 报告](../plan/dsh-official-plugin-compatibility-stage-6.md) |
| `dsh-tool-executor-ptc` | 部分可用 | Windows 官方 Node PTC 执行 TypeScript 单次程序，验证 Rust 子调用授权、文件读写、拒绝和预设撤销。持久程序环境及 Python 待适配 | [阶段 6 报告](../plan/dsh-official-plugin-compatibility-stage-6.md) |
| `dsh-tool-fs-search` | 已验证执行 | Windows headless 的 `glob`、`grep` 跟随会话目录，通过 Rust 宿主调用 ripgrep。跨出工作区的搜索路径被拒绝 | [自动验收](#rc5-自动验收) |
| `dsh-tool-goal` | 已验证执行 | 验证直接用户创建、绑定目标的自动轮次、版本比较、完成及重启后解除激活。暂停后的显式用户恢复入口待适配 | [阶段 5 报告](../plan/dsh-official-plugin-compatibility-stage-5.md) |
| `dsh-tool-jobs` | 已验证执行 | Windows 验证启动、领取、停止、恢复和同一会话的一次完成提示。Linux 验证 Bash 后台启动和领取。输出在进程结算后领取 | [阶段 4 报告](../plan/dsh-official-plugin-compatibility-stage-4.md) |
| `dsh-tool-office` | 已验证执行 | Windows 原生 OfficeKit 验证 PDF 转换、表格重算、双页 PNG 渲染与会话重放。由 Skills 激活，需本地 OfficeKit 引擎。转换和重算遵守桌面审批，渲染需要支持图片输入的模型 | [Office 契约](sidecar-rpc.md#713-官方-office-工具) · [自动验收](#rc5-自动验收) |
| `dsh-tool-present` | 已验证执行 | 按会话目录交付普通文件。Rust 核对工作区路径和轮次，记录可重放的交付引用。桌面卡片提供打开操作，图形交互待复验 | [文件交付契约](sidecar-rpc.md#710-官方工具文件交付) · [自动验收](#rc5-自动验收) |
| `dsh-tool-pwsh` | 已验证执行 | Windows 运行时验证前台执行、输出、退出码、会话目录、审批拒绝、超时和取消。配合 `jobs` 支持显式后台运行。需 PowerShell 7 | [阶段 2 报告](../plan/dsh-official-plugin-compatibility-stage-2.md) · [阶段 4 报告](../plan/dsh-official-plugin-compatibility-stage-4.md) |
| `dsh-tool-pwsh-persistent` | 已验证执行 | Windows 原生 PTY 验证跨调用保留状态、取消和重启后的旧终端状态。需 PowerShell 7 和平台 PTY 附加组件 | [阶段 4 报告](../plan/dsh-official-plugin-compatibility-stage-4.md) |
| `dsh-tool-ralph` | 已验证执行 | 前台运行，验证两个子会话及结构化交接，使用当前模型路由和官方 Node PTC 引擎 | [阶段 5 报告](../plan/dsh-official-plugin-compatibility-stage-5.md) |
| `dsh-tool-schedule` | 部分可用 | `this_session` 的 RRULE 任务持久化，下一次模型步领取到期提醒并去重。验证查看、暂停和过期更新拒绝。待适配桌面空闲自动唤起及新会话目标 | [阶段 5 报告](../plan/dsh-official-plugin-compatibility-stage-5.md) |
| `dsh-tool-session-query` | 部分可用 | 五个工具在同工作区的当前会话树内通过。使用有上限的字面匹配和可公开事件投影。DSH 全文索引排序与替换链追踪待适配 | [阶段 5 报告](../plan/dsh-official-plugin-compatibility-stage-5.md) |
| `dsh-tool-skill` | 已验证执行 | 配合官方文件系统 provider，技能目录随会话工作目录变化。显式 `/技能名` 激活和 `skill` 调用均有模型请求证据 | [Agent 步骤契约](sidecar-rpc.md#711-官方-agent-步骤上下文) · [自动验收](#rc5-自动验收) |
| `dsh-tool-subagent` | 部分可用 | `local` provider 在同一工作区启动独立子会话，记录完成和失败。继承当前模型和权限。后台子会话可用 `working_directory` 读取会话目录，需交互审批的 Shell 调用由父会话执行。模型切换、继承上下文及独立子预设待适配 | [阶段 5 报告](../plan/dsh-official-plugin-compatibility-stage-5.md) |
| `dsh-tool-subagent-control` | 部分可用 | 验证列举、等待、给空闲子 Agent 发消息及越权拒绝。已接入中断，单独工具中断待补专项用例。运行中的子 Agent 将追加消息排到下一轮 | [阶段 5 报告](../plan/dsh-official-plugin-compatibility-stage-5.md) |
| `dsh-tool-terminal` | 已验证执行 | Windows 验证打开、发送、读取、关闭、隔离与重启回放。Linux 宿主验证真实 Bash PTY 列表、隔离和关闭。后台发送待适配 | [阶段 4 报告](../plan/dsh-official-plugin-compatibility-stage-4.md) |
| `dsh-tool-web` | 已验证执行 | 通过官方 DeepSeek Web API provider 和本地测试端点验证搜索、抓取、结果、提示和超时。缺凭据时隐藏相应工具。生产账号、服务端点及直接抓取 provider 仍需发行复验 | [网页契约](sidecar-rpc.md#712-官方网页工具) · [自动验收](#rc5-自动验收) |
| `dsh-tool-workflow` | 部分可用 | 验证前台脚本、子会话调用、结果和取消。需要官方 Node PTC 和本机进程沙箱。后台执行与子模型切换待适配 | [阶段 5 报告](../plan/dsh-official-plugin-compatibility-stage-5.md) |

客户端包 `dsh-client-ui-cordis` `0.2.2-rc.5` **需要适配**。原版在最新有效的 `cordis_run` 卡片内挂载交互视图。直接运行该客户端包需要 Cordis Client 槽位、动态运行器和相关宿主服务。dshr 的原生 CPP-v1 试用面板已有独立验收。 [参考源码与对话窗口验收](../plan/creator-mode.md#对话窗口显示复验)

官方另有 16 个随包可选 bundle 和 2 个按需 bundle。这里的工具包结果只覆盖相应单包和列出的操作。bundle 的启停、组合与配置界面待验收。 [官方 bundle 清单](https://github.com/deepseek-ai/deepseek-harness/blob/dsh-v0.2.2-rc.5/packages/util/official-bundles/src/bundles.ts)

### 0.2.2-rc.1

| 包 | 版本 | 状态 | 已验证的操作与边界 | 启用条件 | 测试证据 |
|---|---|---|---|---|---|
| `dsh-tool-workspace-dependencies` | `0.2.2-rc.1` | 已验证执行 | `load_workspace_dependencies` 返回 DSH 运行时路径。干净 Windows 来宾安装组件后，从工具返回的路径运行了 CPython `3.13.12` 并导入标准库 | 默认条目。需要可用的 DSH `primary-runtime`，来源为外部 DSH 运行时或应用安装的 Python 组件。组件仅含解释器和标准库 | [只读自检记录](../m2-dsh-official.md#证据) · [干净来宾验收](../plan/runtime-components.md#干净来宾验收结果) |
| `dsh-tool-fs` | `0.2.2-rc.1` | 部分可用 | `read`、`write`、`edit`、`apply_patch` 已验证。干净 Windows 来宾在仅有基础包时复验了 `read`。写入目标的父目录必须已存在。`read_image` 依赖尚未接入的附件服务 | 默认条目补充注入 `tools`、`fs`、`systemPrompt`。写操作需启用对应开关、协商 `fs.write` 并通过宿主审批 | [`RPC-L2-506/507`，读取和写入](../../crates/compat-dsh-host/tests/official_packages.rs) · [编辑、补丁、审批拒绝与缺失父目录](../../crates/compat-dsh-host/tests/fs_write_e2e.rs) · [干净来宾验收](../plan/runtime-components.md#干净来宾验收结果) |
| `dsh-tool-todo` | `0.2.2-rc.1` | 已验证执行 | `todo_write` 写入 `todo/write` 会话事件 | 显式配置 `allowParallelInProgress` 布尔值，协商 v2 `service.proxy`，使用会话日志与投影服务 | [`RPC-L2-201`](../../crates/compat-dsh-host/tests/official_packages.rs) |
| `dsh-tool-working-directory` | `0.2.2-rc.1` | 已验证执行 | 全局和预设内的 `working_directory` 均可切换会话目录。工作目录服务的 `get`、`ensure`、`set` 已验证往返。官方 `fs` 工具仍以 dshr 工作区根解析相对路径 | 显式加入全局 profile 或预设的 `config.plugins`，协商 v2 `service.proxy` | [`RPC-L2-501/502/503`](../../crates/compat-dsh-host/tests/official_packages.rs) · [启用示例](../m3-loader.md#声明创造预设) |
| `dsh-tool-pwsh` | `0.2.2-rc.1` | 需要适配 | 源码声明注入 `shell`、`shellEnv` 等服务。工具运行尚未验证 | 需要接入官方 Shell、环境、审批和隔离服务 | [依赖调查](../m3-l2-services.md) |
| `dsh-agent-preset`、`dsh-agent-preset-registry` | `0.2.2-rc.1` | 静态声明兼容 | dshr 解析预设 ID、默认值和子插件列表。后台发布 bundle、patch 与 include 的变更，保留会话和子会话已绑定的旧修订 | 官方 Sidecar 与 `DSHR_LOADER_BUNDLE`。由 dshr 实现注册表 | [`M3-PRESET-001..005`](agent-presets.md#验收) |
| `dsh-tool-cordis` | `0.2.2-rc.1` | 需要适配 | 官方只读检查工具依赖 `cordisInspect` Host provider 与 Agent 执行上下文 | 需要接入官方检查服务 | [依赖调查](../m3-l2-services.md) |
| `dsh-storage-domain` | `0.2.2-rc.1` | 已验证执行 | 域经宿主后端完成打开、KV 读写、删除和关闭，重复打开返回 `already-open` | 显式加入插件，令 `config.backend` 为 `storage-kv` 并协商 v2 `service.proxy` | [`RPC-L2-504`](../../crates/compat-dsh-host/tests/official_packages.rs) |
| `dsh-storage` | `0.2.2-rc.1` | 需要适配 | 包导出 Cordis Service class，现有加载入口接收 `apply` 插件。当前的 `ctx.storage` 由 sidecar 代理提供 | 直接加载该 Service class 需要扩展宿主启动逻辑 | [`RPC-L2-505`](../../crates/compat-dsh-host/tests/official_packages.rs) |

核对插件依赖时同时检查 `inject`、`apply` 和 `execute`，包括可选注入、执行上下文与事件钩子。 [rc.1 官方工具依赖调查](../m3-l2-services.md)

## 启用条件与适配边界

### 官方归档与用户安装目录

Windows x64 `rc.5` 调试包从只读 `sidecar/dsh-official/official.zip` 加载官方 Sidecar 和支持归档读取的依赖。需要真实路径的原生模块、外部程序和子进程依赖随包放在 `sidecar/dsh-official/native/node_modules`，启动时直接使用。归档缺失、损坏或 Node 缺少相应 VFS 支持时，已选择归档布局的 Sidecar 启动失败。源码开发、基础包和旧发行包可以继续使用普通 `node_modules`。Windows `rc.5` 打包验证须从归档与原生目录读取并校验锁定的官方包文件。

桌面与 headless 的安装、更新与卸载使用 `<profile 目录>/node_modules`。显式设置 `DSHR_SIDECAR_DIR` 时改用 `<指定目录>/node_modules`。安装仍接受本地目录或 `.tgz`，沿用插件信任链。停用通过 profile 完成。资源根下保留的旧 `sidecar/dsh-official/node_modules` 中的用户插件仍可读取，重新安装同名插件后优先使用用户目录中的版本。若更换了资源根，可从旧安装包重新安装插件，或显式设置 `DSHR_SIDECAR_DIR`。卸载须移除 profile 选择，并且只删除可写安装目录中的包。

Sidecar 的顶层插件入口先解析发行附带的包，再解析 profile 用户目录或旧安装目录中的用户插件。加载用户插件的依赖时，Cordis 使用宿主的同一模块实例，其余依赖沿用 Node 包解析，缺少的依赖可从发行侧依赖解析。旧安装目录和用户目录均不得覆盖随包官方入口。验证须覆盖官方工具调用、用户插件与 Cordis 共享实例、旧布局回退、安装与卸载不写归档，以及真实路径执行的搜索、终端、Office 和 PTC。 [插件信任链](plugin-trust.md) · [归档实施计划](../plan/dsh-sidecar-archive.md)

基础包带 Node 与 Sidecar。Python 与创造模式构建能力分别由可选组件提供。[运行组件契约](runtime-components.md)

- **桌面启动。** `DSHR_SIDECAR` 未设置或为空时，桌面使用随应用提供的官方 DSH sidecar。`probe` 与 `cordis` Sidecar 仅供显式诊断命令使用。产品运行时保留 `probe_*` 前缀给诊断工具，
  模型工具清单和派发均排除这些名称。桌面产品归档只随附官方 DSH sidecar。官方 sidecar 缺少依赖或启动失败时，桌面显示加载错误并继续提供已加载的原生工具。
  headless 的启动失败条件见[运行配置与工具](headless-profile.md#3-运行配置与工具)。桌面和 CLI 的工具隔离有 [`CPP-CREATOR-009`](creator-mode.md#兼容性与验收) 回归覆盖。
- **配置与装载。** sidecar 导入条目中的 `name`，解析插件的 `Config`，将 `apply` 交给 Cordis。
  在 `plugins.json` 的 `plugins` 数组中按 `name`、`enabled`、`config` 和可选的 `inject` 添加条目。
  桌面从 profile 生成 `DSHR_DSH_PLUGINS`，直接运行 sidecar 时可通过该环境变量传入已启用的条目数组。
  仓库的默认 Sidecar 依赖仍声明 `0.2.2-rc.1`。`tools/dsh-vendor.mjs` 默认抽取本机安装版中的工作区依赖工具、文件工具及其依赖。可以用 `DSHR_ASAR`
  选择其他安装包，用脚本参数指定抽取目录和所需工具包。
  [Sidecar 依赖清单](../../sidecar/dsh-official/package.json) · [Sidecar 装载代码](../../sidecar/dsh-official/sidecar-dsh.mjs#L724) · [抽取脚本](../../tools/dsh-vendor.mjs)
- **文件与会话服务。** `fs.write` 协商成功后，写请求回到 Rust 做工作区检查、审批、写前快照和写入。
  v2 `service.proxy` 另行协商，提供工作目录、会话日志、会话投影和 KV 存储的适配方法。
  桌面端可用 `DSHR_FS_WRITE=0` 关闭写能力，用 `DSHR_HOST_SERVICES=0` 关闭服务代理。
  `exec.agent` 的最小会话接口供 `working_directory`、`todo_write`、`read_image`、`glob`、`grep`、`pwsh` 和 `bash` 使用。`systemPrompt` 适配器收集段落，
  尚未接入 Rust 的提示词组装。[宿主能力开关](../../apps/desktop-gpui/src/bin/chat.rs#L1401) · [v2 服务方法表](sidecar-rpc.md#132-service_key-与方法表)
- **工具作用域和身份。** 已验证的官方包按工作区登记全局工具。桌面也支持 Sidecar 以显式 `session_id` 登记工具，以及
  创造会话中的原生试用工具。会话定义优先于同名全局定义。切换会话保留试用实例，返回工作区时复用仍在运行的
  Sidecar。每次模型请求固定可用的注册，撤销后旧身份无法调用同名替代工具。
  [Sidecar 契约](sidecar-rpc.md#注册作用域和调用身份) · [作用域与撤销测试](../../crates/compat-dsh-host/tests/official_packages.rs)
- **执行和权限。** Sidecar 调用 `definition.execute` 并传入可取消的 `signal`。DSH 执行钩子与
  `isConcurrencySafe` 调度语义尚未接入。
  `restrict`、`guard`、`presentAs` 是空实现。依赖这些能力实施访问控制的插件需要适配宿主策略。
  [工具注册与调用代码](../../sidecar/dsh-official/sidecar-dsh.mjs#L157)
- **界面。** DSH 客户端挂载点和自定义 UI 需用 ADC-v1 重建。[ADC-v1 规范](adc-v1.md)

### 工具字段映射

| DSH 字段或回调 | 当前 sidecar 行为 |
|---|---|
| `name`、`description`、`parameters`、`output.schema`、`timeoutMs` | 映射到 `tool.register` 的 `name`、`description`、`input_schema`、`output_schema`、`timeout_ms` |
| `execute`、`output.render`、`output.presentationMeta` | 结果值进入 `value`，输出回调的结果分别进入 `content` 与 `meta`。没有可用的内容数组时回退到 JSON 内容块 |
| `presentCall`、`presentResult` | 调用和结果视图回调尚未桥接。官方路径以 `generic` 注册工具 |
| `deferLoading`、`isConcurrencySafe` | 上游工具的懒加载和并发判定尚未接入 |
| `projectContent`、`finalizeContent`、`additionalContexts`、`concludesTurn` | 内容投影与收尾回调、向对话追加上下文和结束轮次尚未桥接 |

[注册字段转换](../../sidecar/dsh-official/sidecar-dsh.mjs#L157) · [调用与结果转换](../../sidecar/dsh-official/sidecar-dsh.mjs#L791)

DSH 自身的 Host/Client Remote RPC 和会话格式各有独立契约。dshr 的 Rust 与 Node 之间使用 stdio 上的
NDJSON，Rust 保存自己的会话事件，通过 v2 服务向插件公开所需的会话操作。
[Sidecar RPC](sidecar-rpc.md) · [dshr 会话日志](session-log.md)

## 验证与复现

### rc.1 集成测试

用测试环境中实际加载的 `package.json` 核对包版本。仓库的来源锁记录 todo、工作目录和 storage-domain
三个官方包的版本及摘要。当前 `runtime.ready.dsh_version` 是固定值，核对抽取版本时以包清单为准。
[官方包来源锁](../../sidecar/dsh-official/official-packages.lock.json) · [握手实现](../../sidecar/dsh-official/sidecar-dsh.mjs#L976)

准备 `0.2.2-rc.1` 的抽取物后，可在 Windows 运行下列命令。准备待办和工作目录测试包的方法见 [官方包测试准备](../m3-l2-services.md#9-复现命令)。

```powershell
powershell -NoProfile -ExecutionPolicy Bypass -File tools\official-l2-gate.ps1
tools\build-env.cmd cargo test --offline -p dshr-compat-dsh-host --test fs_write_e2e -- --nocapture
```

官方包 gate 覆盖 `RPC-L2-201`、`RPC-L2-501..507`、`RPC-REG-001` 和 `M3-PRESET-002/003`。普通 `cargo test` 中这些项目标为 `ignored` 时，
表示本次未执行。文件写入测试还需确认实际输出含注册结果且无 `SKIP`。`dshr --selftest-dsh` 验证默认桌面
运行时注册工作区依赖工具和 `read`，并验证读取测试文件和会话回放。打包用的 `dshr --selftest-official` 使用空插件列表验证
sidecar 与 v2 握手。[Conformance 的人工验收口径](../conformance.md#7-与-m3-exit-gate-的对应) · [打包自检实现](../../apps/desktop-gpui/src/bin/chat.rs#L13523)

### rc.5 本地冒烟

2026-11-21 在 Windows 从本机 DSH `0.2.2-rc.5` 安装包抽取矩阵中有执行证据的五个工具包到隔离目录，按各包的
`package.json` 确认版本。首轮使用 dshr `b9ccba4` 的 Sidecar，随后用启用作用域和注册身份的代码复验。
两次都协商 v2 `service.proxy`，未开启 `fs.write`。复验按宿主确认的 `registration_id` 发起调用。

| 检查 | 观察结果 |
|---|---|
| `tool.register` | `load_workspace_dependencies`、`read`、`todo_write`、`working_directory`、`str_replace_editor` |
| 调用 `load_workspace_dependencies` | 返回 Python 路径 |
| 调用 `read`、`str_replace_editor(view)` | 均返回普通文件的内容 |
| 对目录调用 `str_replace_editor(view)` | `ctx.fs.listDir is not a function` |
| 向 `read` 传入不存在的 `registration_id` | `E_TOOL_UNAVAILABLE` |

复验时使用 `node tools/dsh-vendor.mjs <隔离目录>/node_modules <包名...>` 抽取依赖，把
`sidecar-dsh.mjs` 和 `lib/` 复制到隔离目录。在直接启动的 sidecar 进程中设置 `DSHR_DSH_PLUGINS`
与 `DSHR_WORKSPACE`，按矩阵配置插件。工作区依赖工具的 `config.source` 指向 DSH 的 `primary-runtime` 绝对路径。
发送带 `service.proxy` 的 v2 `runtime.hello`，逐条确认 `tool.register` 并分配 ID。收到启动日志后，
使用对应的 `plugin_id`、`registration_id`、`session_id` 和 generation 发出 `tool.invoke`。
[抽取脚本](../../tools/dsh-vendor.mjs) · [协议消息格式](sidecar-rpc.md#4-envelope)

### rc.5 执行复验

2026-11-22 在文件语义适配前，使用 Windows 构建产物和隔离的 `rc.5` 依赖复验。五个抽取包的 `package.json`、`lib/index.js` 经 SHA-256 比对与本机安装包中的文件一致。

| 入口 | 检查 | 观察结果 |
|---|---|---|
| 桌面 `--selftest-dsh-write` | `write`、`edit`、`apply_patch` | 全部按预期写盘。越界写被拒绝，审批拒绝后文件内容保持原样，生成写前快照 |
| Headless，使用本地模型测试服务 | `working_directory` | 切换到测试工作区子目录。会话日志记录 `working-directory/change` 和目标绝对路径 |
| Headless，使用本地模型测试服务 | 切换目录后调用 `read` | 工作区根与子目录各有一个同名文件，相对路径 `read` 返回工作区根的文件内容 |
| Headless，使用本地模型测试服务 | `todo_write` | 调用成功，日志记录 `todo/write` 及提交的待办内容和状态 |
| Headless，使用本地模型测试服务 | 对目录调用 `str_replace_editor(view)` | 调用返回工具错误 `ctx.fs.listDir is not a function` |

桌面复验从临时目录运行，并用 `DSHR_APP_ROOT` 指向含 `sidecar/dsh-official` 的隔离资源根。Headless 通过独立的 `DSHR_PROFILE_DIR` 启用 `fs`、`todo`、`working-directory` 和 `str-replace-editor`，用 `DSHR_WORKSPACE` 指向临时工作区。用本地 OpenAI 兼容测试服务按表中顺序返回工具调用，在 `DSHR_HOME` 的会话日志中核对工具结果与会话事件。 [Headless 工具和会话配置](headless-profile.md#3-运行配置与工具)

### rc.5 自动验收

```powershell
powershell -NoProfile -ExecutionPolicy Bypass -File tools\official-rc5-gate.ps1
```

验收入口从官方 Windows 安装包抽取 29 个工具包和 21 个配套包及依赖到独立资源目录，按来源锁检查 `package.json`、`lib/index.js` 的版本和摘要。Windows 27 项累计用例覆盖文件、图片、进程、提问、交付、Skills、网页、Office、会话编排和执行器。显式传 `-AsarPath` 可选择其他安装档，`-PrimaryRuntime` 可指定相应运行时目录。缺包、版本或摘要变化以及验收跳过均导致失败。Linux Bash 的前台、后台与持久终端有三个独立的 Rust 宿主官方包测试。 [验收脚本](../../tools/official-rc5-gate.ps1) · [rc.5 来源锁](../../sidecar/dsh-official/official-rc5-packages.lock.json)

Windows 自动验收覆盖预设内目录切换、会话间路径隔离、重启恢复、待办落库、编辑器各子命令、审批拒绝与取消。读图用例验证缩放、会话存储、重启重放、损坏文件和越界路径拒绝，以及文本模型的错误结果。 [Rust 宿主用例](../../crates/compat-dsh-host/tests/official_packages.rs) · [桌面与 headless 用例](../../apps/desktop-gpui/tests/headless_cli.rs)

更新矩阵时记录实际包版本、验证的工具或服务操作、启用条件和测试出处。升级抽取物后保留原版本记录，
按新版本的实测结果补行。[DSH 版本漂移记录](../decisions/0003-dsh-pin.md)
