use super::*;

/// Service key：`<name>/v<major>`（spec/cpp-v1.md §5.1），如 `tool-registry/v1`。
///
/// 名字部分与 manifest 校验对齐：`[a-z0-9-]+`；major 是十进制数字（不允许前导 `+`/`-`）。
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ServiceKey {
    raw: String,
    name: String,
    major: u32,
}

impl ServiceKey {
    /// 解析并校验；非法返回 [`PluginError::InvalidServiceKey`]。
    pub fn parse(raw: impl Into<String>) -> Result<Self, PluginError> {
        let raw = raw.into();
        let Some((name, version)) = raw.rsplit_once('/') else {
            return Err(invalid_service_key(&raw, "must be `<name>/v<major>`"));
        };
        if name.is_empty()
            || !name
                .bytes()
                .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
        {
            return Err(invalid_service_key(
                &raw,
                "name part must match `[a-z0-9-]+`",
            ));
        }
        let Some(major_text) = version.strip_prefix('v') else {
            return Err(invalid_service_key(&raw, "version part must be `v<major>`"));
        };
        if major_text.is_empty() || !major_text.bytes().all(|byte| byte.is_ascii_digit()) {
            return Err(invalid_service_key(&raw, "major must be decimal digits"));
        }
        let Ok(major) = major_text.parse::<u32>() else {
            return Err(invalid_service_key(&raw, "major does not fit in u32"));
        };
        let name = name.to_owned();
        Ok(Self { raw, name, major })
    }

    /// 名字部分（不含 `/vN`）。
    pub fn name(&self) -> &str {
        &self.name
    }

    /// major 版本号。
    pub fn major(&self) -> u32 {
        self.major
    }

    /// 原始文本。
    pub fn as_str(&self) -> &str {
        &self.raw
    }
}

impl fmt::Display for ServiceKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.raw)
    }
}

impl FromStr for ServiceKey {
    type Err = PluginError;

    fn from_str(raw: &str) -> Result<Self, Self::Err> {
        Self::parse(raw)
    }
}

fn invalid_service_key(key: &str, reason: &str) -> PluginError {
    PluginError::InvalidServiceKey {
        key: key.to_owned(),
        reason: reason.to_owned(),
    }
}

/// 服务提供者身份（用于"只回收自己发布的服务"）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServiceProvider {
    pub plugin_id: String,
    pub fiber_id: u64,
    pub generation: u64,
}

struct ServiceEntry {
    value: Arc<dyn Any + Send + Sync>,
    provider: ServiceProvider,
}

/// 类型擦除的 Service 表（最小 Realm：一个表 = 一个 Realm）。
///
/// 服务值是 `Arc<dyn Any + Send + Sync>`：发布方与使用方通过具体类型
/// （通常是 trait object，如 `Arc<dyn ToolRegistry>`）downcast。**不使用 unsafe**。
#[derive(Default)]
pub struct ServiceTable {
    services: BTreeMap<ServiceKey, ServiceEntry>,
}

impl fmt::Debug for ServiceTable {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ServiceTable")
            .field("services", &self.services.keys().collect::<Vec<_>>())
            .finish()
    }
}

impl ServiceTable {
    pub fn new() -> Self {
        Self::default()
    }

    /// 发布服务；同一 key 已有提供者时返回 `E_SERVICE_CONFLICT`（spec §5.2）。
    pub fn publish(
        &mut self,
        key: ServiceKey,
        value: Arc<dyn Any + Send + Sync>,
        provider: ServiceProvider,
    ) -> Result<(), PluginError> {
        if self.services.contains_key(&key) {
            return Err(PluginError::ServiceConflict {
                key: key.as_str().to_owned(),
            });
        }
        self.services.insert(key, ServiceEntry { value, provider });
        Ok(())
    }

    /// 撤销服务；幂等（返回是否真的移除了条目）。
    pub fn revoke(&mut self, key: &ServiceKey) -> bool {
        self.services.remove(key).is_some()
    }

    /// 仅当当前提供者就是给定 Fiber 时才撤销（卸载/回滚用，避免误删别人的服务）。
    pub fn revoke_if_owned(&mut self, key: &ServiceKey, provider: &ServiceProvider) -> bool {
        let owned = self
            .services
            .get(key)
            .is_some_and(|entry| entry.provider == *provider);
        if owned {
            self.services.remove(key);
        }
        owned
    }

    /// 按具体类型取服务；类型不匹配返回 `None`（不 panic）。
    pub fn get<T: Any + Send + Sync>(&self, key: &ServiceKey) -> Option<Arc<T>> {
        let entry = self.services.get(key)?;
        Arc::clone(&entry.value).downcast::<T>().ok()
    }

    /// 类型擦除视图（诊断/转发用）。
    pub fn get_erased(&self, key: &ServiceKey) -> Option<Arc<dyn Any + Send + Sync>> {
        self.services.get(key).map(|entry| Arc::clone(&entry.value))
    }

    /// 当前提供者。
    pub fn provider(&self, key: &ServiceKey) -> Option<&ServiceProvider> {
        self.services.get(key).map(|entry| &entry.provider)
    }

    pub fn contains(&self, key: &ServiceKey) -> bool {
        self.services.contains_key(key)
    }

    pub fn keys(&self) -> impl Iterator<Item = &ServiceKey> {
        self.services.keys()
    }

    pub fn len(&self) -> usize {
        self.services.len()
    }

    pub fn is_empty(&self) -> bool {
        self.services.is_empty()
    }
}

// ---------------------------------------------------------------------------
// EffectStack
// ---------------------------------------------------------------------------
