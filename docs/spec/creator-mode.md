# dshr 创造模式

> 状态为 Draft。首版范围和进度见 [实施计划](../plan/creator-mode.md)。

## 会话入口

在桌面新会话输入卡上方，打开工作区旁的模式菜单并选择「创造模式」。发送第一条消息前可改选，保留当前会话和草稿。`/creator` 和插件页面的「新建创造会话」可作为快捷入口。官方 Sidecar 使用
`preset: "cordis"` 和 [Agent 预设契约](agent-presets.md)。旧 `dshr-creator` 根会话和诊断 Sidecar 使用原生创造模式。
两种创造根会话都可使用下列 CPP-v1 创作工具。子会话按预设契约继承 Cordis 工具，原生 CPP-v1 试用仍属于各自根会话。
创造模式复用当前模型和工具权限设置。

## 创作工具

| 工具 | 参数 | 成功结果 |
|---|---|---|
| `creator_inspect` | 空对象，或同时传 `section` 和 `name` | 空对象返回创作入口与可查名称。精确查询返回一个槽位、ADC 元素或 Rust SDK 接口的契约 |
| `creator_preview_panel` | `element` 为 ADC-v1 元素树 | 在工具结果中呈现只读视觉预览，包括合法的 Button 和 Input。校验失败时返回元素路径和修正指引 |
| `creator_write_file` | `path` 为工作区相对文件路径，`content` 为完整 UTF-8 文本 | 写入文件并返回路径、字节数及覆盖前的快照 ID |
| `creator_build_plugin` | `path` 为工作区相对的 Rust 外部进程插件目录 | 在用户批准后使用本机 Cargo 构建，自动提供匹配的 SDK，并把可执行文件放到清单的 `runtime.entry` |
| `creator_try_plugin` | `path` 为工作区相对的 CPP-v1 包目录。面板任务同时传 `expect_panel: true` | 返回插件 ID、会话可用的工具、通过首次渲染校验的工作区面板及是否替换旧试用。包含面板时，`panel_location` 指明当前会话内的试用结果卡片 |

### 精确查询

`creator_inspect({})` 保留清单、构建状态、示例和当前工具列表，并在 `queries` 中列出各 `section` 的可查名称。写代码前按实际用到的接口查询。`section` 为 `slot`、`element` 或 `sdk`，`name` 大小写敏感。只提供一个字段、提供空名称、额外字段或未知名称时返回错误。未知名称错误提示对应的 `queries` 入口。查询为只读操作，无需审批。 [参数 Schema](../../schema/creator-mode/inspect-arguments.schema.json)

| `section` | 示例 `name` | 详情 |
|---|---|---|
| `slot` | `viewer.tab` | 当前桌面支持的位置、作用域、注册字段和键规则、挂载触发、SDK 回调及其上下文、可用动作、试用验证阶段和实际验证方法。`workspace.panel` 返回 `PanelDecl` 路径 |
| `element` | `Slider` | 从 ADC-v1 Schema 选出的节点和 props 定义，附相关 `$ref` 的定义和控件动作的触发及载荷。递归子节点引用 `#/$defs/node`，可按子元素名称继续查 |
| `sdk` | `ViewHandler` | 随创造模式构建提供的 `dshr-plugin-sdk` 的函数签名或结构体字段，以及适用场景和载荷示例 |

