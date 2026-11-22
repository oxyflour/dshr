# DSH 官方工具包兼容实施计划

> 已完成六阶段工具侧验收。官方 `rc.5` 的 29 个工具包均有执行证据。部分能力的适配范围与后续验证项见兼容矩阵。 [兼容矩阵](../spec/dsh-compat-mapping.md#兼容矩阵) · [阶段 1 报告](dsh-official-plugin-compatibility-stage-1.md) · [阶段 2 报告](dsh-official-plugin-compatibility-stage-2.md) · [阶段 3 报告](dsh-official-plugin-compatibility-stage-3.md) · [阶段 4 报告](dsh-official-plugin-compatibility-stage-4.md) · [阶段 5 报告](dsh-official-plugin-compatibility-stage-5.md) · [阶段 6 报告](dsh-official-plugin-compatibility-stage-6.md)

先把已能执行的文件、待办和工作目录操作纳入自动回归，再逐步补齐标准模式依赖。每个包从官方安装档抽取，按原始包名和配置加载，通过实际工具调用、钩子或宿主状态变化验收。Windows 先验证 PowerShell，Bash 在 Linux 验收，macOS 单独记录实测状态。 [官方标准预设](https://github.com/deepseek-ai/deepseek-harness/blob/dsh-v0.2.2-rc.5/packages/bundle/web-app/presets/standard.patch.yml)

## 阶段和交付条件

| 阶段 | 范围 | 验收 |
|---|---|---|
| 1. 锁定证据，补齐文件语义 | 为 `rc.5` 建立独立来源锁和验收入口。自动复跑 `workspace-dependencies`、`fs`、`todo`、`working-directory`、`str-replace-editor`。打通会话目录、`fs.listDir`、编辑器写入和图片附件。 | 缺失或错误版本的抽取物使 gate 失败。桌面和 headless 的文件读写按切换后的会话目录解析。目录查看、编辑器写入、图片附件重放及待办落库都有断言。覆盖审批拒绝、越界路径和先前保存的会话恢复。 |
| 2. 搜索和前台 Shell | `fs-search`、`pwsh`、`bash`。依赖阶段 1 的会话目录。实现 `subprocess`、`shell`、`shellEnv` 的宿主适配，转发信号和退出状态，接入 Rust 权限与审批。 | `glob` / `grep` 随会话目录切换。平台对应的 Shell 在桌面通过前台运行、取消、超时和拒绝用例。Headless 工具清单继续按其规范过滤通用 Shell。 |
| 3. 提问、Skills、网页和文件交付 | `ask-user`、`skill`、`web`、`present`、`office`。为所需插件提供会话范围的 Agent 查询接口，接入 `systemPrompt` 段落、`agent/pre-step` 和 `tools/result` 等实际所需的钩子。文件交付复用阶段 1 的附件和文件访问。 | 桌面问题可回答和取消，headless 给出明确的非交互结果。Skill 在正确会话生效。搜索与抓取由本地服务验证超时和结果，文件与 Office 交付可在同一会话重放。每个工具记录实际验收的功能和所需可选服务。 |
| 4. 后台任务与终端 | `jobs`、`terminal`、`pwsh-persistent`、`bash-persistent`。依赖阶段 2 的命令执行。Rust 宿主记录任务与终端归属，桥接后台输出、状态通知、回收和停止。 | 运行、领取输出、停止均可验证。终端保存同一会话内的状态，其他会话不能查询或控制。取消、重启、Sidecar 退出均覆盖清理和状态回放。 |
| 5. 会话和 Agent 编排 | `goal`、`session-query`、`subagent`、`subagent-control`、`agent-team`、`workflow`、`ralph`、`schedule`。先交付目标、会话查询及子会话控制，再接团队、工作流和持久调度。复用 Rust 会话树、事件日志及 `sessionProjections`。 | 目标和调度恢复后状态一致。查询和控制按会话授权。子会话正常完成、失败和取消均写入可查询状态。验证工作流中止及重启后调度任务不重复投递。 |
| 6. 执行钩子与创造模式 | `call-timeout-policy`、`executor-native`、`executor-ptc`、`cordis`。先桥接 `tools/execute` 和只读 `cordisInspect`，再接入 DSH Native 与 PTC 执行器。 | 官方超时钩子作用于实际调用。`cordis_inspect_list` / `cordis_inspect_query` 返回当前会话有权访问的结果。执行器安装和撤销后，Rust 仍以同一套注册身份、权限、超时与取消规则协调调用，程序内的嵌套调用同样受这些规则检查。 |

新增能力先由显式测试 profile 启用。在开启默认配置前，用发行包复验可选运行组件、平台差异、当前 `rc.1` profile 的升级路径和主流工作流。桌面交付完整原版 `cordis_run` 交互卡片前，单独验证动态运行器与 Cordis Client 槽位，并确认 bundle 装载、启停与配置编辑。这个客户端与 bundle 验收独立跟踪。 [DSH 创造预设](https://github.com/deepseek-ai/deepseek-harness/blob/dsh-v0.2.2-rc.5/packages/bundle/web-app/presets/cordis.patch.yml) · [dshr 创造模式规范](../spec/creator-mode.md)

## 第一阶段实施顺序

1. 在独立资源目录抽取五个官方包及依赖。记录实际 `package.json` 版本和来源摘要，将桌面写入自检、headless 会话事件和预设作用域验证纳入同一个 `rc.5` gate。先用现有行为验证目录切换后相对 `read` 仍读取工作区根，再用同一用例验证修复后的路径。
2. 更新 Sidecar 文件和会话契约。为需要目录的文件工具提供当前调用对应的会话目录。归一化后仍由宿主工作区边界和审批判断是否可写。覆盖两个会话分别切换到同名文件所在的不同子目录、跨越工作区边界、取消和未协商服务。`fs.listDir` 使用相同的路径和错误语义，编辑器写入统一经过 `fs.write`。
3. 将 `attachments` 的实际读图方法接到现有附件存储和模型能力判断。验证文件格式、大小、工作区权限和会话授权，确认工具结果中的附件引用可重放。图片服务未启用时验证工具注册表，图像格式或模型能力不匹配时验证结构化错误。 [RPC 文件回调](../spec/sidecar-rpc.md#75-文件写入回调fswrite) · [RPC 附件引用](../spec/sidecar-rpc.md#138-载荷上限与附件引用)

## 验收命令

```powershell
node tools/check-links.mjs
node --test sidecar/dsh-official/lib/host-services.test.mjs sidecar/dsh-official/lib/session-projections.test.mjs
powershell -NoProfile -ExecutionPolicy Bypass -File tools\official-rc5-gate.ps1
powershell -NoProfile -ExecutionPolicy Bypass -File tools\official-l2-gate.ps1
tools\build-env.cmd cargo test --offline -p dshr-compat-dsh-host -p dshr-compat-dsh-fs -p dshr-tool-runtime
tools\build-env.cmd cargo test --offline -p dshr-desktop-gpui --test headless_cli
tools\build-env.cmd cargo test --offline -p dshr-desktop-gpui --bin dshr
node tools/conformance.mjs --quick
```

现有 `official-l2-gate.ps1` 复跑 `rc.1` 来源锁。`tools/official-rc5-gate.ps1` 用实际的 `rc.5` 官方依赖执行断言，缺抽取物、摘要不符、缺预期调用或出现跳过均使对应 gate 失败。各阶段先更新受影响的规范、Schema 和契约测试，再实现适配并复验。验收后在矩阵中按平台、包版本和通过的操作更新状态，并在作者指南同步启用方法。 [官方 `rc.1` gate](../../tools/official-l2-gate.ps1) · [插件作者指南](../plugin-authoring.md)

## 进度

- 2026-11-22 完成 `rc.5` 工具包清点及文件写入、会话目录、待办和目录查看复验。 [复验记录](../spec/dsh-compat-mapping.md#rc5-执行复验)
- 2026-11-22 阶段 1 完成。文件、图片附件和会话恢复通过 Windows 自动验收。 [阶段 1 报告](dsh-official-plugin-compatibility-stage-1.md)
- 2026-11-22 开始阶段 2。核对官方 ripgrep 参数接口与 PowerShell、Bash 的前台执行接口，记录宿主进程的授权、回收和输出契约。 [前台进程契约](../spec/sidecar-rpc.md#78-官方工具前台进程)
- 2026-11-22 阶段 2 完成。Windows 搜索和桌面 PowerShell、Linux Bash 宿主用例通过。 [阶段 2 报告](dsh-official-plugin-compatibility-stage-2.md)
- 2026-11-22 开始阶段 3。先接入官方前台提问，定义工具调用作用域、桌面应答和 headless 失败语义。 [提问契约](../spec/sidecar-rpc.md#79-官方工具提问)
- 2026-11-22 阶段 3 的前台提问通过桌面运行时和 headless 官方包验收。图形交互及 Skills、网页、文件交付和 Office 验收待完成。
- 2026-11-22 阶段 3 完成。提问、Skills、网页、文件交付及 Windows OfficeKit 的转换、重算和两页渲染通过专项 gate。 [阶段 3 报告](dsh-official-plugin-compatibility-stage-3.md)
- 2026-11-22 开始阶段 4。先实现可在会话重启后查询的后台任务，再接持续运行的终端与跨调用的 Shell 状态。 [后台任务契约](../spec/sidecar-rpc.md#714-官方后台任务)
- 2026-11-22 阶段 4 完成。Windows 后台任务与持久 PowerShell、Linux 后台 Bash 与持久 Bash 通过验收。 [阶段 4 报告](dsh-official-plugin-compatibility-stage-4.md)
- 2026-11-22 开始阶段 5，定义目标状态、自动继续授权及会话查询边界。 [目标和查询契约](../spec/sidecar-rpc.md#716-官方目标与会话查询)
- 2026-11-22 阶段 5 完成。八项会话、子 Agent、团队、流程和调度专项用例通过。 [阶段 5 报告](dsh-official-plugin-compatibility-stage-5.md)
- 2026-11-22 开始阶段 6。执行器沿用宿主步骤内的注册快照，程序化调用使用 Rust 分配的子请求身份。 [执行契约](../spec/sidecar-rpc.md#721-官方执行钩子执行器与-cordis-检查)
- 2026-11-22 阶段 6 完成。官方 Native、协作超时、Cordis Host 检查、PTC 宿主审批和预设撤销通过用例。 [阶段 6 报告](dsh-official-plugin-compatibility-stage-6.md)
