# 桌面运行组件

基础发行包包含当前平台的 Node、官方 DSH Sidecar 入口和已解析的 Node 依赖。缺少任一项时打包失败。应用按现有路径规则找到包内 Node，运行文件工具和已构建的插件无需用户安装开发工具。

Windows x64 基础归档在依赖 Visual C++ 运行库的可执行文件同级目录提供 `vcruntime140.dll`。打包从构建机 Visual Studio 的 x64 可再发行目录取得该文件，缺失时失败。应用使用随包副本启动，安装可选组件前无需安装系统级 Visual C++ 运行库。

## 可选组件

Windows x64 基础包从随包的 `runtime/components.json` 读取发行方目录。目录使用 `schema_version: 1`，包含 `platform: "windows-x64"` 和 `components`，组件 ID 为 `python`、`creator-build`。各项记录不可变版本、ZIP 文件名、字节数、SHA-256 和可选 HTTPS 下载地址。ZIP 文件统一从随包目录指向的版本取用，可在应用目录的 `runtime-packages/` 提供相同文件用于离线安装。工作区和模型工具参数无法更改安装源。[组件目录 Schema](../../schema/runtime-components/catalog.schema.json)

用户从桌面「设置 → 运行组件」发起安装。缺少编译器的创造空会话也可打开这一面板。应用后台下载或读取离线文件到临时目录，检查大小、SHA-256、包内路径和必要文件，再发布到应用数据根的 `runtime-components/<id>/<version>/`。同版本安装可重试；并发安装由该版本的锁协调，失败清理临时目录并保留既有版本。应用通过绝对路径使用已安装组件。命令行 `dshr --install-runtime-component <id>` 使用同一安装逻辑。应用范围的安装不会更改系统 PATH。目录缺失、下载错误、损坏包和平台不符给出可读错误，常规对话和文件工具继续可用。

| 组件 | 发布内容 | 使用条件 |
|---|---|---|
| Python | 官方 CPython 嵌入版，按 DSH `primary-runtime` 布局打包；首版只承诺解释器和标准库 | 默认来源可用时向模型提供 `load_workspace_dependencies`，保留显式配置的外部 DSH 来源。安装后下一次模型请求可见 |
| 创造模式构建 | 固定版本的 Windows GNU Cargo、rustc、标准库、链接工具和已固定的 SDK 依赖源码 | `creator_build_plugin` 优先使用已配置的显式工具链，否则使用已安装的构建组件。组件支持使用随包依赖的 Rust 外部进程插件 |

运行组件的安装与插件构建、试用的审批相互独立。发行包提供构建组件且找不到 Cargo 时，`creator_inspect` 指向应用内安装入口。构建使用现有离线策略。新增第三方依赖超出组件随附范围时返回 Cargo 的依赖错误。

## 验收

归档自检从只含 OS 路径的环境启动包内 Node。`M4-RUNTIME-001` 覆盖正常安装、坏哈希、非法条目、重复安装和缺项。`M4-RUNTIME-002` 从干净 Windows 虚拟机开始，检查 Node、Python、Cargo 和 DeepSeek Harness 的预装状态，随后验证基础包的文件工具。通过应用安装可选组件后，验证 Python 真实运行及真实模型依次写入、构建、试用并调用 Hello World 工具。验收保留虚拟机基线、组件版本与哈希和会话轨迹。[验收步骤与记录](../plan/runtime-components.md)
