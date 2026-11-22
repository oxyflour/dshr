use super::*;

/// 一轮内的可选预算（默认**不限制**，对齐官方 DSH）。
///
/// 官方 DSH 0.2.2-rc.1 的 `@deepseek-ai/dsh-agent-loop` 只声明
/// `maxParallelToolCalls`（默认 10），**没有**每轮工具调用总数或 step 上限：
/// 一个 step 里模型发多少 `tool_calls` 就分批全执行，循环由模型自己收敛。
/// dshr 早期用 `MAX_TOOL_CALLS = 8` 直接 `TurnFailed`，把"一步并发 9 个调用"
/// 这类正常场景判成整轮失败；现在默认放开，预算只保留为**可选**的兜底：
///
/// - [`ToolBudget::max_tool_calls`]：一轮内最多真正执行的工具调用数。超出的调用
///   **不执行**，但事件流仍然发 `ToolCall` + `is_error` 的 `ToolResult`（降级而非
///   失败），并记入本轮的 `tool` 消息；本步剩余调用逐条标记失败后结束该步，然后
///   直接 `TurnEnded`（不再给模型继续发工具调用的机会，避免预算外的"空转步"无限
///   循环）。
/// - [`ToolBudget::max_steps`]：一轮内最多 step 数。命中后不再发起新的模型请求，
///   直接 `TurnEnded`（同样不是 `TurnFailed`）。
///
/// 两者都可以用环境变量显式收紧（`DSHR_MAX_TOOL_CALLS` / `DSHR_MAX_STEPS`）；
/// 空值、`0`、非法值都表示"不限制"，与 DSH 默认一致。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ToolBudget {
    pub max_tool_calls: Option<usize>,
    pub max_steps: Option<usize>,
}

/// 解析预算环境变量的值：空/`0`/非法值都返回 `None`（即不限制）。
pub fn parse_positive_limit(raw: &str) -> Option<usize> {
    let raw = raw.trim();
    if raw.is_empty() {
        return None;
    }
    raw.parse::<usize>().ok().filter(|limit| *limit > 0)
}

impl ToolBudget {
    /// 从 `DSHR_MAX_TOOL_CALLS` / `DSHR_MAX_STEPS` 读取可选预算。
    pub fn from_env() -> Self {
        Self {
            max_tool_calls: parse_budget_env("DSHR_MAX_TOOL_CALLS"),
            max_steps: parse_budget_env("DSHR_MAX_STEPS"),
        }
    }
}

pub(super) fn parse_budget_env(name: &str) -> Option<usize> {
    std::env::var(name)
        .ok()
        .and_then(|value| parse_positive_limit(&value))
}

/// DSH `maxParallelToolCalls` 的默认值（`@deepseek-ai/dsh-agent-loop` 0.2.2-rc.1）。
pub const DEFAULT_MAX_PARALLEL_TOOL_CALLS: usize = 10;

/// 并行上限的环境变量（空/`0`/非法 → 默认 10，与 DSH 一致）。
pub const MAX_PARALLEL_TOOL_CALLS_ENV: &str = "DSHR_MAX_PARALLEL_TOOL_CALLS";

// ---------------------------------------------------------------------------
// DSH 默认差异（M3）：reasoning effort / 重复工具调用软提醒
// ---------------------------------------------------------------------------

/// 重复工具调用的默认阈值（DSH `repeat-tool-reminder.thresholds` = `[3, 5, 8]`）。
pub const REPEAT_TOOL_REMINDER_THRESHOLDS: [usize; 3] = [3, 5, 8];
/// 开关（`on`/`off`）。
pub const REPEAT_TOOL_REMINDER_CONFIG_ENV: &str = "DSHR_REPEAT_TOOL_REMINDERS";
/// 自定义阈值（逗号分隔，如 `3,5,8`）。
pub const REPEAT_TOOL_REMINDER_THRESHOLDS_ENV: &str = "DSHR_REPEAT_TOOL_REMINDER_THRESHOLDS";

/// 重复工具调用软提醒配置（默认开启，阈值 `[3,5,8]`，与 DSH 一致）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepeatToolReminderConfig {
    pub enabled: bool,
    /// 触发提醒的“第 N 次”计数；升序、去重、只保留正数。
    pub thresholds: Vec<usize>,
}

impl Default for RepeatToolReminderConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            thresholds: REPEAT_TOOL_REMINDER_THRESHOLDS.to_vec(),
        }
    }
}

impl RepeatToolReminderConfig {
    /// 从 env 读取：`DSHR_REPEAT_TOOL_REMINDERS=off` 关闭；
    /// `DSHR_REPEAT_TOOL_REMINDER_THRESHOLDS=3,5,8` 覆盖阈值（非法值回退默认）。
    pub fn from_env() -> Self {
        let enabled = match std::env::var(REPEAT_TOOL_REMINDER_CONFIG_ENV) {
            Ok(raw) => !matches!(
                raw.trim().to_ascii_lowercase().as_str(),
                "0" | "off" | "false" | "no" | "disabled"
            ),
            Err(_) => true,
        };
        let thresholds = std::env::var(REPEAT_TOOL_REMINDER_THRESHOLDS_ENV)
            .ok()
            .and_then(|raw| parse_repeat_thresholds(&raw))
            .unwrap_or_else(|| REPEAT_TOOL_REMINDER_THRESHOLDS.to_vec());
        Self {
            enabled,
            thresholds,
        }
    }

    /// 关闭提醒（测试 / 需要完全静默的调用方）。
    pub fn disabled() -> Self {
        Self {
            enabled: false,
            thresholds: Vec::new(),
        }
    }

    /// 自定义阈值：过滤 0、去重、升序。
    pub fn with_thresholds(mut self, thresholds: impl IntoIterator<Item = usize>) -> Self {
        let mut values: Vec<usize> = thresholds.into_iter().filter(|count| *count > 0).collect();
        values.sort_unstable();
        values.dedup();
        self.thresholds = values;
        self
    }
}

/// 解析逗号分隔阈值；任意一项非法即整体非法（返回 `None`，调用方回退默认）。
pub fn parse_repeat_thresholds(raw: &str) -> Option<Vec<usize>> {
    let mut values = Vec::new();
    for part in raw.split(',') {
        let part = part.trim();
        if part.is_empty() {
            continue;
        }
        let count = part.parse::<usize>().ok().filter(|count| *count > 0)?;
        values.push(count);
    }
    if values.is_empty() {
        return None;
    }
    values.sort_unstable();
    values.dedup();
    Some(values)
}

/// 一次触发的软提醒（供宿主记录 `tool.repeat` 诊断/日志）。
#[derive(Debug, Clone, PartialEq)]
pub struct RepeatToolReminder {
    pub tool: String,
    pub arguments: Value,
    /// 本轮内该（工具 + 参数）出现的第几次。
    pub count: usize,
    /// 命中的阈值（与 `count` 相同；显式保留便于日志核对）。
    pub threshold: usize,
    /// 注入模型的提醒文本。
    pub text: String,
}

impl RepeatToolReminder {
    pub(super) fn new(tool: &str, arguments: &Value, count: usize) -> Self {
        let text = format!(
            "[重复工具调用提醒] 你已在本轮第 {count} 次用相同参数调用工具 `{tool}`。\
             如果已经拿到足够信息，请停止重复调用并直接给出最终回答；\
             如果确实还需要调用，请换一种参数或说明为什么必须重复。"
        );
        Self {
            tool: tool.to_owned(),
            arguments: arguments.clone(),
            count,
            threshold: count,
            text,
        }
    }
}
