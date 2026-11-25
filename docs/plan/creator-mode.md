# dshr 创造模式实施计划

> 首版、会话作用域、DSH 静态预设修订及后台热重载已实现。Windows 原生工具链验收通过。[创造模式规范](../spec/creator-mode.md) · [Agent 预设规范](../spec/agent-presets.md)

| 阶段 | 任务与依赖 | 验收 |
|---|---|---|
| 1. 会话入口 | 复用会话创建事件持久化预设，接入桌面 `/creator` 和模式提示 | 普通会话、子会话和 headless 无创作工具，重开创造根会话恢复预设 |
| 2. 创作工具 | 复用工具请求与 ADC-v1 工具卡，提供运行时信息与静态预览 | 合法元素显示在工具卡，非法元素返回诊断 |
| 3. 插件试用 | 复用审批和 CPP-v1 宿主，从工作区创建快照，确认后装载 | 拒绝不触发进程或注册，批准后工具可用 |
| 4. 交付 | 运行桌面与宿主测试，补充作者指南和兼容边界 | 下列验收命令通过，并记录测试平台 |
| 5. 会话与调用身份 | 先定义创造模式和 Sidecar 契约，再接入会话宿主、模型步骤快照、注册撤销，最后同步指南与兼容矩阵 | `CPP-CREATOR-005..006`、Sidecar 动态注册回归和官方包验收 |
| 6. DSH 预设修订 | 对照原版。接入静态声明、修订与子会话绑定，再复验创造工具和官方包。 | `M3-PRESET-001..004` 与桌面、Sidecar 测试 |
| 7. 后台配置热重载 | 用相同 Loader 在后台读取并发布。保留已绑定修订与上一次有效配置，同步使用指南和兼容矩阵。 | `M3-PRESET-005` 与 Windows 原生桌面、Sidecar 验收 |
| 8. 真实模型插件开发 | 用 DeepSeek 在隔离工作区写源码，使用 Windows 工具链构建，回到创造会话调用工具。 | `CPP-CREATOR-007` |
| 9. 空工作区开发 | 从空工作区复验写包、预览、构建及试用。官方写入单独验证既有行为。 | `CPP-CREATOR-007..008` 及文件写入回归 |
| 10. 诊断工具隔离 | 桌面和 headless 产品入口排除探针，创造模式提供 `creator_write_file`，正式归档只带官方 DSH Sidecar。 | `CPP-CREATOR-007..009`、CLI 工具清单与归档回归 |
| 11. 清单指引 | 在工具响应中提供清单样例与入口规则，试用前定位原文件和缺失产物。 | `CPP-CREATOR-010` 和不在测试提示里给出清单的 `CPP-CREATOR-007` |
| 12. 对话内构建 | 连接 Rust 外部进程的编译入口、绑定 SDK 和独立审批，真实模型测试覆盖从写入到调用。 | `CPP-CREATOR-007`、`CPP-CREATOR-011` |
| 13. 新会话选择预设 | 在工作区选择器旁切换空白根会话的模式，保持已有会话的修订绑定。 | `M3-PRESET-004..006` 与 Windows 桌面交互验收 |
| 14. 定位本机 Cargo | 桌面读取应用内工具链配置，复用自定义安装和离线缓存。 | `CPP-CREATOR-012` 与真实模型构建 |
| 15. 交互面板开发 | 提供完整面板示例、只读控件预览、首次渲染校验与原会话重试。 | `CPP-CREATOR-013..014` |
| 16. 会话中显示面板 | 试用结束后把面板快照交给桌面，在试用结果卡片内显示并随工具调用刷新。 | `CPP-CREATOR-015` |
| 17. 按名称发现接口 | 先定义 `creator_inspect` 的查询参数与错误，再从槽位目录、元素 Schema 和 SDK 提供精确结果。修正试用工具对首次渲染的说明。 | `CPP-CREATOR-016..018`，通过现有宿主动作链验证模板外控件 |

持久安装继续使用现有 CLI。

## 验收命令

