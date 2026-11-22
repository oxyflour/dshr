use super::*;

#[derive(Debug, Clone, PartialEq)]
pub struct ToolSpec {
    pub name: String,
    pub description: String,
    /// JSON Schema（原样交给模型；MockProvider 忽略它）。
    pub parameters: Value,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ToolCallRequest {
    pub call_id: String,
    pub tool: String,
    pub arguments: Value,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ToolOutcome {
    pub is_error: bool,
    /// 模型可见内容（DSH: `content: ContentBlock[]`）。
    pub content: Value,
    /// 结构化返回值（DSH: `value`）。展示与回放用它，不要再从 content 里挖。
    pub value: Value,
    /// 工具自带的元数据（DSH: `meta`）。
    pub meta: Option<Value>,
}

impl ToolOutcome {
    pub fn success(content: Value, value: Value) -> Self {
        Self {
            is_error: false,
            content,
            value,
            meta: None,
        }
    }

    pub fn failure(reason: impl Into<String>) -> Self {
        let reason = reason.into();
        Self {
            is_error: true,
            content: json!([{ "type": "text", "text": reason }]),
            value: Value::Null,
            meta: None,
        }
    }
}

pub fn tool_image_references(content: &Value) -> Vec<MediaAttachment> {
    content
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|block| {
            if block["type"] != "image" {
                return None;
            }
            let reference = block["attachment"]["attachmentId"].as_str()?;
            let hash = reference.strip_prefix("spill:")?;
            if hash.len() != 64 || !hash.bytes().all(|byte| byte.is_ascii_hexdigit()) {
                return None;
            }
            let format = AttachmentFormat::from_mime(block["attachment"]["mediaType"].as_str()?)?;
            if format == AttachmentFormat::Pdf {
                return None;
            }
            Some(MediaAttachment {
                name: block["attachment"]["name"]
                    .as_str()
                    .unwrap_or("tool image")
                    .to_owned(),
                format,
                reference: reference.to_owned(),
                bytes: None,
            })
        })
        .collect()
}

#[derive(Default)]
pub struct StepContext {
    pub system_sections: Vec<String>,
    pub turn_instructions: Vec<String>,
    pub unavailable_tools: Vec<String>,
}

pub trait ToolExecutor {
    fn specs(&self) -> Vec<ToolSpec>;

    /// Refresh the tool inventory immediately before handing it to the model.
    fn prepare_step(&mut self) -> Vec<ToolSpec> {
        self.specs()
    }
    fn prepare_step_context(&mut self, _step: u32, _user_text: Option<&str>) -> StepContext {
        StepContext::default()
    }
    fn execute(&mut self, call: &ToolCallRequest) -> ToolOutcome;

    fn tool_images(&self, _outcome: &ToolOutcome) -> Vec<MediaAttachment> {
        Vec::new()
    }

    /// 一个 step 内**同一批**工具调用：返回顺序必须与 `calls` 一一对应。
    ///
    /// 默认实现逐个 `execute`（保持串行语义）；支持并发的执行器（如 sidecar）可覆盖，
    /// 在 `max_parallel` 上限内并发跑完再按原顺序返回。预算/取消在 Agent 层裁决，
    /// 这里收到的都是"本批真正要执行"的调用。
    fn execute_batch(
        &mut self,
        calls: &[ToolCallRequest],
        max_parallel: usize,
    ) -> Vec<ToolOutcome> {
        let _ = max_parallel;
        calls.iter().map(|call| self.execute(call)).collect()
    }
}
