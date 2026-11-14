//! CPP-v1 capability 探测（spec/cpp-v1.md §10）。
//!
//! 规范只允许 `plugin.api` 写 major 版本，minor 能力通过 capability 字符串探测：
//! 宿主在装载期把**已实现**的能力集合交给 Fiber，插件在 `apply` 期间调用
//! [`CapabilitySet::has`] / [`CapabilitySet::require`] 询问；缺失时得到
//! `E_PERMISSION`（[`PluginError::CapabilityMissing`]），由宿主把 Fiber 标成 `Failed`。
//!
//! ## 与 cordis-rs 的适配缝隙
//!
//! cordis-rs 没有 capability 概念，这是 CPP-v1 自有的版本协商层。将来换实现时，
//! capability 只出现在宿主装载路径（`plugin-host::fiber` 的 `FiberContext`）里，
//! 不进入 [`crate::Plugin`] trait；cordis 的 provide/inject 语义不需要改动。

use std::collections::BTreeSet;
use std::fmt;
use std::str::FromStr;

use crate::PluginError;

/// 工具可以流式产出结果（spec/cpp-v1.md §7 的流式工具）。
pub const TOOL_STREAM: &str = "cpp.tool.stream";
/// 工具调用支持取消令牌（spec/cpp-v1.md §7.4）。
pub const TOOL_CANCEL: &str = "cpp.tool.cancel";
/// 工具结果可以引用附件（spec/cpp-v1.md §7.2 的 `$attachment`）。
pub const TOOL_ATTACHMENT: &str = "cpp.tool.attachment";
/// 插件可以提交 UI 贡献（spec/cpp-v1.md §3.3 的 `[[ui]]`）。
pub const UI_CONTRIBUTE: &str = "cpp.ui.contribute";
/// 插件可以发布 Service（spec/cpp-v1.md §5）。
pub const SERVICE_PUBLISH: &str = "cpp.service.publish";
/// 插件登记的 Effect 会被宿主回收（spec/cpp-v1.md §6）。
pub const EFFECT_DISPOSE: &str = "cpp.effect.dispose";
/// 插件可以订阅宿主事件。
pub const EVENT_SUBSCRIBE: &str = "cpp.event.subscribe";
/// 配置变更走 `on_config` 热更新（spec/cpp-v1.md §8）。
pub const CONFIG_HOT_RELOAD: &str = "cpp.config.hot_reload";
/// 插件日志可以流式回传宿主。
pub const LOG_STREAM: &str = "cpp.log.stream";

/// 全部 v1 capability 常量（宿主 `host_v1()` 的默认集合）。
pub const ALL_V1: [&str; 9] = [
    TOOL_STREAM,
    TOOL_CANCEL,
    TOOL_ATTACHMENT,
    UI_CONTRIBUTE,
    SERVICE_PUBLISH,
    EFFECT_DISPOSE,
    EVENT_SUBSCRIBE,
    CONFIG_HOT_RELOAD,
    LOG_STREAM,
];

/// 一个 capability 标识（`cpp.<area>.<feature>`）。
///
/// 只做**标识与校验**，不携带策略：宿主是否授予由 [`CapabilitySet`] 决定。
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Capability(String);