```powershell
node tools/check-links.mjs
node schema/check.mjs
tools\build-env.cmd cargo test --locked --offline -p dshr-tool-runtime -p dshr-plugin-host --lib
tools\build-env.cmd cargo test --locked --offline -p dshr-desktop-gpui --bin dshr
tools\build-env.cmd cargo test --locked --offline -p dshr-desktop-gpui --test headless_cli
tools\build-env.cmd cargo test --locked --offline -p dshr-compat-dsh-host
tools\build-env.cmd cargo test --locked --offline -p dshr-profile -p dshr-compat-dsh-protocol
tools\build-env.cmd cargo test --locked --offline -p dshr-desktop-gpui --bin dshr desktop_reloads_preset -- --ignored
tools\build-env.cmd cargo test --locked --offline -p dshr-desktop-gpui --bin dshr m3_preset_005 -- --ignored
powershell -NoProfile -ExecutionPolicy Bypass -File tools\official-l2-gate.ps1
tools\build-env.cmd cargo build --locked --offline -p dshr-desktop-gpui --bin dshr
$env:DSHR_SIDECAR = 'probe'
target\debug\dshr.exe --selftest
Remove-Item Env:\DSHR_SIDECAR
target\debug\dshr.exe --selftest-dsh
node tools/conformance.mjs --quick
```

### 精确查询验收

常规桌面测试运行 `creator_inspect_exact_contracts_match_discovery_and_runtime`。在有 Cargo 和离线 SDK 依赖的环境另外运行 `cargo test -p dshr-desktop-gpui --bin dshr creator_inspect_drives_external_slot_callback -- --ignored`。该测试从工具返回的注册例子、元素定义和回调签名生成右栏滑块插件，经正式构建和试用工具启动子进程，再走桌面槽位动作派发验证刷新值。

### 真实模型验收

将 `DSHR_LIVE_CREATOR_DIR` 指向空工作区或只含 `hello-world` 包副本的测试工作区。用应用数据根的 `creator-toolchain.json`、`PATH` 或 `DSHR_CREATOR_CARGO` 指向本机 Cargo，确保缓存包含 SDK 依赖。测试进程另需 `DEEPSEEK_API_KEY` 和 `DSHR_SIDECAR=dsh`。运行 `cargo test -p dshr-desktop-gpui --bin dshr live_creator_authors_and_invokes_hello_world -- --ignored --nocapture`。
空工作区测试提交「做一个 hello world 插件，并试运行」。包副本测试要求继续构建并处理编译诊断。测试端只回应隔离工作区内的审批。会话日志须记录 agent 查指引、用 `creator_build_plugin` 构建、批准试用并调用插件工具的顺序。

交互面板用例使用空工作区，运行 `cargo test -p dshr-desktop-gpui --bin dshr live_creator_authors_and_exercises_an_interactive_panel -- --ignored --nocapture`。真实模型需一次构建成功并试用已注册的面板。测试通过与桌面相同的动作派发提交输入和按钮，验证重新渲染后的问候文本。

精确查询的真实模型用例使用空工作区，运行 `cargo test -p dshr-desktop-gpui --bin dshr live_creator_authors_a_slider_from_exact_contracts -- --ignored --nocapture`。会话日志须在首次写文件前出现右栏页签、`Slider` 和 SDK 的精确查询，随后首次构建和试用成功，宿主派发动作后滑块显示 `7.5`。

### 交互面板故障复盘

会话 `s-1795271304112` 在收到带界面的请求后，五次查找 SDK 文件失败，`creator_preview_panel` 报错 39 次。生成的 Rust 包缺少直接的 `serde_json` 依赖。补齐依赖后首次试用返回 `panels: []`，因为没有将面板放进 `ReadyDecls`。修正并重建后，试用又因同一 ID 已装载而失败。随后调用的是旧试用实例的工具。

Windows 本机真实模型复验 `s-1795272235422` 一次读取指引、三次写入、一次构建、一次试用，试用返回面板 `Hello`，工具调用均无错误。测试提交 `Panel Check` 并派发按钮动作，重新渲染的 Text 含该姓名。轨迹在本地 `.tmp/creator-panel-live-20261121-224355/workspace/sessions/`。35 项桌面测试、81 项插件宿主测试与 17 项 ADC 点击和键盘测试通过。

2026-11-21 干净 Windows 11 企业版 build 26100 来宾复验通过。发行归档 SHA-256 为 `e8f90ec9c4bb6c77a2d91ab062edade0aac8446bcc86e28a3ddefa2f9a3b87d5`。来宾基线无 Node、Python、Cargo 或 DSH。安装 Python `3.13.12` 和创造模式构建组件 `1.93.0-sdk.1` 后，真实 DeepSeek 会话 `s-1795272437147` 首次构建即成功（14），`expect_panel: true` 的首次试用渲染出 `Hello` 面板（16），并调用 `hello_status`（18）。宿主派发输入和按钮动作后，面板重新渲染，问候文本包含 `Panel Check`。全会话工具调用无错误。验收日志与轨迹位于本地 `.tmp/runtime-component-guest/panel-case/evidence/`。

