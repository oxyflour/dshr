# ADR-0008 — Script UI 5 日 spike：时间盒、判定标准与退路

- 状态：Accepted
- 日期：2026-11-12
- 关联：`PLAN.md` §4.2 B / §8 M3、[`../review-m3-plan.md`](../review-m3-plan.md) §2 第 3 条、[`0007-m3-scope.md`](0007-m3-scope.md)

## 背景

PLAN §4.2 B 把 Script UI（gpui-shell / QuickJS 脚本扩展）列为第三方动态扩展的优先方向，
但 M1 只完成了 WebView（A3）实验，**Script UI 没有任何实验记录**；产品 crate 不依赖
`gpui-kit`，`gpui-shell` 在仓库内无命中。M3 原清单把它写成"适配方式定案，或明确替代方案"，
这是决策不是交付，无法作为 gate；若直接实做（10–20 日）会挤掉 ADC 渲染器。

## 选项

| 选项 | 内容 | 结论 |
|---|---|---|
| A. M3 实做 Script UI | 直接接 `gpui-shell`/QuickJS，作为第三方动态 UI 的必达路径 | 否决：上游是实验路径（PLAN §14），无本仓库验证；失败会同时拖垮 ADC |
| B. M3 完全不做，直接定 Native+WebView | 省 10–20 日，但失去"轻量动态 UI"选项的证据 | 否决：没有数据就定死路线，违反"先判死再建设"（PLAN §12） |
| **C. 5 日时间盒 spike + ADR** | 只做探针与三演示，产出"选/不选/退路"结论；不作 M3 必达 | **采用** |

## 决定

1. **时间盒 5 个工程日**，独立探针目录（`tools/script-ui-probe/`，不进入产品依赖图）。
2. 判定演示（三个都要，缺一即不通过）：
   - **动态加载/卸载**：运行中从脚本文件装载一个 View、卸载后无残留注册/订阅；
   - **错误隔离**：脚本抛错/死循环/内存增长时宿主不崩、可强制卸载并给出诊断；
   - **元素树渲染**：脚本提交 ADC-v1 元素树（至少 `Stack`/`Text`/`Button`）并被 GPUI 渲染器正确显示、动作回传。
3. 判定标准另加：与现有 `ui-gpui` 不产生 GPUI 版本冲突；二进制增量 ≤ 8 MB；启动冷加载 ≤ 200 ms（本机 debug 记录即可，不做产品承诺）。
4. 产出 `docs/decisions/0008` 的结论增补（或新 ADR）：**选**（进入 M4 实做，M3 不承诺）、**不选**（走 Native + WebView，元素集不再对外承诺脚本路径）、**退路**（以 WebView 独立 Panel 承载动态 UI）。
5. 无论结论如何，**M3 的 ADC 元素集与渲染器不等待 Script UI**；ADC 冻结以 Native 渲染器为准。

## 后果

**好消息**：最高不确定性的路线在 M3 头两周被判死，不会拖到后期；ADC 主路径不受影响。

**坏消息**：若 spike 不通过，第三方动态 UI 只剩 Native（需重编译）与 WebView（重）两条路，
"轻量脚本插件"卖点推迟到 M4 之后。

## 反悔条件

- 上游 `gpui-shell` 在 spike 期间发布可用版本且三演示通过 → 结论改为"选"，M4 排期实做；
- spike 发现 QuickJS 与 GPUI 主线程模型根本冲突 → 直接走 WebView 退路，不再投入；
- 若 WebView 依赖决策同时失败（HTML Widget 移 M4）→ 第三方动态 UI 在 M3 明确不承诺。

## 验证

- spike 目录内 README 记录环境、命令、三演示的录屏/日志与结论；
- ADR 增补在 M3 W2 结束前落盘；`docs/m3-status.md` 更新结论；
- 判定为"不选"时，PLAN §4.2 B 与 §13「UI Script Runtime」行同步改口径。
