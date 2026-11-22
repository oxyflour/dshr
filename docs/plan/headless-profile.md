# Headless profile 实施计划

> 首版已实现 · Windows 与 Linux 验收通过 · macOS 待 CI 验证。契约见 [Headless CLI 规范](../spec/headless-profile.md)。

## 交付顺序

| 阶段 | 工作与依赖 | 验收 |
|---|---|---|
| 1. 启动与配置 | 在桌面初始化前分流。Windows 包内提供控制台入口。为内置和具名 headless profile 合并静态声明，复用 Loader 的解析。 | `CLI-INPUT-001`、`CLI-PROFILE-001`、`CLI-PACKAGE-001` |
| 2. 单轮运行 | 复用 Agent、provider、工具链和上下文。CLI 订阅结构化事件，桌面从同一轮次生成快照。保存失败向上返回，非交互审批采用专用 broker 配置。 | `CLI-OUTPUT-001`、`CLI-TOOLS-001`、`CLI-CANCEL-001` |
| 3. 共享会话 | 引入会话级跨进程锁，协调桌面写入、恢复和删除。CLI 在锁内读取、验证工作区、续跑并保存。 | `CLI-SESSION-001` |
| 4. 交付 | 通过现有 CLI 与桌面回归、release 包冒烟和链接检查，更新用户入口指南。 | [规范 §6](../spec/headless-profile.md#6-验收) |

CLI 和桌面共享轮次运行与持久化。Windows 包随桌面程序构建控制台启动器，转发标准流和取消信号。桌面在取得写入权后刷新会话，CLI 在同一把锁下校验工作区并续跑。

第三方声明型 bundle 的后续工作包括包解析顺序和相对文件路径。原版应用 bundle 还需要对接 Cordis 服务图、表达式求值和原版 Agent 运行时。两项均在这次交付范围之外。

## 验收命令

```powershell
node tools/check-links.mjs
tools\build-env.cmd cargo test -p dshr-profile -p dshr-session-store -p dshr-tool-runtime
tools\build-env.cmd cargo test -p dshr-desktop-gpui --bin dshr
tools\build-env.cmd cargo test -p dshr-desktop-gpui --test headless_cli
pwsh -File tools/package-desktop.ps1 -Configuration release
```

在本地 HTTP fixture 或显式 Mock 下运行 CLI 契约用例。发行包验收从包目录外执行并捕获 stdin、stdout、stderr 和退出码。Windows 覆盖 PowerShell 和 cmd，Linux 与 macOS 覆盖 Unix shell。

Linux 无显示环境执行：

```sh
env -u DISPLAY -u WAYLAND_DISPLAY -u XDG_RUNTIME_DIR cargo test -p dshr-desktop-gpui --test headless_cli
```

## 进度

- 2026-11-21 确定首版采用静态 DSH profile 声明，梳理规范索引并建立跨模块实施计划。
- 2026-11-21 Windows 通过 16 项桌面单元测试和 9 项 CLI 契约测试。profile、session-store 和 tool-runtime 单元测试分别通过 81、42 和 31 项。
- 2026-11-21 从 Windows release ZIP 解压后，验证 SHA-256、PowerShell 管道、cmd 重定向、并发建会话、同 ID 续跑和 Ctrl+C。使用本地模型 fixture 调用官方 `read`，确认禁用 `tool-fs` 后注册表中没有 `read`，退出后无包内 Node 子进程。原有 `--selftest`、`--selftest-stream`、`--selftest-recovery` 和 `--selftest-tree` 通过。
- 2026-11-21 WSL Ubuntu 清除显示环境变量后通过 9 项 CLI 契约测试，包含 SIGTERM 和写入失败。三平台 CI 已接入契约测试，macOS 结果待 CI。
