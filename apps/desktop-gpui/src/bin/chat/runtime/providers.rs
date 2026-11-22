use crate::desktop_permission_mode;
use dshr_agent_core::{
    CompletionRequest, MockProvider, ModelProvider, ModelReply, ReasoningEffort,
};
use dshr_llm_deepseek::DeepSeekProvider;
use dshr_llm_openai::OpenAiProvider;
use dshr_llm_retry::{RetryPolicy, RetryingProvider};
use dshr_tool_runtime::PermissionMode;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;

/// 给没有取消概念的 provider（mock）补上取消 token。
///
/// 无 key/自检场景仍要验证“停止”：Agent 通过 `provider.cancel_token()` 检查取消，
/// MockProvider 自己不带 token，所以在这里包一层（真实 provider 使用各自的 token）。
struct CancelAwareProvider<P> {
    inner: P,
    cancel: Arc<AtomicBool>,
}

impl<P: ModelProvider> ModelProvider for CancelAwareProvider<P> {
    fn complete(&mut self, request: &CompletionRequest) -> Result<ModelReply, String> {
        self.inner.complete(request)
    }

    fn cancel_token(&self) -> Option<Arc<AtomicBool>> {
        Some(self.cancel.clone())
    }

    fn complete_streaming(
        &mut self,
        request: &CompletionRequest,
        on_delta: &mut dyn FnMut(&str),
    ) -> Result<ModelReply, String> {
        self.inner.complete_streaming(request, on_delta)
    }
}

pub(crate) fn cancel_aware(
    provider: impl ModelProvider + 'static,
    cancel: &Arc<AtomicBool>,
) -> Box<dyn ModelProvider> {
    Box::new(CancelAwareProvider {
        inner: provider,
        cancel: cancel.clone(),
    })
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ModelKind {
    Unconfigured,
    Mock,
    DeepSeek,
    OpenAi,
}

impl ModelKind {
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Unconfigured => "未配置模型",
            Self::Mock => "本地模拟",
            Self::DeepSeek => "DeepSeek",
            Self::OpenAi => "OpenAI 兼容",
        }
    }
}

pub(crate) enum DesktopModel {
    Unconfigured,
    Mock,
    DeepSeek(DeepSeekProvider),
    OpenAi(OpenAiProvider),
}

impl DesktopModel {
    pub(crate) fn provider_name(&self) -> &'static str {
        match self {
            Self::Unconfigured => "",
            Self::Mock => "mock",
            Self::DeepSeek(_) => "deepseek",
            Self::OpenAi(_) => "openai",
        }
    }

    pub(crate) fn kind(&self) -> ModelKind {
        match self {
            Self::Unconfigured => ModelKind::Unconfigured,
            Self::Mock => ModelKind::Mock,
            Self::DeepSeek(_) => ModelKind::DeepSeek,
            Self::OpenAi(_) => ModelKind::OpenAi,
        }
    }

    pub(crate) fn name(&self) -> &str {
        match self {
            Self::DeepSeek(provider) => provider.model(),
            Self::OpenAi(provider) => provider.model(),
            Self::Unconfigured | Self::Mock => self.kind().label(),
        }
    }

    pub(crate) fn is_configured(&self) -> bool {
        !matches!(self, Self::Unconfigured)
    }

    pub(crate) fn supports_attachments(&self) -> bool {
        match self {
            Self::Unconfigured => false,
            Self::DeepSeek(provider) => provider.supports_attachments(),
            Self::OpenAi(_) | Self::Mock => true,
        }
    }

    pub(crate) fn reasoning_levels(&self) -> &'static [ReasoningEffort] {
        use ReasoningEffort::{High, Low, Max, Off};
        match self {
            Self::DeepSeek(_) => &[Off, Low, High, Max],
            Self::OpenAi(_) => &[Low, High, Max],
            Self::Unconfigured | Self::Mock => &[],
        }
    }

    pub(crate) fn provider_reasoning(&self) -> Option<ReasoningEffort> {
        match self {
            Self::DeepSeek(provider) => provider.reasoning_effort(),
            Self::OpenAi(provider) => provider.reasoning_effort(),
            Self::Unconfigured | Self::Mock => None,
        }
    }

    pub(crate) fn provider(
        &self,
        cancel: &Arc<AtomicBool>,
        reasoning: Option<ReasoningEffort>,
    ) -> Box<dyn ModelProvider> {
        let reasoning = reasoning.or_else(|| self.provider_reasoning());
        let (name, configured): (&str, Box<dyn ModelProvider>) = match self {
            Self::Unconfigured => return Box::new(UnconfiguredProvider),
            Self::Mock => {
                eprintln!("[chat] provider=mock");
                return cancel_aware(MockProvider, cancel);
            }
            Self::DeepSeek(provider) => (
                "deepseek",
                Box::new(
                    provider
                        .clone()
                        .with_reasoning_effort(reasoning)
                        .with_cancel(cancel.clone()),
                ),
            ),
            Self::OpenAi(provider) => (
                "openai",
                Box::new(
                    provider
                        .clone()
                        .with_reasoning_effort(reasoning)
                        .with_cancel(cancel.clone()),
                ),
            ),
        };
        let retry = RetryPolicy::from_env();
        eprintln!(
            "[chat] provider={name} model={} retries={}",
            self.name(),
            retry.max_attempts
        );
        Box::new(RetryingProvider::new(configured, retry))
    }
}

