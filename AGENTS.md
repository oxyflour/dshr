# AGENTS.md

## 文档规则

- [总路线图](docs/PLAN.md) 记录项目目标与里程碑。小修复直接实现和验证。新文档用小写短横线命名，ADR 和问题记录沿用编号。
- `docs/proposal/<topic>.md` 用于方向待定的重大变更，记录问题、范围、备选方案和取舍。定案后原地更新状态并链接规范或 ADR。
- [docs/spec/](docs/spec/README.md) 记录公开契约。改动 API、协议、CLI 行为或兼容性前，先更新对应规范，写清行为、失败条件和验收要求。同步规范索引及受影响的 Schema 和契约测试。
- `docs/plan/<topic>.md` 用于需分阶段或跨模块交付的变更，记录任务、依赖、验收命令和进度。方向明确时可直接写规范或计划。完成或取消后原地更新状态，记录验收结果或取消原因。
- [docs/decisions/](docs/decisions/README.md) 记录架构决定，单个架构议题可以直接写 `Proposed` ADR。[docs/issues/](docs/issues/README.md) 记录已确认、暂缓修复的问题，修复后原地更新状态。
- 交付时把使用方法补进指南。按需把旧文档中的有效条款并入规范或指南，并在原文补链接。现有文件沿用原路径，确需移动时同步更新引用。文档变更运行 `node tools/check-links.mjs`。