impl Capability {
    /// 校验并构造；非法名字（空、含空白/大写）返回 `E_INTERNAL`。
    pub fn parse(name: impl Into<String>) -> Result<Self, PluginError> {
        let name = name.into();
        if is_valid_capability(&name) {
            Ok(Self(name))
        } else {
            Err(PluginError::Invalid(format!(
                "invalid capability `{name}`; expected `[a-z][a-z0-9._-]{{0,63}}`"
            )))
        }
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for Capability {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl FromStr for Capability {
    type Err = PluginError;

    fn from_str(raw: &str) -> Result<Self, Self::Err> {
        Self::parse(raw)
    }
}

impl AsRef<str> for Capability {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

/// 宿主授予某个 Fiber 的 capability 集合。
///
/// 默认空集：缺省拒绝（与 spec/cpp-v1.md §3.4 "默认拒绝" 一致）。
/// 插件不得在运行中提升 capability——这是装载时属性，改变意味着重启 Fiber。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CapabilitySet {
    granted: BTreeSet<Capability>,
}

impl CapabilitySet {
    /// 空集合：除显式 `grant` 外全部不可用。
    pub fn new() -> Self {
        Self::default()
    }

    /// 宿主 v1 已实现的全部 capability（`apply` 期间可用的完整面）。
    pub fn host_v1() -> Self {
        let mut set = Self::default();
        for name in ALL_V1 {
            // 常量都是合法名字，这里不可能失败。
            if let Ok(capability) = Capability::parse(name) {
                set.grant(capability);
            }
        }
        set
    }

    /// 授予一个 capability；重复授予幂等。
    pub fn grant(&mut self, capability: Capability) {
        self.granted.insert(capability);
    }

    /// 便捷入口：按名字授予（名字非法时忽略并返回 `false`）。
    pub fn grant_str(&mut self, name: &str) -> bool {
        match Capability::parse(name) {
            Ok(capability) => {
                self.grant(capability);
                true
            }
            Err(_) => false,
        }
    }

    /// 探测能力；大小写敏感，未知名称一律 `false`。
    pub fn has(&self, capability: &str) -> bool {
        self.granted
            .iter()
            .any(|granted| granted.as_str() == capability)
    }

    /// 探测 + 缺失即报错（`E_PERMISSION`）。插件在 `apply` 里用这个表达硬依赖。
    pub fn require(&self, capability: &str) -> Result<(), PluginError> {
        if self.has(capability) {
            Ok(())
        } else {
            Err(PluginError::CapabilityMissing {
                capability: capability.to_owned(),
            })
        }
    }

    pub fn iter(&self) -> impl Iterator<Item = &Capability> {
        self.granted.iter()
    }

    pub fn len(&self) -> usize {
        self.granted.len()
    }

    pub fn is_empty(&self) -> bool {
        self.granted.is_empty()
    }
}

/// capability 名字：小写字母开头，后接小写字母/数字/`.`/`_`/`-`，长度 ≤ 64。
fn is_valid_capability(name: &str) -> bool {
    let bytes = name.as_bytes();
    if bytes.is_empty() || bytes.len() > 64 || !bytes[0].is_ascii_lowercase() {
        return false;
    }
    bytes[1..].iter().all(|byte| {
        byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'.' | b'_' | b'-')
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    // CPP-FIBER-101..105：capability 标识、集合、缺失拒绝。
    #[test]
    fn cpp_fiber_101_capability_constants_are_valid() {
        for name in ALL_V1 {
            let capability = Capability::parse(name).expect("常量必须是合法 capability");
            assert_eq!(capability.as_str(), name);
        }
        assert_eq!(TOOL_STREAM, "cpp.tool.stream");
        assert_eq!(UI_CONTRIBUTE, "cpp.ui.contribute");
    }

    #[test]
    fn cpp_fiber_102_capability_rejects_invalid_names() {
        for bad in ["", "Cpp.Tool.Stream", "cpp tool stream", "1cpp.tool"] {
            assert!(Capability::parse(bad).is_err(), "`{bad}` 应被拒绝");
        }
        assert!(Capability::parse("x".repeat(65)).is_err());
    }

    #[test]
    fn cpp_fiber_103_empty_set_has_nothing_and_require_fails() {
        let set = CapabilitySet::new();
        assert!(set.is_empty());
        assert!(!set.has(TOOL_STREAM));
        let err = set.require(TOOL_STREAM).unwrap_err();
        assert_eq!(
            err,
            PluginError::CapabilityMissing {
                capability: TOOL_STREAM.to_owned()
            }
        );
        assert_eq!(err.code(), "E_PERMISSION");
        assert!(!err.is_retryable());
    }

    #[test]
    fn cpp_fiber_104_host_v1_grants_every_v1_capability() {
        let set = CapabilitySet::host_v1();
        assert_eq!(set.len(), ALL_V1.len());
        for name in ALL_V1 {
            assert!(set.has(name), "host_v1 应包含 {name}");
            assert!(set.require(name).is_ok());
        }
        assert!(!set.has("cpp.future.unknown"));
        assert_eq!(set.granted_names().count(), ALL_V1.len());
    }

    #[test]
    fn cpp_fiber_105_grant_is_idempotent_and_case_sensitive() {
        let mut set = CapabilitySet::new();
        assert!(set.grant_str(UI_CONTRIBUTE));
        assert!(set.grant_str(UI_CONTRIBUTE));
        assert_eq!(set.len(), 1);
        assert!(set.has(UI_CONTRIBUTE));
        assert!(!set.has("CPP.UI.CONTRIBUTE"));
        assert!(!set.grant_str("Not Valid"));
    }

    impl CapabilitySet {
        /// 测试辅助：已授予名称的迭代器。
        fn granted_names(&self) -> impl Iterator<Item = &str> {
            self.granted.iter().map(Capability::as_str)
        }
    }
}