struct UnconfiguredProvider;

impl ModelProvider for UnconfiguredProvider {
    fn complete(&mut self, _: &CompletionRequest) -> Result<ModelReply, String> {
        Err("请先配置可用的模型连接。".to_owned())
    }
}

pub(crate) fn force_mock() -> bool {
    std::env::var("DSHR_FORCE_MOCK").as_deref() == Ok("1")
}

pub(crate) fn configured_models() -> Vec<Arc<DesktopModel>> {
    if force_mock() {
        return vec![Arc::new(DesktopModel::Mock)];
    }
    let mut models = Vec::new();
    if let Some(provider) = DeepSeekProvider::from_env() {
        if matches!(
            provider.base_url().trim_end_matches('/'),
            "https://api.deepseek.com" | "https://api.deepseek.com/v1"
        ) && !provider.supports_attachments()
        {
            models.push(Arc::new(DesktopModel::DeepSeek(
                provider.clone().with_model("deepseek-flash"),
            )));
        }
        models.insert(0, Arc::new(DesktopModel::DeepSeek(provider)));
    }
    if let Some(provider) = OpenAiProvider::from_env() {
        models.push(Arc::new(DesktopModel::OpenAi(provider)));
    }
    if std::env::var("DSHR_PROVIDER").as_deref() == Ok("mock") {
        models.push(Arc::new(DesktopModel::Mock));
    }
    models
}

#[derive(Clone)]
pub(crate) struct DesktopSettings {
    pub(crate) model: Arc<DesktopModel>,
    pub(crate) permission: Option<PermissionMode>,
    pub(crate) reasoning: Option<ReasoningEffort>,
}

impl DesktopSettings {
    pub(crate) fn from_env(models: &[Arc<DesktopModel>]) -> Self {
        let requested = if force_mock() {
            Some(ModelKind::Mock)
        } else {
            match std::env::var("DSHR_PROVIDER").as_deref() {
                Ok("mock") => Some(ModelKind::Mock),
                Ok("deepseek") => Some(ModelKind::DeepSeek),
                Ok("openai") => Some(ModelKind::OpenAi),
                _ => None,
            }
        };
        let model = models
            .iter()
            .find(|model| requested.is_none_or(|kind| model.kind() == kind))
            .cloned()
            .unwrap_or_else(|| Arc::new(DesktopModel::Unconfigured));
        if let Some(effort) = ReasoningEffort::from_env() {
            println!("[chat] reasoning_effort={}", effort.as_str());
        }
        Self {
            model,
            permission: desktop_permission_mode(),
            reasoning: None,
        }
    }
}