精确查询成功返回 `section`、`name` 和 `contract`。槽位查询中的 `validation` 对应试用结果，查询本身不启动插件。`registration` 表示仍需触发该槽位才能验证渲染或动作，`rendered` 表示试用已验证首次取树。元素查询返回创作时的严格 Schema 片段，挂载处是否派发动作仍取决于槽位及会话状态。 [ADC-v1 元素](adc-v1.md#6-元素词汇表)

### 清单指引

开始编写包时，工具说明引导模型先调用 `creator_inspect`。结果中的 `manifest` 给出两种运行时的最小可解析 TOML 样例，以及必填表、平台入口、权限默认值和面板声明规则。`runtime.entry` 必须指向包内已构建的入口，Windows 外部进程使用 `.exe`，WASM 使用编译好的 `.wasm`。宿主通过 CPP-v1 协议启动外部进程，清单不支持 `runtime.command`。未知字段导致解析失败。对于 `[[ui]]`，`view` 是运行时用于渲染的字符串标识，ADC-v1 树由预览工具或已注册的面板运行时提供。检查失败返回相对于工作区的清单路径、诊断和 `creator_inspect` 修正入口。入口文件缺失时，错误指出要构建的包内路径。两类失败均发生在试用审批之前。

[CPP-v1 清单字段](cpp-v1.md#33-字段表) · [SDK 面板示例](../plugin-authoring.md#5-用-sdk-写面板adc-v1)

`creator_preview_panel` 使用 ADC-v1 解析器与工具卡的 `meta.ui` 渲染路径。控件在预览中只显示外观，工具卡不派发动作。实际交互由试用中的工作区面板提供。

面板任务先查看 `creator_inspect.workspace_panel`。示例用 `ReadyDecls::new(tools, vec![panel])` 注册外部进程面板，`PanelDecl` 的最后一个参数是 view ID。使用 `json!` 和 `Value` 时在 `Cargo.toml` 直接声明示例给出的 `serde_json` 版本。SDK 由构建工具注入临时构建目录。

通过 `creator_inspect.adc_slots` 获取桌面位置和注册示例，通过 `ReadyDecls::with_slots` 声明。只试用槽位时省略 `expect_panel`。成功的 `slots` 条目包括 `slot_id`、`location` 和 `validation`，同时保留注册的 `id`、`key`、`label` 及 `view` 或 `run`。普通视图通过取树校验后返回 `validation: "rendered"`。工具卡、对话节点、文件预览和命令条目返回 `validation: "registration"`。在同一会话调用插件自身工具验证工具卡和插件发布的节点。选择交付文件验证预览，从菜单或快捷键验证命令。 [桌面槽位注册](adc-v1.md#52-桌面槽位注册)

`Input.onChange` 和 `Button.onPress` 可用 `{"call":"方法名","args":{}}`。回调参数依次是 view ID、动作种类、载荷。`call` 的动作种类为 `"call"`，载荷包含 `method` 和 `args`，Input 提交值位于 `args.value`。`command` 的动作种类为 `"command"`，标识符位于 `payload.id`。
[ADC-v1 元素](adc-v1.md) · [工具卡与面板](../m4-adc.md)

可以从空工作区开始。将 `plugin.toml`、构建配置和源码写到同一个新的包子目录，例如 `hello-world/plugin.toml` 与 `hello-world/src/main.rs`。用 `creator_write_file` 写入文件。该工具仅在创造根会话可用，沿用宿主文件写入策略和审批。获准后才补齐缺失的父目录。只读模式、工作区外路径、链接路径和超过 1 MiB 的写入均会失败。官方 `write` 要求父目录已存在。构建完成后，将包目录交给 `creator_try_plugin`。

## 对话内构建

工具说明给模型的默认顺序为 `creator_inspect` → 写包 → `creator_build_plugin` → `creator_try_plugin` → 调用已注册的工具。静态预览按任务需要使用。构建接收独立的 Rust Cargo 包，包含 `Cargo.toml`、`src/`、`plugin.toml`，可包含 `Cargo.lock`。将本地 SDK 依赖声明为 `dshr-plugin-sdk = { path = "sdk" }`。宿主在隔离的临时构建目录提供同版本 SDK。入口为 `bin/<Cargo 二进制名>.exe`（Windows）或 `bin/<Cargo 二进制名>`（Unix）。

构建使用 Cargo 离线编译单个二进制。工具链优先取 `DSHR_CREATOR_CARGO`，其次取应用数据根的 `creator-toolchain.json`。Windows 便携包未显式设置 `DSHR_HOME` 时，也可读取 `%LOCALAPPDATA%/dshr/creator-toolchain.json`。随后查找应用安装的构建组件、`PATH` 和 Cargo 常规安装位置。Windows x64 发行包可从「设置 → 运行组件」安装编译器和离线 SDK 依赖。[运行组件契约](runtime-components.md)

`creator-toolchain.json` 包含必填的 `cargo`（Cargo 可执行文件绝对路径）和可选的 `cargo_home`（已有 Cargo 缓存目录的绝对路径）。设置文件只影响插件构建，重新读取后即可在当前会话重试。显式设置无效时，`creator_inspect.build.setup_error` 和构建错误报告应用无法使用该配置的原因。只读模式拒绝构建。批准前准备源码快照，审批显示包目录和目标入口，并提示 Cargo 构建依赖可执行本机代码。每次构建均需显式批准。批准后以当前用户离线运行 Cargo，移除继承的凭据变量。构建结果只发布到清单指定的工作区内入口。失败、拒绝、取消和超时均保留已有入口文件。编译错误返回诊断，agent 修正源码后可重新请求构建。试用有单独的审批。

## 试用与失败条件

试用接受 CPP-v1 `external-process` 与 `wasm-component`，复用产品安装后的运行时加载路径。宿主在启动前复制一份临时包快照，检查 manifest，并在审批面板中显示插件 ID、版本、运行时、来源和声明权限。外部程序以当前账号运行。每次试用都由用户明确允许，工具免审批设置和自检环境变量不代替这次决定。只读权限下拒绝试用。

工作区外的路径、链接、特殊文件、超过 64 MiB 或 2048 个条目的包、超过 32 层的目录及不合法的 manifest 都会在启动前失败。拒绝、取消或超时不会启动插件。装载失败或同一试用宿主内的注册冲突返回工具错误，原有工具和面板仍可用。试用工具和面板仅在其创造根会话可见。不同会话可试用同一个包或注册同名工具。会话内的试用工具优先于继承的同名全局工具，审批、参数校验和派发使用该次解析得到的定义。切换会话保留原实例和临时包，返回时继续使用。删除所属会话或退出应用会释放试用实例和快照。试用包不会写入插件 profile。需要持久使用时，通过现有 `dshr --install-plugin` 安装已构建的包。

获准启动的实例必须能首次渲染其声明的 `workspace.panel`，并通过 ADC-v1 树校验。`expect_panel` 默认为 `false`，设为 `true` 时至少需要一个此类面板。试用校验只渲染面板，不执行交互动作。面板试用成功以结果中的非空 `panels` 为准。

一轮生成结束后，运行中的试用面板显示在当前创造会话中对应插件最近一次成功试用的结果卡片内，随对话一起滚动。正在跟随最新消息时会定位到新卡片，向上浏览历史时保留当前位置。即使工具过程折叠，当前面板仍默认可见。用户可在卡片内折叠和展开面板。成功重试将面板移至新的结果卡片，失败的重试保留原位置。历史试用结果保留可回放的摘要。生成期间已有面板可查看，交互在生成结束后恢复。后续工具调用和面板动作会刷新同一面板。插件页的「插件面板」也可访问当前会话的面板，包括对应试用结果已移出可见历史的情形。

对同一创造会话中来自同一个包目录、同一插件 ID 的试用，`creator_try_plugin` 使用新快照重新请求审批。新实例启动、面板校验及注册检查都通过后才替换旧实例。被拒绝、取消、校验失败或与其他插件注册冲突时保留旧实例和状态。不同包目录使用同一 ID 会报错。当前模型步骤中排队的旧实例调用返回可重试错误。新实例从下一次模型请求起可调用。
[CPP-v1 规范](cpp-v1.md) · [插件信任](plugin-trust.md)

## 兼容性与验收

内置 `cordis` 以可扩展的最小子插件列表配合 dshr 创作工具。显式启用 `dsh-tool-cordis` 可检查当前会话的工具 Schema 和 Host 协商服务元数据。原版完整工具组合、Client 插槽、动态运行器和插件管理流程需要相应宿主适配。每次桌面模型请求固定当步可用的工具及注册身份。新增会话工具从下一步起可见。模型请求中未公开的工具、已撤销的注册和失效的 generation 返回配对的工具错误。试用结果只列出会话可用的工具，`probe_*` 前缀保留给诊断。
[DSH 兼容矩阵](dsh-compat-mapping.md) · [验收命令](../plan/creator-mode.md#验收命令)

| 用例 | 验证内容 |
|---|---|
| `CPP-CREATOR-001` | 重开创造根会话恢复工具，普通会话、子会话和 headless 按各自工具集执行 |
| `CPP-CREATOR-002` | 合法 ADC 树及控件可只读预览，非法元素返回路径诊断 |
| `CPP-CREATOR-003` | 模型发起的预览调用落盘，重放后工具卡仍带 ADC 预览 |
| `CPP-CREATOR-004` | 只读和越界路径失败，拒绝后无工具注册，批准时使用已准备的快照 |
| `CPP-CREATOR-005` | 两个创造会话独立使用同名工具，切换后复用原实例，删除清理所属试用 |
| `CPP-CREATOR-006` | 同一步不能调用新注册的工具，下一步调用和事件审计采用新定义 |
| `CPP-CREATOR-007` | 真实模型从简短任务出发，读取 `creator_inspect`，通过产品工具请求 Windows 构建和试用，并调用新工具 |
| `CPP-CREATOR-008` | 创造根会话用原生写入创建此前不存在的多层包目录 |
| `CPP-CREATOR-009` | 桌面、创造根会话及 headless 工具清单和派发均排除 `probe_*` |
| `CPP-CREATOR-010` | 模型可在写文件前获取清单样例，样例通过宿主校验。清单无效或产物缺失时，试用错误可定位原始包且无需审批 |
| `CPP-CREATOR-011` | 构建审批、入口边界、SDK 随包注入、失败或拒绝时保留已有入口 |
| `CPP-CREATOR-012` | 应用本地工具链配置可解析，拒绝错误路径，普通启动环境可使用指定 Cargo 与离线缓存 |
| `CPP-CREATOR-013` | 面板缺失、渲染失败及无效 ADC 树拒绝试用；同一包可获准替换，失败时保留旧实例 |
| `CPP-CREATOR-014` | 真实模型接到带界面的简短任务后写包、构建并试用，面板渲染成功，输入和按钮动作回传并改变视图 |
| `CPP-CREATOR-015` | Windows 正常对话页的试用结果卡片内显示实际面板。面板随对话滚动，支持折叠、展开及内容刷新。成功重试只有新的结果卡片挂载活动面板 |
| `CPP-CREATOR-016` | 精确查询接受正确的参数组合，目录中的名称均可查询。槽位字段、控件 Schema 和 SDK 上下文与对应注册或渲染契约一致 |
| `CPP-CREATOR-017` | 从精确查询取得模板外槽位和控件的契约，构建并试用真实外部进程插件，经宿主动作派发验证回调和视图刷新 |
| `CPP-CREATOR-018` | 真实模型只收到右栏滑块任务，通过查询槽位、元素和 SDK 取得契约，在首次构建和试用成功后触发回调 |
