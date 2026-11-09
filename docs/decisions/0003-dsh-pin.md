# ADR-0003 — 锁定 DSH 版本与 vendored Cordis

- 状态：**Accepted**（基线数据已采集；升级流程待 M1 验证）
- 日期：2026-11-08
- 关联：PLAN ADR-004 / ADR-008 / §5.1

## 背景

兼容性承诺必须有明确对象。"兼容 DSH"这句话在没有版本号时没有意义。同时 Sidecar 要复用 DSH 的 vendored Cordis、Loader 与 npm 解析，这些内部包的小版本变化就可能改变插件加载语义。

## 采集到的基线（来自本机安装的 DSH 桌面包）

| 项 | 值 |
|---|---|
| 包名 | `@deepseek-ai/dsh-desktop-runtime` |
| 版本 | `0.2.1-rc.3` |
| Host 协议版本 | `4` |
| 打包 Node | `24.21.0` |
| 打包 pnpm | `11.28.5` |
| Cordis（vendored） | `@deepseek-ai/cordis@4.0.5` |
| Cordis 插件 | `plugin-group@1.0.5`、`plugin-include@1.0.10`、`plugin-loader@1.0.6`、`plugin-timer@1.1.7` |
| Schemastery | `@deepseek-ai/schemastery@3.18.5` |
| 并发结构 | 约 300 个 `@deepseek-ai/dsh-*` 包，版本与主包一致 |

> 采样方式：读取安装目录 `resources/app.asar` 中的 `dsh/package.json`。升级前重新采样一次，差异写进本表。

## 决定

1. **兼容目标锁定为 `0.2.1-rc.3` / hostProtocol `4`**，并在仓库内记录完整依赖清单（`docs/spec/` 之外，放 `vendor/dsh-manifest.json` 或其等价物）。
2. Sidecar 使用**同版本**的 vendored Cordis（4.0.5）与 Schemastery（3.18.5），不自行升级。
3. 版本升级只在里程碑边界进行（M2/M3/M4 各一次），每次升级必须跑完整 conformance matrix。
4. 许可证与更新政策：**未完成**。安装包内的 `dsh/package.json` 没有 `license` 字段；仓库存在按包分发的 `LICENSE`（如 `packages/boot/hmr/LICENSE`），需逐包采集后写入 `vendor/dsh-manifest.json`。在此之前**不得**假设 MIT 或任何单一许可证。

## 后果

- **好消息**：兼容性断言可测试（"对 0.2.1-rc.3 的 L1 通过"）；升级变成可计划的事件，而不是随机断裂。
- **坏消息**：DSH 处于 `rc` 阶段，小版本可能带语义变化；我们必须按里程碑冻结，错过一些即时修复。
- 上游已自带桌面宿主（`dsh-desktop-host`、`desktop-runtime.json`），说明"DSH 桌面"是活跃方向——这不是技术风险，而是产品差异风险（见 PLAN §11 与 `docs/product-difference.md`）。

## 反悔条件

- 上游发布 1.0 且 API 稳定 → 改为跟随 minor，放宽 pin。
- 上游 vendored Cordis 大版本升级且不向下兼容 → 重新评估 L1/L2 的实现方式（可能需要在 Sidecar 内保留两套 Cordis，代价很高，需单独立项）。

## 验证

1. `vendor/dsh-manifest.json` 的哈希与安装包一致。
2. L1 conformance 在锁定的版本上全绿；升级前后各跑一次并对比报告。
3. Sidecar 报告的 `dsh_version` 与 host 期望值不一致时，握手失败（见 [`sidecar-rpc.md`](../spec/sidecar-rpc.md) §3）。
