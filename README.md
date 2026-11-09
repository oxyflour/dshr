# Native Agent Desktop — workspace

Rust 是唯一权威运行时；GPUI 是原生渲染底座；DSH 通过 Node Sidecar 兼容；UI 扩展走 ADC-v1。

## 文档地图

| 文件 | 内容 |
|---|---|
| [`docs/PLAN.md`](docs/PLAN.md) | 实施计划：目标、架构决策、里程碑、估算 |
| [`docs/spec/`](docs/spec/README.md) | 设计规范：ADC-v1、CPP-v1、Sidecar RPC、Session Log |
| [`docs/decisions/`](docs/decisions/README.md) | ADR：版本 pin、Cordis 选型、不走 Web 壳 |
| [`docs/product-difference.md`](docs/product-difference.md) | 与上游 DSH 桌面端的差异（待核实假设） |
| [`docs/m0-runbook.md`](docs/m0-runbook.md) | M0 状态、环境事实、复验命令 |
| [`schema/`](schema/README.md) | 契约的 JSON Schema 与校验用例 |

## 目录结构

```text
apps/desktop-gpui        桌面壳入口（占位，UI 原型属 M1 实验 A）
crates/agent-core        Agent Loop / Turn / Step / 取消
crates/agent-context     计量、压缩、裁剪、外部化
crates/session-store     事件日志、回放、迁移
crates/tool-runtime      Tool 注册、执行、Policy
crates/plugin-api        CPP-v1 公开契约
crates/plugin-host       Cordis 适配层（唯一允许依赖 cordis-rs 的 crate）
crates/ui-contributions  ADC-v1 服务端（slot 注册表）
crates/ui-gpui           GPUI 渲染实现
crates/compat-dsh-*      Sidecar 协议与宿主
crates/compat-acp|mcp    外部 Agent / Tool 接入
schema/                  契约 Schema 与 fixture 校验
tools/                   构建环境脚本与 GPUI 编译探针
```

## 快速开始

```powershell
# 契约校验（只需 Node）
node schema/check.mjs
node tools/check-links.mjs

# 构建。Windows 上必须走 build-env：本机 %TEMP% 不能让链接器写入
tools\build-env.cmd cargo build --workspace
```

## 已固定的版本

| 组件 | 版本 | 记录 |
|---|---|---|
| Rust | `1.99.1` (stable) | [`ADR-0001`](docs/decisions/0001-gpui-pin.md) |
| GPUI | zed `d2a4fb6` | 同上 |
| GPUI Kit | `dbb0b9a`（0.9.0） | 同上 |
| Rust Cordis | `jincdream/cordis-rs` | [`ADR-0002`](docs/decisions/0002-cordis-rs-selection.md) |
| DSH | `0.2.1-rc.3` / Cordis `4.0.5` | [`ADR-0003`](docs/decisions/0003-dsh-pin.md)（license 待补） |

## 当前状态

**M0：4 项完成、1 项部分、1 项转入 M1。** 工具链已就绪，`cargo build --workspace` 通过，契约校验 20/20、文档互链 0 broken。剩余项见 [`docs/m0-runbook.md`](docs/m0-runbook.md) §5。

**M1 进行中。** A1 虚拟化通过（10,000 行只构造 24 行）；A1b 已拆解启动开销：到首帧 1.0–1.15 s（应用初始化 0.62–0.76 s + 窗口创建 0.33–0.41 s），与行数/文本/CJK 无关；连续出帧下四档 p50 均为 31.2 ms，说明当前负载低于出帧节拍。实验 B（Cordis 选型）已完成；A2（IME）、A3（WebView2）、C（Sidecar 成本）未开始。数据与复现命令见 [`docs/m1-log.md`](docs/m1-log.md)。
