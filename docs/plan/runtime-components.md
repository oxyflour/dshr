# 运行组件分发实施计划

[运行组件规范](../spec/runtime-components.md) · [创造模式](../spec/creator-mode.md)

| 阶段 | 工作 | 验收 |
|---|---|---|
| 1. 基础包 | 把 Node、官方 Sidecar 依赖和 Windows 应用本地的 VC++ 运行库列为发布必检项 | 打包与归档校验拒绝缺件 |
| 2. 构建组件包 | 从固定来源生成 Python 及 Windows GNU Rust、离线 SDK 依赖 | 组件目录、摘要和应用安装验证 |
| 3. 接入应用 | 设置中的安装入口、离线文件与 HTTPS 下载、运行时能力门控 | 失败可重试。安装后 Python 工具和 Rust 构建可用 |
| 4. 干净虚拟机 | 从空 Windows 环境安装并跑真实模型全流程 | 记录基线环境、组件哈希、工具调用和最终结果 |

## 进度

- 2026-11-21 已授权本机账号使用 Hyper-V。重启后通过经典 Windows Sandbox 启动了干净来宾。
- 2026-11-21 Windows 11 专业版 build 26200 主机隔离验收通过。Windows x64 release 内置 Node `24.21.0`。`verify-package.ps1` 通过窗口、官方 Sidecar 和三项 CPP 自检。桌面单测 34 项通过，两项需实装 Sidecar 的预设重载测试另行通过。
- 2026-11-21 首轮干净来宾确认 Node、Python、Cargo 和 DSH 均未预装，验收程序启动返回 `0xC0000135`。PE 导入表确认桌面、CLI 与验收程序需要 `vcruntime140.dll`。Windows 打包已改为在两个可执行文件目录提供同一份 Visual C++ 可再发行库。
- 2026-11-21 修订发行包通过主机归档自检和 `M4-RUNTIME-002` 干净来宾验收。

## 主机隔离验收记录

| 产物 | 版本 | 大小（字节） | SHA-256 |
|---|---|---:|---|
| Windows x64 基础归档 | release | 56212277 | `edaebcdc1eb640584131ea183504689f5bd2e10a84070ca53524f7bbe778ef95` |
| Python | `3.13.12` | 10944090 | `47e573400a1728c8cd1fd8b88134e0e5a60e4b10963be34488f6bc0d1b5316bc` |
| 创造模式构建 | `1.93.0-sdk.1` | 230133588 | `0a5aa153f050118a17003595a5fbb5b361d722764ee8e8acfdda1d3eb49318c5` |

进程只保留 Windows 系统 PATH，并使用空白应用数据根。安装前官方 `dsh-tool-fs@0.2.2-rc.1` 的 `read` 成功。应用通过 `--install-runtime-component` 读取离线发行包。安装后，当前会话的 `dsh-tool-workspace-dependencies@0.2.2-rc.1` 返回组件内的解释器路径，解释器导入 `json` 并输出版本 `[3, 13, 12]`。

真实 `deepseek-chat` 会话 `s-1795265634800` 的工具结果依次为 `creator_inspect`（6）、三次 `creator_write_file`（9、10、12）、`creator_build_plugin`（14）、`creator_try_plugin`（16）、`hello_world`（18）。最后一次工具调用返回 `Hello, Cursor!`。审批测试通过产品的构建和试用入口。测试日志与轨迹存于本地 `.tmp/runtime-component-evidence/host-final/`，验收输入位于 `.tmp/runtime-component-guest/input/`。

## Windows 来宾验收

先按[桌面打包指南](../m4-package.md#1-命令)生成发行归档、SHA-256 文件及两个运行组件 ZIP。用 `tools\build-env.cmd cargo test -p dshr-desktop-gpui --bin dshr --no-run` 构建测试入口，取 Cargo 报告的 `target\debug\deps\dshr-<hash>.exe`。把产物和 `tools/verify-clean-windows.ps1` 复制到干净虚拟机。测试入口只随验收资料交付。

向测试进程提供 `DEEPSEEK_API_KEY`，或用 `-DeepSeekEnv` 指向来宾可读取的凭据文件。运行以下命令。

~~~powershell
.\verify-clean-windows.ps1 -Archive .\dshr-release-windows-x64.zip -RuntimePackages .\runtime-packages -AcceptanceExe .\dshr-test.exe -Evidence .\evidence
~~~

保留 `baseline.txt`、`components.json`、`creator.stdout.txt` 和会话 JSONL。通过时记录 `OFFICIAL_READ_BEFORE_INSTALL_PASS`、`OFFICIAL_PYTHON_PASS` 及 `LIVE_CREATOR_PASS`。可用 `-HostSmoke` 在 Windows 开发主机补测，输出前缀为 `HOST_SMOKE`。

### 干净来宾验收结果

Windows Sandbox 来宾为 Windows 11 企业版 build 26100，机器型号为 `Virtual Machine`。基线检查确认 Node、Python、Cargo、Rust 和 DeepSeek Harness 均未预装，`System32` 也没有 `vcruntime140.dll`。

验收使用补齐应用本地运行库的 release 归档，大小为 56337017 字节，SHA-256 为 `fbd75efd6c7d883fa52b35885b096d40c74a684ce8cb837171a2e87caa4012a4`。内置 Node `24.21.0` 和 Microsoft Visual C++ 运行库 `14.44.35211.0`。Python `3.13.12` 与创造模式构建组件 `1.93.0-sdk.1` 的哈希与上表相同，通过应用 CLI 从配套离线包安装。

控制台帮助命令成功启动。官方 `dsh-tool-fs@0.2.2-rc.1` 的 `read` 在安装组件前已成功，安装后官方 `dsh-tool-workspace-dependencies@0.2.2-rc.1` 返回的解释器成功导入 `json`。真实 `deepseek-chat` 通过主机临时转发服务完成请求，服务凭据保留在主机。

会话 `s-1795268805046` 从空工作区执行 `creator_inspect`（6）、三次 `creator_write_file`（10、11、12）、`creator_build_plugin`（14）、`creator_try_plugin`（16），随后调用 `hello_world`（18），返回 `Hello, Cordis!`。验收状态为 `CLEAN_WINDOWS_PASS`，本地基线、组件清单、控制台输出和会话轨迹存于 `.tmp/runtime-component-guest/clean-crt/evidence/`。