### 对话窗口显示复验

实际会话 `s-1795271571903` 的 `creator_try_plugin` 已返回「笔记面板」（32）。旧界面只在「插件 → 插件面板」渲染，且面板快照只在试用列表变更时刷新。

Windows 窗口用例要求 `DSHR_HOME` 和 `DSHR_WORKSPACE` 分别指向同一隔离父目录下的数据和工作区。工作区含已构建的 `demo.notes-panel` 包，数据目录含创造根会话。配置真实 DeepSeek 后，运行 `cargo test -p dshr-desktop-gpui --bin dshr live_creator_desktop_displays_trial_panel -- --ignored --nocapture`。在对话中试用 `notes-panel`，再调用 `notes_add`。检查面板在试用结果卡片内自动展开，随对话滚动，支持收起、展开，后续内容刷新到同一卡片。再次试用后检查只有新的结果卡片包含活动面板。测试驱动只批准该测试包的试用。

2026-11-21 使用实际 `notes-panel` 构建产物的副本复验，窗口未进入插件页。会话 `s-1795273308702` 试用成功后，消息列表上方显示「笔记面板」与「1. 面板验收」。下一轮只调用 `notes_add`，同一区域更新为两条笔记并显示「2. 刷新确认」。按「收起面板」和「展开面板」后仍显示两条。35 项常规桌面测试通过。轨迹保存在 `.tmp/notes-panel-window-check/data/sessions/`，Windows x64 发行归档的 SHA-256 为 `8e271c62472b5ef7b2c888a4f0a2f64b0bfc2f756e27097d754f7c9318fa8aea`。

DSH Desktop `0.2.2-rc.5` 的安装包 `dsh-client-ui-cordis/src/client/CordisRunRow.tsx` 和 `slots.ts` 声明，运行中的插件业务视图挂载在最近一次符合条件的 `cordis_run` 卡片内。dshr 采用对应的会话内挂载方式。

2026-11-22 在 Windows 原生窄窗复验。真实 DeepSeek 会话 `s-1795292369542` 对 `demo.notes-panel@0.1.0` 成功试用两次，第二次返回 `replaced: true`（12），随后调用新实例的 `notes_add`（14）。约 721 × 439 逻辑像素的窗口在回合结束后完整显示一张试用结果卡片，面板仅显示新实例的「对话卡片复验」，底部输入框仍可见。会话轨迹和窗口截图位于本地 `.tmp/notes-panel-inline-replay/`。38 项桌面测试和 26 项会话模型测试通过，包含调用 ID 重用、失败重试和两个插件的面板归属。此次窗口复验使用只读截图，未操作折叠和滚动控件。

## 进度

- 2026-11-25 `creator_inspect` 支持按名称查询槽位、元素和 SDK。Windows 离线验收用查询结果构建了右栏 `Slider` 插件，通过正式试用和槽位动作派发将值由 `2.0` 更新至 `7.5`。真实模型用例已加入验收，当前环境缺少 DeepSeek 凭据，待运行。

- 2026-11-21 确定首版采用原生创造会话与临时 CPP-v1 试用，先记录公开契约和失败条件。
- 2026-11-21 Windows MSVC 通过 20 项桌面单测和 9 项 headless CLI 回归。`CPP-CREATOR-001..004`
  在 `DSHR_APPROVAL_MODE=allow` 环境下复跑通过，试用仍等待显式用户决定。
