# M0 执行状态与运行手册

> 目标（PLAN §8 M0）：产出可复现基线、五份 ADR、三份契约 Schema、一个能跑起来的 Windows 构建。
> 最后更新：2026-11-09。本文件记录**环境事实、当前状态、复验命令**，避免下一个人重新摸索。

## 1. 环境事实（2026-11-09 实测）

| 项 | 值 | 备注 |
|---|---|---|
| Rust | `1.99.1 (58cf8f9c3 2026-10-13)`，minimal profile | 装在 `%USERPROFILE%\.cargo`，**未修改 PATH** |
| 原生工具链 | VS 2022 Community，MSVC `14.44.35207`，Win SDK `10.0.26100.0` | `link.exe` 可用 |
| **`%TEMP%` 限制** | **子进程不可写** | 链接器报 `LNK1104 ...\Temp\lnk{GUID}.tmp`；`cc` 报 `D8050`。用 `tools/build-env.cmd` 把 `TMP`/`TEMP` 指到 `.tmp/` 解决 |
| Node / pnpm | `24.14.0` / `10.32.1` | `schema/check.mjs` 与 `tools/check-links.mjs` 依赖 Node |
| 本机 DSH | `0.2.1-rc.3`，hostProtocol `4`，Node `24.21.0`，pnpm `11.28.5`，Cordis `4.0.5`，Schemastery `3.18.5` | 采自 `resources/app.asar` 的 `dsh/package.json` |
| 网络 | PowerShell 可达 `github.com`、`static.rust-lang.org`、`crates.io` | `web_fetch` 工具因 DNS 映射不可用；`winget` 未安装 |
| 版本控制 | 非 git 仓库 | 需要时 `git init` 后再提交 |

## 2. M0 任务状态

| # | 任务 | 状态 | 证据 |
|---|---|---|---|
| 1 | 固定 GPUI / GPUI Kit 与 toolchain | **完成** | GPUI `d2a4fb6`、GPUI Kit `dbb0b9a`（0.9.0）；冷 `cargo check` 352 s 通过 → ADR-0001 |
| 2 | 实测两个 cordis-rs 候选 | **完成** | dshbox 167 passed / jincdream 111 passed；选定 jincdream → ADR-0002 |
| 3 | 锁定 DSH 与 vendored Cordis | **部分完成** | 版本已采集；license 尚未逐包采集（安装包无 license 字段） |
| 4 | 三份首版 Schema | **完成** | 4 份 schema + 20 条正反用例，`node schema/check.mjs` 全绿 |
| 5 | ADR-009 + 与上游差异一页纸 | **完成** | ADR-0005、`docs/product-difference.md` |
| 6 | GPUI 极简 UI 原型 | **未开始（转入 M1 实验 A）** | 只做到依赖级编译验证；窗口/布局/IME 属 M1 判死实验 |

**结论：M0 完成 4 项、部分 1 项、未开始 1 项。** 阻塞已全部解除（工具链装好、依赖可拉取），剩下的 6 号是 M1 的实质工作，不是在 M0 能顺手补的。

## 3. 复验命令

```powershell
# 1) 构建（必须走 build-env，避免 %TEMP% 链接失败）
tools\build-env.cmd cargo build --workspace

# 2) 契约与文档校验
node schema/check.mjs          # 20/20
node tools/check-links.mjs     # 0 broken

# 3) GPUI 依赖复验（耗时约 6 分钟，需缓存或耐心）
tools\build-env.cmd cargo check --manifest-path tools\gpui-probe\Cargo.toml
```

## 4. M0 退出条件核对

| 条件 | 状态 |
|---|---|
| 干净环境 `cargo build --workspace` < 10 分钟 | **通过**（无外部依赖时 1.1 s；首次拉 GPUI 另计 352 s，见 ADR-0001） |
| Schema 有文件 + 可执行校验 | **通过**（20 用例） |
| 五份 ADR 无占位 | **4/5**：0001/0002/0004/0005 已 Accepted；0003 缺 license |
| PLAN 与实际一致 | **通过**（PLAN §3.5 已同步为已选定实现） |

## 5. 剩余工作（进入 M1 前）

1. **license 采集**：DSH 安装包不带 license 字段，需逐包采集（仓库内存在按包分布的 `LICENSE`，例如 `packages/boot/hmr/LICENSE`），完成后把 ADR-0003 转 Accepted。
2. **GPUI 原型**：并入 M1 实验 A（10k 事件虚拟化、中文 IME、WebView2 叠层），不要单独做"好看的原型"。
3. **`plugin-host` 接入 jincdream**：把 `cordis-rs` 作为唯一依赖引入 `plugin-host`，同时把它的 111 个测试作为 conformance 基线的一部分。
4. **`rust-toolchain.toml` 收紧**：`stable` → `1.99.1`，否则半年后重建会换编译器。
5. **git init**（可选，但 M1 之前建议做）。

## 6. 已知风险

- **`%TEMP%` 坑会反复出现**：任何直接 `cargo build`（不经 `build-env.cmd`）都可能在链接阶段随机失败；CI 必须设置 `TMP`/`TEMP`。
- **`_probe/` 已删除**：两个 cordis 克隆约 4 GB，测试证据已写进 ADR-0002，需要复现时重新 `git clone` 即可。
- **jincdream 是单维护者的一次性实现**：反悔条件见 ADR-0002；`plugin-host` 的隔离是换实现的唯一保险。
- **DSH 是 rc 版本**：`0.2.1-rc.3` 之后可能有不兼容变化，升级窗口按 ADR-0003。
