# Headless CLI 规范

> 状态：Draft · 目标入口：`dshr --profile headless`
> 实施、验收命令和进度见 [Headless 实施计划](../plan/headless-profile.md)。

## 1. 范围

支持终端、脚本和 CI 提交一个任务，运行一轮 DSHR Agent，持久化结果并退出。任务期间可以调用多次模型和工具。宿主无需显示环境，启动时不初始化 GPUI、ADC 面板或剪贴板，也不开监听端口。

兼容基线为本机 DSH `0.2.2-rc.5` 的 `dsh --profile headless --help`，运行语义参照官方 headless 文档。首版对齐显式 profile 入口、argv 与 stdin、最终答复、JSONL、按 ID 续跑和 `0` / `1` 退出语义。具名 profile 与配置叠加见 [§7 声明兼容](#7-dsh-profile-声明兼容)。DSH 的简写入口、会话文件互读、完整 JSON 事件、`--permission-mode`、`--approval-policy` 和 `--add-dir` 留待单独评估。[`dsh` CLI 帮助](https://github.com/deepseek-ai/deepseek-harness/blob/master/docs/cli-help.md) · [DSH headless 运行语义](https://github.com/deepseek-ai/deepseek-harness/blob/master/packages/bundle/headless/README.md)

## 2. 命令与任务输入

本节定义内置 headless 入口。具名声明和 launcher 的补丁选项见 §7。

```text
dshr --profile headless [--json] [--session-id <id>] [--] [<task>...]
dshr --profile headless --help
```

- `--profile headless` 位于命令开头，也接受 `--profile=headless`。进入 profile 分支后只由该分支解析参数。缺少或非法的 profile 名称、重复选项、缺少选项值和未知选项均作为参数错误退出。任务中的选项字面量放在 `--` 后面。无参数的 `dshr` 启动桌面，原有独立 CLI 命令继续提供各自入口。
- 一个位置参数原样提交，多个位置参数用一个 ASCII 空格连接。显式任务存在时无需读取 stdin。空字符串、纯空白和混用 `-` 与其他任务词都是参数错误。
- 未提供任务且 stdin 是终端时，提示任务用法并立即退出。未提供任务且 stdin 已重定向，或唯一任务词是 `-` 时，从 stdin 读取 UTF-8 至 EOF，保留空白和换行。空输入、纯空白、无效 UTF-8 或读取失败均退出 `1`。
- `-h` / `--help` 将帮助写到 stdout 并退出 `0`。帮助和参数错误均在创建数据目录、加载插件、发起模型请求之前处理。

```sh
dshr --profile headless "用三句话概括这段代码"
dshr --profile headless < prompt.txt > answer.txt
dshr --profile headless --json "总结这份材料"
dshr --profile headless --session-id s-123 "把上次的总结压缩成两点"
```

## 3. 运行配置与工具

- 复用 DSHR 的 provider、模型、重试和上下文配置，沿用 DeepSeek 优先、OpenAI 兼容 provider 次之的选择顺序。显式指定 `DSHR_PROVIDER` 时必须使用指定 provider，未知值或缺少凭据直接报错。没有可用真实 provider 时也报错。Mock 仅在显式选择 `DSHR_PROVIDER=mock` 或现有测试开关 `DSHR_FORCE_MOCK=1` 时启用，两者有冲突时报告配置错误。
- 有效工具工作区为 `DSHR_WORKSPACE` 指定的目录或调用进程的当前目录。解析为实际存在的绝对目录，供会话、工具和 sidecar 共同使用。工作区无效时退出 `1`。数据目录由 `DSHR_HOME` 和现有目录覆盖变量控制，与工作区分别解析。
- 复用已支持的 Rust 工具、DSHR 插件配置和 Node sidecar 能力协商。默认尝试发行包中的官方 DSH Sidecar 或已准备的开发依赖。未显式选择 Sidecar 时，启动失败会在 stderr 提示，本轮继续使用其余可用工具。显式设置 `DSHR_SIDECAR=dsh` 时要求 Sidecar 成功启动。插件配置加载失败时退出 `1`。
- `DSHR_PERMISSION_MODE` 可选 `read-only` 或 `workspace-write`，默认 `workspace-write`，其余值报配置错误。任何需要交互审批的调用立即拒绝，按正常工具错误返回给模型并记录审批结果。Rust 工具和 sidecar 的写服务必须使用同一决定，继承的 `DSHR_APPROVAL_MODE=allow` 不得改变此行为。暂缓支持 `danger-full-access`、通用 shell 执行、会话树新建和外部进程插件自动装载。

正常 headless 入口保留 `probe_*` 前缀给诊断工具。运行时使用官方 Sidecar，显式选择 `probe` 或 `cordis` 会返回 `E_CONFIG`。

显式启用官方 `ask_user_question` 时，调用立即返回需要桌面交互的工具错误。工具不会等待标准输入或在 headless 中生成提问界面。 [提问和取消契约](sidecar-rpc.md#79-官方工具提问)

## 4. 输出、终态与退出

### 4.1 文本模式

成功时，stdout 只写**本次轮次最终提交的助手文本**，以 UTF-8 输出。非空且末尾没有换行时补一个 LF。成功但没有可显示文本时，stdout 为空。启动、工具和错误诊断走 stderr，默认不输出模型 reasoning。发生失败或取消时 stdout 为空，部分回复仍可按日志规则保存。

### 4.2 JSON 模式

`--json` 将 stdout 切换为逐行 UTF-8 JSON，每行一个对象，行以 LF 结束。首版字段如下。客户端可忽略未知字段和事件类型。

| `type` | 字段 | 发出时机 |
|---|---|---|
| `session` | `sessionId` | 首行。新会话已持久化或已取得既有会话的写入权并打开后发出。 |
| `tool_call` | `sessionId`, `callSeq`, `tool` | 宿主接收本轮工具调用时。 |
| `tool_result` | `sessionId`, `callSeq`, `status` | 工具结算时，`status` 为 `completed`、`error` 或 `aborted`。 |
| `final` | `sessionId`, `text`, `reason` | 本轮终态保存成功后发出。`reason.kind` 为 `completed`、`error` 或 `cancelled`。后两种的 `text` 为空字符串。 |
| `error` | `code`, `message`, `sessionId?` | 参数、启动、会话或保存失败时发出。只有已发出 `session` 时才带 `sessionId`。 |

工具事件按运行顺序推送和刷新，调用与结算通过日志中的调用序号 `callSeq` 配对，允许多个调用先发出再结算。工具参数和结果正文可从会话日志查阅。正常可写的输出流以恰好一个 `final` 或 `error` 结束。`--` 前被识别为选项的 `--json` 对参数错误也生效。帮助始终输出文本。

`error.code` 可供脚本分支。首版包含 `E_USAGE`、`E_INPUT`、`E_CONFIG`、`E_RUNTIME`、`E_SESSION_NOT_FOUND`、`E_SESSION_BUSY`、`E_SESSION_WORKSPACE`、`E_SESSION_IO`。诊断中的凭据须脱敏。

### 4.3 退出与清理

| 代码 | 条件 |
|---|---|
| `0` | 帮助输出成功，或本轮完成、终态持久化且输出写入成功。 |
| `1` | 参数、输入、配置、启动、模型、会话、持久化或输出错误，亦用于已处理的取消。 |

轮次结果由显式终态决定。模型继续处理单个工具错误后，仍可正常完成本轮。收到 Ctrl+C 或 Unix SIGTERM 时，使用共享取消令牌停止发起新请求，结算可取消的模型及工具工作，尽可能保存 `cancelled` 终态。正常退出和已处理的取消均释放会话写入权、关闭并回收本次启动的 sidecar。若发现 stdout 已关闭，取消尚未完成的轮次并退出 `1`。已保存的终态保留，即使随后的输出失败。

## 5. 会话与持久化

- 默认在现有会话目录中创建唯一的根会话，写入工作区标识。新会话可由桌面的会话索引和导出工具识别。并行启动必须原子地独占新 ID，遇到冲突时重新分配，禁止覆盖。
- `--session-id` 精确打开同一会话目录内的 DSHR 会话并重建模型历史。对 ID 做文件名校验和目录约束。未知 ID、不可恢复日志、缺少可确认的工作区或工作区不匹配均报错。可复用现有备份恢复，恢复也需取得写入权。
- 从读入可写快照到保存本轮，必须使用跨进程的会话级独占机制。桌面与 CLI 的会话内容写入、恢复和删除须遵守同一规则。目标被占用时，headless 立即报告 `E_SESSION_BUSY`，不排队。不同会话可并行运行。
- 沿用 [Session Log](session-log.md) 的 `turn.started` / `turn.ended` 及原子保存，轮次终态可区分 `completed`、`error`、`cancelled`。最终答案和退出条件只取本轮区间。历史轮次缺少终态事件时可照常回放，工作区可确认且日志可恢复的旧会话可续跑。保存失败必须返回 CLI，禁止发送成功答案或 `final`。

## 6. 验收

Windows 发行包提供 `bin/dshr.exe` 控制台入口，与包根的桌面入口共用应用资源和数据根。脚本从 PATH 解析到控制台入口，或直接调用 `bin/dshr.exe`。无参数调用可打开桌面。Unix 使用发行包的 `dshr`。

首版使用 Mock 或本地 HTTP fixture 和临时数据目录验证。

| ID | 验收 |
|---|---|
| `CLI-INPUT-001` | argv 与 stdin 均可提交 Unicode、多行和含选项字面量的任务，验证保留换行。无任务的 TTY、空输入、参数错误和帮助均不启动运行时。 |
| `CLI-OUTPUT-001` | 捕获 stdout 和 stderr，验证文本无启动前缀。JSON 每行能独立解析，工具调用可对应终态，成功、参数错误、轮次失败与保存失败均有对应的终端记录和退出码。 |
| `CLI-TOOLS-001` | 经已配置 sidecar 运行读工具，写入审批拒绝直接返回并留痕，继承 `DSHR_APPROVAL_MODE=allow` 时也无文件写入。模型清单排除 `probe_*`，伪造调用返回错误。 |
| `CLI-SESSION-001` | 新任务并行创建不同 ID。用同一 ID 续跑能读到旧上下文，仅输出新轮次。错误 ID、不同工作区、桌面或 CLI 持有同会话写入权时，历史保持完整。 |
| `CLI-CANCEL-001` | 在模型或工具执行途中取消，验证保存为 `cancelled`、退出非零、回收 sidecar。分别在运行中与结果写出时关闭管道，验证退出非零且已保存终态可回放。 |
| `CLI-PACKAGE-001` | 在实际 release 包验证管道、重定向、退出码、无显示环境及从任意工作目录解析资源。Windows 覆盖 PowerShell、cmd 和无父控制台的重定向子进程。桌面入口与现有 CLI 路径通过回归检查。 |
| `CLI-PROFILE-001` | 加载未修改的 DSH headless manifest，验证 profile、home 和重复 CLI patch 的优先级、`config` 整体替换及实际工具注册。未知 bundle、未知条目、未知字段和动态表达式报告路径并在启动前失败。 |

## 7. DSH profile 声明兼容

具名 headless profile 可以复用 DSH 的 `package.json` 和受支持的静态补丁。由 DSHR 内置实现承接 `base` 和 `headless` 两个标准 bundle。

### 7.1 上游格式与现状

DSH 在 `$DSH_HOME/profiles/<name>/package.json` 的 `dsh.profile.bundles` 声明有序的包名列表。bundle 在自己的 `package.json` 用 `dsh.bundle.patch` 声明一个包内路径，或多个按顺序应用的路径。从空条目树依次应用 bundle patches、profile 的 `cordis.patch.yml`、home 的 `cordis.patch.yml`，最后应用命令行重复传入的 `--patch <path>`。按 `id` 覆盖时，`config` 整体替换。[`dsh` CLI 行为参考](https://github.com/deepseek-ai/deepseek-harness/blob/master/apps/cli/reference/README.md) · [DSH profile 读取与合并实现](https://github.com/deepseek-ai/deepseek-harness/blob/master/packages/boot/app-boot/src/profile.ts)

本机 `0.2.2-rc.5` 安装包中，`@deepseek-ai/dsh-base` 和 `@deepseek-ai/dsh-headless` 都声明了 `./cordis.patch.yml`。原版 `headless` 通过 `!!js ctx.headlessStartup.*` 取得任务、会话和输出模式，`base` 还声明了 DSH 自己的 Agent、会话与权限服务。DSHR 现有 Loader 能读取静态条目和部分 patch 操作，当前只把结果投影成插件项。表达式会原文保留，patch 插入的 include 缺少文件基准，Cordis 的服务隔离语义也尚未接入。[DSH headless bundle](https://github.com/deepseek-ai/deepseek-harness/blob/master/packages/bundle/headless/cordis.patch.yml) · [DSHR Loader 差异](../m3-loader.md)

### 7.2 DSHR 声明契约

在 DSHR 数据根的 `profiles/<name>/` 读取声明。`dshr --profile <name> [--patch <path>]... [headless-options] [--] [<task>...]` 先消费 launcher 的 `--patch`，然后交给 §2 的 headless 解析器。名称以 ASCII 字母或数字起始，后续可包含字母、数字、下划线、连字符和非尾部的点。保留 Windows 设备名及 `node_modules`。`headless` 没有磁盘声明时使用内置清单，其他名称必须有清单。可将 DSH 的声明文件复制过来，读取范围限于 DSHR 数据根及显式传入的补丁路径。

例如，`profiles/batch-review/package.json` 可直接采用以下 DSH 清单形状。

```json
{
  "private": true,
  "dsh": {
    "profile": {
      "bundles": ["@deepseek-ai/dsh-base", "@deepseek-ai/dsh-headless"]
    }
  }
}
```

- 第一批只接受示例中的两个 bundle，顺序相同。安装包内置对应的 DSHR 静态声明。前者提供可配置的工具条目，后者选择 Rust headless runner。检测到其他 bundle 时，诊断指出包名和所在清单。普通 npm 元数据可以保留，读取清单不会安装依赖或运行 npm scripts。
- 补丁顺序沿用 DSH，home 层取 DSHR 数据根的 `cordis.patch.yml`。profile 和 home 的补丁文件可以缺省。命令行补丁按调用目录解析相对路径，文件必须存在。第一批支持对明确映射的条目按 `id` 设 `disabled` 和整体替换静态 `config`。配置用 YAML 块映射或空映射 `{}` 表示，补丁受 §3 的权限约束。
- 使用具名清单、磁盘 `headless` 清单或声明补丁时，以声明合成结果配置插件。普通内置调用可沿用现有插件配置。声明模式与旧的 `DSHR_LOADER_BUNDLE` / `DSHR_LOADER_PATCH` 显式入口冲突时报告配置错误，避免两个来源同时配置同一条目。
- 缺失自定义清单、未知 bundle、未知条目、未知配置字段，以及 `!!js`、`isolate`、`intercept` 等尚未支持的语义都按 `E_CONFIG` 处理。错误带文件路径和条目 ID（如有），并在会话创建、插件载入和模型请求前返回。成功输出遵循 §4。

| 条目 ID | DSHR 对应项 | `config` 可用字段 |
|---|---|---|
| `tool-fs` | 官方 sidecar 的 `@deepseek-ai/dsh-tool-fs`。条目启用时由 `DSHR_SIDECAR=dsh` 挂载。 | 布尔字段 `read`、`readImage`、`write`、`edit`、`applyPatch`。初始值为 `read: true`、`readImage: false`，写入开关仍由运行时能力和审批约束。 |

例如，在 `profiles/batch-review/cordis.patch.yml` 中禁用已配置的官方文件工具。

```yaml
- id: tool-fs
  disabled: true
```

运行 `dshr --profile batch-review "概括输入内容"` 即选择这份声明。
