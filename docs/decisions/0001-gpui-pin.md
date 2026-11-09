# ADR-0001 — 固定 GPUI 与 GPUI Kit 的精确版本

- 状态：**Accepted**（2026-11-09 编译验证通过）
- 日期：2026-11-08（验证于 2026-11-09）
- 关联：PLAN ADR-003 / ADR-008 / §11 风险表
- 验证载体：`_probe/gpui-probe`（独立于 workspace，随仓库保留以便复验）

## 背景

GPUI 与 GPUI Kit 都处于快速演进期，`gpui-shell` 等能力仍标为实验路径。桌面应用对渲染层的破坏性变更极其敏感：一次 API 改名意味着几十个 View 的返工。同时 GPUI 没有稳定的发布节奏承诺，跟随 `main` 等于把构建稳定性交给上游。

## 选项

| 选项 | 优点 | 缺点 |
|---|---|---|
| **A. 跟随 `main`** | 总能拿到修复 | 构建随机失败；无法复现历史问题 |
| **B. 固定精确 commit（crates.io 或 git rev）** | 可复现、可打包、可 bisect | 修复需手动 cherry-pick |
| **C. Fork 并长期维护** | 完全可控 | 长期维护成本高，失去上游修复 |
| **D. 等 1.0 再开始** | 风险最低 | 项目无限期停摆 |

## 决定

采用 **B**，并在下列情形升级为 **C 的最小形态**（只 fork、只打补丁、定期 rebase）：

1. 上游存在阻塞性 bug 且短期无修复；
2. 上游做出破坏性变更而我们暂时无法迁移；
3. 需要为 Windows 特定行为（IME、DPI、WebView 叠层）打局部补丁。

## 记录字段（已填）

| 字段 | 值 |
|---|---|
| GPUI commit | `d2a4fb6ded7c470be25d1541281da3882495b65b`（zed HEAD @ 2026-11-09） |
| GPUI Kit commit | `dbb0b9a4895c4b45753ed8647cb0dbb58178bd96`（gpui-kit `0.9.0`，依赖 gpui-base `0.9.0`） |
| Rust toolchain | `stable` = `1.99.1 (58cf8f9c3 2026-10-13)`，`x86_64-pc-windows-msvc` |
| `cargo` / `rustc` | `cargo 1.99.1 (5f94df478 2026-08-27)` / `rustc 1.99.1 (58cf8f9c3 2026-10-13)` |
| 原生工具链 | VS 2022 Community，MSVC `14.44.35207`，Windows SDK `10.0.26100.0` |
| 目标三元组 | `x86_64-pc-windows-msvc` |
| 首次可构建日期 | 2026-11-09 |
| 冷构建（`cargo check`，含两个 git 依赖全部编译） | **352 s**（5m52s） |
| workspace 构建（无外部依赖） | 1.1 s |

记录位置：`rust-toolchain.toml`、根 `Cargo.toml`、`_probe/gpui-probe/Cargo.toml`。

> `rust-toolchain.toml` 当前写的是 `stable`。M1 开始前应改为确切版本号（`1.99.1`），否则半年后重建会拿到不同的编译器。

## 实测证据

```
gpui-probe cargo check   → Finished dev profile in 5m 52s, exit 0
  gpui       (zed d2a4fb6)
  gpui-kit   (gpui-kit dbb0b9a, v0.9.0)
  gpui-base  v0.9.0, accesskit_windows v0.34.0, gpui_pre_platform 0.3.11
```

结论：LLVM/MSVC 工具链、WebView 之外的 GPUI 依赖在本机可完整编译，不需要额外的系统组件（Vulkan SDK 虽在 PATH 中，但并非 gpui 的必需项）。

## 后果

- **好消息**：构建可复现；M1 的性能基线（帧率、冷启动）有意义；出问题能定位到确切版本。
- **坏消息**：安全/崩溃修复要人工跟进；每次升级是一次小型迁移项目（预计 2–5 日/季度）。

## 反悔条件

- 上游连续两个季度出现无法 cherry-pick 的破坏性变更 → 转为长期 fork。
- Windows 上的阻塞问题一年内未修复 → 重新评估渲染层（见 ADR-0005 的折中方案）。

## 验证

1. **已通过**：干净机器冷 `cargo check` 352 s；`cargo build --workspace` 1.1 s。
2. 待做：CI 与本地产出相同二进制哈希（除时间戳）。
3. 待做：升级演练，记录一次从 pin 到下一版的完整耗时。

> 注意：M0 的 10 分钟门槛针对的是**本项目 workspace**，不是两个上游 git 依赖的首次编译。首次拉取 GPUI 需要 6 分钟以上，CI 应缓存 `~/.cargo/git` 与 `target`。