- 2026-11-21 Windows MSVC 通过 31 项工具审批与注册表测试，以及 81 项插件宿主测试。
- 2026-11-21 Windows 的外部进程安装、调用、面板、卸载及重载自检通过，WASM Component 持久安装与工具调用自检通过。
- 2026-11-21 快速契约验收、文档链接和 Schema 校验通过。真实窗口的点击与键盘操作待专项验收。
- 2026-11-21 会话作用域对齐通过 27 项桌面单测和 9 项 headless CLI 回归。桌面 `--selftest` 验证跨工作区返回后保留 Sidecar generation，并校验工具事件携带原注册身份。
- 2026-11-21 官方 gate 通过 9 项测试，包含 `RPC-REG-001` 和此前的 8 项官方包用例。桌面 `--selftest-dsh` 与 `0.2.2-rc.5` 只读操作复验通过。[按版本记录的运行证据](../spec/dsh-compat-mapping.md#验证与复现)
- 2026-11-21 预设修订验收通过。官方 Sidecar gate 的 10 项用例覆盖修订共享、退役回收和官方 `working_directory` 在预设内执行。桌面专项验证配置重读和子会话继承。Loader、协议、桌面和 headless 回归通过。
- 2026-11-21 Windows 后台热重载验收通过。空闲时发布了 bundle、patch 和 include 的修改，当前及停放工作区均收到新修订。旧会话和继承子会话仍使用原实例，无效配置修复后自动恢复。
- 2026-11-21 使用真实 `deepseek-chat` 完成 Hello World 验收。模型通过 `probe_write` 写出源码，收到编译器的 `E0308` 后修复返回类型。Windows 原生构建成功，模型通过 `creator_try_plugin` 发起已批准的试用并调用 `hello_world`，会话日志记录了 `Hello World from dshr!`。
- 2026-11-21 空工作区回归通过。真实 `deepseek-chat` 使用官方 `write` 创建插件目录、清单和源码，并成功预览静态 ADC 面板。模型根据编译和清单诊断修正文件，Windows 原生构建后批准试用，模型实际调用 `hello_world` 得到 `Hello World from dshr!`。这是回滚官方写入改动前的记录。随后的空目录验收使用了诊断工具。
- 2026-11-21 恢复官方文件工具的既有行为。Windows 回归确认官方 `write` 遇到缺失父目录时返回错误，且不发起审批。真实 `deepseek-chat` 从空工作区用原生 `probe_write` 创建包文件，预览 ADC、根据编译诊断修正源码，构建并批准试用后调用 `hello_world`，返回 `Hello World from dshr!`。
- 2026-11-21 诊断工具隔离验收通过。Windows MSVC 通过 29 项桌面单测、10 项 headless CLI 测试及 2 项官方文件工具测试。桌面归档校验确认探针脚本缺席，官方 Sidecar、窗口及 CPP-v1 自检通过。仓库的探针和故障恢复自检通过。
- 2026-11-21 真实 `deepseek-chat` 在空工作区用 `creator_write_file` 创建了清单和源码，预览静态 ADC 面板。测试端在 Windows 原生编译，通过审批服务确认试用后，模型调用 `hello_world` 并收到 `Hello World from dshr!`。会话日志无 `probe_*` 调用。本轮测试提示预先给出了清单字段。
- 2026-11-21 复核桌面会话 `s-1795257268804`。旧工具响应只给出文件名和运行时名称，模型八次试用失败。新版工具回传两种最小清单、字段约束及 Windows 入口要求。真实 `deepseek-chat` 验收由模型先读取指引，首次生成的清单通过校验，Windows 原生编译后首次试用成功，返回 `Hello World from dshr!`。
- 2026-11-21 最终短提示回归通过。会话 `s-1795257953307` 先查指引，清单只写一次，静态面板预览通过。模型根据一次 SDK 源码编译诊断修正实现，首次试用成功，再次验证 `hello_world` 的实际输出。
- 2026-11-21 复核实际会话 `s-1795258762708`。SDK 查找失败，模型写完包后因缺少构建工具转交用户。新增 `creator_build_plugin`，并在 `s-1795259340394` 以真实 DeepSeek 和 Windows 本机 Cargo 验证。模型从空工作区依次写入、请求构建、请求试用并调用 `hello`，返回 `Hello, world!`。测试驱动只回应审批。31 项桌面单测、10 项 headless CLI 回归通过。
- 2026-11-21 新会话模式菜单验收通过。核对 DSH Desktop `0.2.2-rc.5` 的 Agent 预设选择器。Windows 真窗口验证空白会话在标准和创造模式之间改选时保留 ID 和草稿，首轮日志保存 `cordis`，发送后菜单收起。另验证窄窗菜单及 Escape 回到输入框。32 项桌面单测、10 项 headless CLI 用例和两项桌面预设重读专项通过。
- 2026-11-21 实际会话 `s-1795262859632` 的失败来自桌面进程无法发现仓库独立安装的 Cargo。加入应用配置后，在无 Cargo PATH、无构建覆盖环境变量的 Windows 进程中复验通过。真实 DeepSeek 在空工作区完成构建、试用和调用，也在实际包的副本中处理了 SDK 源码诊断并完成试用（`s-1795263292490`）。33 项桌面单测、10 项 headless CLI 回归和 Windows 归档校验通过。
