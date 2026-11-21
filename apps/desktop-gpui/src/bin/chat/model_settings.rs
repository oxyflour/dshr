use super::{
    chrome_button, force_mock, ChatApp, DesktopModel, DesktopSettings, ModelKind, TextInput, Theme,
};
use dshr_llm_deepseek::DeepSeekProvider;
use dshr_llm_openai::OpenAiProvider;
use gpui::prelude::*;
use gpui::{div, px, relative, AnyElement, Context, Div, Entity, Focusable, FontWeight, Window};
use ring::digest::{digest, SHA256};
use ring::rand::{SecureRandom, SystemRandom};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::collections::BTreeMap;
use std::fs::{self, OpenOptions};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use url::Url;

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub(super) enum ConnectionId {
    DeepSeek,
    OpenAi,
    #[serde(rename = "openai_model")]
    OpenAiModel(String),
}

impl ConnectionId {
    const LEGACY: [Self; 2] = [Self::DeepSeek, Self::OpenAi];

    fn key(&self) -> &str {
        match self {
            Self::DeepSeek => "0",
            Self::OpenAi => "1",
            Self::OpenAiModel(id) => id,
        }
    }

    fn kind(&self) -> ModelKind {
        match self {
            Self::DeepSeek => ModelKind::DeepSeek,
            Self::OpenAi | Self::OpenAiModel(_) => ModelKind::OpenAi,
        }
    }

    fn new_openai() -> Result<Self, ring::error::Unspecified> {
        let mut bytes = [0u8; 16];
        SystemRandom::new().fill(&mut bytes)?;
        Ok(Self::OpenAiModel(
            bytes.iter().map(|byte| format!("{byte:02x}")).collect(),
        ))
    }

    fn from_marker(name: &str) -> Option<Self> {
        (name.len() == 32 && name.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')))
            .then(|| Self::OpenAiModel(name.to_owned()))
    }
}

// Keep the endpoint and its key in a single vault entry, so replacing a connection
// cannot pair an old key with a new endpoint after an interrupted save.
#[derive(Serialize, Deserialize)]
struct Connection {
    base_url: String,
    model_id: String,
    api_key: String,
}

impl Connection {
    fn decode(bytes: &[u8]) -> Option<Self> {
        let mut connection: Self = serde_json::from_slice(bytes).ok()?;
        connection.base_url = base_url(&connection.base_url).ok()?;
        validate_model_id(&connection.model_id).ok()?;
        validate_api_key(&connection.api_key).ok()?;
        Some(connection)
    }

    fn into_model(self, id: &ConnectionId) -> SavedModel {
        let model = match id {
            ConnectionId::DeepSeek => DesktopModel::DeepSeek(
                DeepSeekProvider::new(self.api_key)
                    .with_base_url(self.base_url.clone())
                    .with_model(self.model_id),
            ),
            ConnectionId::OpenAi | ConnectionId::OpenAiModel(_) => DesktopModel::OpenAi(
                OpenAiProvider::new(self.api_key)
                    .with_connection(self.base_url.clone(), self.model_id),
            ),
        };
        SavedModel {
            base_url: self.base_url,
            model: Arc::new(model),
        }
    }
}

fn base_url(value: &str) -> Result<String, &'static str> {
    const INVALID: &str = "请填写 HTTP 或 HTTPS API 根地址，地址中应省略账号、查询参数和片段。";
    let mut url = Url::parse(value.trim()).map_err(|_| INVALID)?;
    if !matches!(url.scheme(), "http" | "https")
        || !url.has_host()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err(INVALID);
    }
    if let Some(path) = url
        .path()
        .trim_end_matches('/')
        .strip_suffix("/chat/completions")
    {
        let path = path.to_owned();
        url.set_path(&path);
    }
    let result = url.as_str().trim_end_matches('/');
    if result.len() > 768 {
        return Err("API 根地址过长。");
    }
    Ok(result.to_owned())
}

fn validate_model_id(value: &str) -> Result<(), &'static str> {
    if value.is_empty()
        || value.len() > 256
        || value
            .chars()
            .any(|ch| ch.is_whitespace() || ch.is_control())
    {
        return Err("请填写服务商提供的模型 ID（不含空白，最多 256 字节）。");
    }
    Ok(())
}

fn same_origin(previous: &str, next: &str) -> bool {
    Url::parse(previous)
        .ok()
        .zip(Url::parse(next).ok())
        .is_some_and(|(previous, next)| previous.origin() == next.origin())
}

fn validate_api_key(value: &str) -> Result<(), &'static str> {
    if value.is_empty() || value.len() > 1024 || !value.bytes().all(|byte| byte.is_ascii_graphic())
    {
        return Err("请填入有效的 API Key（最多 1024 个 ASCII 字符）。");
    }
    Ok(())
}

struct SavedModel {
    base_url: String,
    model: Arc<DesktopModel>,
}

struct Editor {
    id: ConnectionId,
    key_source: Option<ConnectionId>,
    base_url: Entity<TextInput>,
    model_id: Entity<TextInput>,
    api_key: Entity<TextInput>,
    confirm_remove: bool,
}

pub(super) struct ModelSettings {
    namespace: String,
    registrations: PathBuf,
    env_models: Vec<Arc<DesktopModel>>,
    saved: BTreeMap<ConnectionId, SavedModel>,
    editor: Option<Editor>,
    notice: Option<Result<&'static str, &'static str>>,
    pub(super) pending: bool,
}

impl ModelSettings {
    pub(super) fn new(root: &Path, env_models: Vec<Arc<DesktopModel>>) -> Self {
        let root = root.canonicalize().unwrap_or_else(|_| root.to_owned());
        let namespace = digest(&SHA256, root.to_string_lossy().as_bytes())
            .as_ref()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect();
        Self {
            namespace,
            registrations: root.join("model-connections"),
            env_models,
            saved: BTreeMap::new(),
            editor: None,
            notice: None,
            pending: false,
        }
    }

    fn credential_ref(&self, id: &ConnectionId) -> String {
        format!("dshr://models/{}/{}", self.namespace, id.key())
    }

    // Keep an empty file per custom ID so independent saves never overwrite a shared index.
    // Only follow markers whose names can be generated by new_openai().
    fn registered_ids(&self) -> io::Result<Vec<ConnectionId>> {
        let entries = match fs::read_dir(&self.registrations) {
            Ok(entries) => entries,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(vec![]),
            Err(error) => return Err(error),
        };
        let mut ids = Vec::new();
        for entry in entries {
            let entry = entry?;
            if entry.file_type()?.is_file() {
                if let Some(id) = entry
                    .file_name()
                    .to_str()
                    .and_then(ConnectionId::from_marker)
                {
                    ids.push(id);
                }
            }
        }
        Ok(ids)
    }

    fn registration(&self, id: &ConnectionId) -> Option<PathBuf> {
        match id {
            ConnectionId::OpenAiModel(key) => Some(self.registrations.join(key)),
            _ => None,
        }
    }

    fn options(&self) -> Vec<Arc<DesktopModel>> {
        let mut options = self.env_models.clone();
        let mock = if options
            .last()
            .is_some_and(|model| model.kind() == ModelKind::Mock)
        {
            options.pop()
        } else {
            None
        };
        options.extend(self.saved.values().map(|saved| saved.model.clone()));
        options.extend(mock);
        options
    }

    fn saved_entry(&self, model: &Arc<DesktopModel>) -> Option<(&ConnectionId, &SavedModel)> {
        self.saved
            .iter()
            .find(|(_, saved)| Arc::ptr_eq(model, &saved.model))
    }

    pub(super) fn source(&self, model: &Arc<DesktopModel>) -> &str {
        if model.kind() == ModelKind::Mock {
            "本地模拟"
        } else if let Some((_, saved)) = self.saved_entry(model) {
            &saved.base_url
        } else {
            "环境配置"
        }
    }

    pub(super) fn reasoning_key(&self, model: &Arc<DesktopModel>) -> String {
        if let Some((id, _)) = self.saved_entry(model) {
            return format!("saved:{}", id.key());
        }
        let endpoint = match model.as_ref() {
            DesktopModel::DeepSeek(provider) => provider.base_url(),
            DesktopModel::OpenAi(provider) => provider.base_url(),
            _ => "",
        };
        json!([
            model.kind().label(),
            endpoint.trim_end_matches('/'),
            model.name()
        ])
        .to_string()
    }

    pub(super) fn close_editor(&mut self) {
        if self.editor.take().is_some() {
            self.notice = None;
        }
    }

    pub(super) fn editor_is_open(&self) -> bool {
        self.editor.is_some()
    }
}

impl ChatApp {
    pub(super) fn load_model_settings(
        &mut self,
        autosend: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if force_mock() {
            if let Some(text) = autosend {
                self.on_submit(&text, window, cx);
            }
            return;
        }
        self.model_settings.pending = true;
        let ids = self.model_settings.registered_ids();
        let mut failed = ids.is_err();
        let reads = ConnectionId::LEGACY
            .into_iter()
            .chain(ids.unwrap_or_default())
            .map(|id| {
                let read = cx.read_credentials(&self.model_settings.credential_ref(&id));
                (id, read)
            })
            .collect::<Vec<_>>();
        cx.spawn_in(window, async move |this, cx| {
            let mut saved = BTreeMap::new();
            for (id, read) in reads {
                match read.await {
                    Ok(Some((_, bytes))) => match Connection::decode(&bytes) {
                        Some(connection) => {
                            let entry = connection.into_model(&id);
                            saved.insert(id, entry);
                        }
                        None => failed = true,
                    },
                    Ok(None) => {}
                    Err(_) => failed = true,
                }
            }
            let _ = this.update_in(cx, |chat, window, cx| {
                chat.model_settings.saved = saved;
                chat.model_settings.pending = false;
                chat.model_settings.notice =
                    failed.then_some(Err("无法读取部分已保存的模型连接。"));
                chat.model_options = chat.model_settings.options();
                let mut settings = DesktopSettings::from_env(&chat.model_options);
                chat.preferences.apply(&mut settings, &chat.model_options);
                if std::env::var_os("DSHR_PROVIDER").is_none() {
                    if let Some(saved) = chat
                        .preferences
                        .model_connection
                        .as_ref()
                        .and_then(|id| chat.model_settings.saved.get(id))
                    {
                        settings.model = saved.model.clone();
                    }
                }
                chat.apply_model(settings.model);
                if let Some(text) = autosend {
                    chat.on_submit(&text, window, cx);
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    fn open_model_editor(
        &mut self,
        id: ConnectionId,
        copy_from: Option<ConnectionId>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let existing = self.model_settings.saved.get(&id);
        let key_source = copy_from.or_else(|| existing.map(|_| id.clone()));
        let source = key_source
            .as_ref()
            .and_then(|id| self.model_settings.saved.get(id));
        let (default_url, default_id) = match id.kind() {
            ModelKind::DeepSeek => ("https://api.deepseek.com", "deepseek-flash"),
            _ => ("https://api.openai.com/v1", ""),
        };
        let address = source.map_or(default_url, |saved| saved.base_url.as_str());
        let model_id = existing.map_or(default_id, |saved| saved.model.name());
        let base_input = cx.new(|cx| TextInput::new("API 根地址", cx));
        let id_input = cx.new(|cx| TextInput::new("模型 ID", cx));
        let key_input = cx.new(|cx| {
            TextInput::new(
                if source.is_some() {
                    "API Key 已保存，留空沿用"
                } else {
                    "输入 API Key"
                },
                cx,
            )
            .password()
        });
        base_input.update(cx, |input, cx| input.set_content(address, window, cx));
        id_input.update(cx, |input, cx| input.set_content(model_id, window, cx));
        let focus = if existing.is_some() {
            &key_input
        } else {
            &id_input
        };
        window.focus(&focus.read(cx).focus_handle(cx), cx);
        self.model_settings.editor = Some(Editor {
            id,
            key_source,
            base_url: base_input,
            model_id: id_input,
            api_key: key_input,
            confirm_remove: false,
        });
        self.model_settings.notice = None;
        cx.notify();
    }

    fn add_openai_model(
        &mut self,
        copy_from: Option<ConnectionId>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match ConnectionId::new_openai() {
            Ok(id) => self.open_model_editor(id, copy_from, window, cx),
            Err(_) => {
                self.model_settings.notice = Some(Err("无法创建模型连接标识，请重试。"));
                cx.notify();
            }
        }
    }

    fn lock_model_connections(&mut self, cx: &mut Context<Self>) -> Option<fs::File> {
        match super::try_lock_session(&self.model_settings.registrations) {
            Ok(lock) => Some(lock),
            Err(error) => {
                self.model_settings.notice =
                    Some(Err(if error.kind() == io::ErrorKind::WouldBlock {
                        "另一个窗口正在保存模型设置，请重试。"
                    } else {
                        "无法写入本地模型设置，请检查数据目录权限。"
                    }));
                cx.notify();
                None
            }
        }
    }

    fn save_model_connection(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(editor) = self
            .model_settings
            .editor
            .as_ref()
            .filter(|_| !self.busy && !self.model_settings.pending && !force_mock())
        else {
            return;
        };
        let id = editor.id.clone();
        let key_source = editor.key_source.as_ref();
        let key = editor.api_key.read(cx).content().to_owned();
        let model_id = editor.model_id.read(cx).content().trim().to_owned();
        let result = base_url(editor.base_url.read(cx).content()).and_then(|url| {
            validate_model_id(&model_id)?;
            if key.is_empty() {
                let Some(saved) = key_source.and_then(|id| self.model_settings.saved.get(id))
                else {
                    return Err("请输入 API Key。");
                };
                if !same_origin(&saved.base_url, &url) {
                    return Err("API 地址已切换到其他服务，请输入该服务的 API Key。");
                }
            } else {
                validate_api_key(&key)?;
            }
            Ok(url)
        });
        let url = match result {
            Ok(url) => url,
            Err(error) => {
                self.model_settings.notice = Some(Err(error));
                cx.notify();
                return;
            }
        };
        let reference = self.model_settings.credential_ref(&id);
        let registration = self.model_settings.registration(&id);
        let read_reference = key_source
            .filter(|_| key.is_empty())
            .map(|id| self.model_settings.credential_ref(id));
        let Some(lock) = self.lock_model_connections(cx) else {
            return;
        };
        let read = read_reference.map(|source| cx.read_credentials(&source));
        self.model_settings.pending = true;
        self.model_settings.notice = None;
        cx.spawn_in(window, async move |this, cx| {
            let result = async {
                let api_key = if let Some(read) = read {
                    let (_, bytes) = read
                        .await
                        .ok()
                        .flatten()
                        .ok_or("无法读取已保存的 API Key，请重新输入。")?;
                    let previous = Connection::decode(&bytes)
                        .ok_or("已保存的连接无效，请重新输入 API Key。")?;
                    // Another window could have saved a new connection since this one loaded.
                    if !same_origin(&previous.base_url, &url) {
                        return Err("已保存的地址已变更，请重新输入 API Key。");
                    }
                    previous.api_key
                } else {
                    key
                };
                let connection = Connection {
                    base_url: url,
                    model_id,
                    api_key,
                };
                let bytes = serde_json::to_vec(&connection).map_err(|_| "无法编码模型连接。")?;
                if let Some(path) = registration {
                    // Register first so an interrupted save can leave only a harmless empty marker.
                    fs::create_dir_all(path.parent().expect("connection registration directory"))
                        .and_then(|_| {
                            OpenOptions::new()
                                .write(true)
                                .create(true)
                                .truncate(false)
                                .open(path)
                        })
                        .and_then(|file| file.sync_all())
                        .map_err(|_| "无法登记模型连接，请检查数据目录权限。")?;
                }
                let write = cx
                    .update(|_, cx| cx.write_credentials(&reference, "DSHR", &bytes))
                    .map_err(|_| "无法访问系统凭据库。")?;
                write
                    .await
                    .map_err(|_| "无法保存到系统凭据库，请检查凭据库是否可用。")?;
                Ok(connection.into_model(&id))
            }
            .await;
            drop(lock);
            let _ = this.update_in(cx, |chat, window, cx| {
                chat.model_settings.pending = false;
                match result {
                    Ok(saved) => {
                        let model = saved.model.clone();
                        chat.model_settings.saved.insert(id, saved);
                        chat.model_options = chat.model_settings.options();
                        chat.close_model_editor(window, cx);
                        let applied = chat.select_model_in_settings(model, cx);
                        chat.model_settings.notice = Some(Ok(if applied {
                            "连接已保存，下一条消息将使用此模型。"
                        } else {
                            "连接已保存，可从会话模型中选择。"
                        }));
                    }
                    Err(error) => chat.model_settings.notice = Some(Err(error)),
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    fn remove_model_connection(
        &mut self,
        id: ConnectionId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.busy || self.model_settings.pending || force_mock() {
            return;
        }
        let Some(lock) = self.lock_model_connections(cx) else {
            return;
        };
        self.model_settings.pending = true;
        self.model_settings.notice = None;
        let registration = self.model_settings.registration(&id);
        let remove = cx.delete_credentials(&self.model_settings.credential_ref(&id));
        cx.spawn_in(window, async move |this, cx| {
            let result = remove.await;
            if result.is_ok() {
                if let Some(path) = registration {
                    // A leftover marker is safe when the corresponding credential is absent.
                    let _ = fs::remove_file(path);
                }
            }
            drop(lock);
            let _ = this.update_in(cx, |chat, window, cx| {
                chat.model_settings.pending = false;
                if result.is_ok() {
                    chat.model_settings.saved.remove(&id);
                    chat.model_options = chat.model_settings.options();
                    let current = chat.settings.model.clone();
                    if !chat
                        .model_options
                        .iter()
                        .any(|option| Arc::ptr_eq(option, &current))
                    {
                        let fallback = DesktopSettings::from_env(&chat.model_options).model;
                        chat.select_model_in_settings(fallback, cx);
                    } else if chat.preferences.model_connection.as_ref() == Some(&id) {
                        chat.select_model_in_settings(current, cx);
                    }
                    chat.close_model_editor(window, cx);
                    chat.model_settings.notice = Some(Ok("已移除保存的模型连接。"));
                } else {
                    chat.model_settings.notice = Some(Err("系统凭据库未能移除模型连接。"));
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    fn close_model_editor(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.model_settings.close_editor();
        if self.settings_tab.is_some() {
            window.focus(&self.settings_focus, cx);
        }
        cx.notify();
    }

    fn select_model_in_settings(
        &mut self,
        model: Arc<DesktopModel>,
        cx: &mut Context<Self>,
    ) -> bool {
        if !self.apply_model(model.clone()) {
            return false;
        }
        self.preferences.model = model.is_configured().then(|| model.name().to_owned());
        self.preferences.model_connection = self
            .model_settings
            .saved_entry(&model)
            .map(|(id, _)| id.clone());
        self.save_preferences();
        cx.notify();
        true
    }

    pub(super) fn model_settings_view(&self, t: &Theme, cx: &mut Context<Self>) -> Div {
        let p = t.palette;
        let editable = !self.busy && !self.model_settings.pending && !force_mock();
        let text = |label: &str| {
            div()
                .text_size(t.type_scale.secondary)
                .text_color(p.color(p.label_secondary))
                .child(label.to_owned())
        };
        let card = || {
            div()
                .flex()
                .flex_col()
                .min_w(px(0.0))
                .p(px(10.0))
                .gap(px(8.0))
                .border_1()
                .border_color(p.color(p.border_l2))
                .rounded(t.radii.lg)
                .bg(p.color(p.bg_base))
        };
        let panel = div()
            .flex()
            .flex_col()
            .gap(px(10.0))
            .when(self.model_settings.pending, |page| {
                page.child(text("正在访问系统凭据库…"))
            })
            .when(force_mock(), |page| {
                page.child(text("当前以强制模拟模式启动。"))
            })
            .when_some(self.model_settings.notice, |page, notice| {
                let (label, color) = match notice {
                    Ok(label) => (label, p.label_secondary),
                    Err(label) => (label, p.state_error),
                };
                page.child(text(label).text_color(p.color(color)))
            });
        if let Some(editor) = &self.model_settings.editor {
            return panel.child(card().child(self.model_connection_form(
                editor,
                self.model_settings.saved.contains_key(&editor.id),
                editable,
                t,
                cx,
            )));
        }
        let connections = std::iter::once(&ConnectionId::DeepSeek).chain(
            self.model_settings
                .saved
                .keys()
                .filter(|id| **id != ConnectionId::DeepSeek),
        );
        panel
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .items_center()
                    .justify_between()
                    .gap(px(8.0))
                    .child(
                        div()
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(p.color(p.label_primary))
                            .child("模型连接"),
                    )
                    .child(
                        chrome_button("add-openai-model", "＋ 添加 OpenAI 兼容模型", t)
                            .when(editable, |button| {
                                button
                                    .focusable()
                                    .on_click(cx.listener(|chat, _, window, cx| {
                                        chat.add_openai_model(None, window, cx);
                                    }))
                            })
                            .when(!editable, |button| button.opacity(0.5).cursor_default()),
                    ),
            )
            .children(connections.map(|id| {
                let saved = self.model_settings.saved.get(id);
                let detail = if let Some(saved) = saved {
                    saved.base_url.as_str()
                } else if self
                    .model_settings
                    .env_models
                    .iter()
                    .any(|model| model.kind() == id.kind())
                {
                    "环境配置"
                } else {
                    "待配置"
                };
                let name = saved.map_or(id.kind().label(), |saved| saved.model.name());
                let key = id.key();
                let edit_id = id.clone();
                let copy_id = id.clone();
                card().child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(8.0))
                        .child(
                            div()
                                .flex_1()
                                .min_w(px(0.0))
                                .child(
                                    div()
                                        .text_size(t.type_scale.body)
                                        .font_weight(FontWeight::MEDIUM)
                                        .text_color(p.color(p.label_primary))
                                        .overflow_hidden()
                                        .whitespace_nowrap()
                                        .text_ellipsis()
                                        .child(name.to_owned()),
                                )
                                .child(
                                    text(detail)
                                        .overflow_hidden()
                                        .whitespace_nowrap()
                                        .text_ellipsis(),
                                ),
                        )
                        .when(id.kind() == ModelKind::OpenAi, |row| {
                            row.child(
                                chrome_button(format!("add-model-{key}"), "添加模型", t)
                                    .flex_none()
                                    .when(editable, |button| {
                                        button.focusable().on_click(cx.listener(
                                            move |chat, _, window, cx| {
                                                chat.add_openai_model(
                                                    Some(copy_id.clone()),
                                                    window,
                                                    cx,
                                                );
                                            },
                                        ))
                                    })
                                    .when(!editable, |button| button.opacity(0.5).cursor_default()),
                            )
                        })
                        .child(
                            chrome_button(format!("edit-model-{key}"), "编辑", t)
                                .flex_none()
                                .when(editable, |button| {
                                    button.focusable().on_click(cx.listener(
                                        move |chat, _, window, cx| {
                                            chat.open_model_editor(
                                                edit_id.clone(),
                                                None,
                                                window,
                                                cx,
                                            );
                                        },
                                    ))
                                })
                                .when(!editable, |button| button.opacity(0.5).cursor_default()),
                        ),
                )
            }))
            .child(
                div()
                    .mt(px(6.0))
                    .font_weight(FontWeight::MEDIUM)
                    .text_size(t.type_scale.body)
                    .text_color(p.color(p.label_primary))
                    .child("会话模型"),
            )
            .child(
                card()
                    .gap(px(4.0))
                    .when(
                        self.model_options.is_empty() && !self.model_settings.pending,
                        |options| options.child(text("尚无可用模型")),
                    )
                    .children(self.model_options.iter().enumerate().map(|(index, model)| {
                        let option = model.clone();
                        let selected = Arc::ptr_eq(model, &self.settings.model);
                        chrome_button(("settings-model", index), "", t)
                            .aria_label(model.name().to_owned())
                            .aria_selected(selected)
                            .h(px(40.0))
                            .w_full()
                            .justify_between()
                            .gap(px(6.0))
                            .child(
                                div()
                                    .flex_1()
                                    .min_w(px(0.0))
                                    .overflow_hidden()
                                    .whitespace_nowrap()
                                    .text_ellipsis()
                                    .child(model.name().to_owned()),
                            )
                            .when(model.kind() != ModelKind::Mock, |button| {
                                button.child(
                                    text(self.model_settings.source(model))
                                        .max_w(relative(0.5))
                                        .min_w(px(0.0))
                                        .overflow_hidden()
                                        .whitespace_nowrap()
                                        .text_ellipsis(),
                                )
                            })
                            .when(selected, |button| button.child("✓"))
                            .when(!self.busy && !self.model_settings.pending, |button| {
                                button
                                    .focusable()
                                    .on_click(cx.listener(move |chat, _, _, cx| {
                                        chat.select_model_in_settings(option.clone(), cx);
                                    }))
                            })
                    })),
            )
    }

    fn model_connection_form(
        &self,
        editor: &Editor,
        has_key: bool,
        editable: bool,
        t: &Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let p = t.palette;
        let id = editor.id.clone();
        let field = |label, input: &Entity<TextInput>| {
            div()
                .flex()
                .flex_col()
                .gap(px(3.0))
                .min_w(px(0.0))
                .child(div().text_size(t.type_scale.secondary).child(label))
                .child(
                    div()
                        .flex()
                        .items_center()
                        .min_h(px(30.0))
                        .overflow_hidden()
                        .px(px(8.0))
                        .border_1()
                        .border_color(p.color(p.border_l2))
                        .rounded(t.radii.sm)
                        .bg(p.color(p.input_major))
                        .child(input.clone()),
                )
        };
        div()
            .flex()
            .flex_col()
            .gap(px(5.0))
            .text_color(p.color(p.label_primary))
            .child(div().font_weight(FontWeight::MEDIUM).child(
                if has_key || id.kind() == ModelKind::DeepSeek {
                    id.kind().label()
                } else {
                    "添加 OpenAI 兼容模型"
                },
            ))
            .child(field("API 根地址", &editor.base_url))
            .child(field("模型 ID", &editor.model_id))
            .child(field("API Key", &editor.api_key))
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .items_center()
                    .gap(px(6.0))
                    .justify_end()
                    .when(has_key && editable, |actions| {
                        actions.child(
                            chrome_button(
                                "remove-model-connection",
                                if editor.confirm_remove {
                                    "确认移除"
                                } else {
                                    "移除配置"
                                },
                                t,
                            )
                            .focusable()
                            .text_color(p.color(p.state_error))
                            .on_click(cx.listener(
                                move |chat, _, window, cx| {
                                    if let Some(editor) = chat.model_settings.editor.as_mut() {
                                        if editor.confirm_remove {
                                            chat.remove_model_connection(id.clone(), window, cx);
                                        } else {
                                            editor.confirm_remove = true;
                                            cx.notify();
                                        }
                                    }
                                },
                            )),
                        )
                    })
                    .child(
                        chrome_button("cancel-model-connection", "取消", t)
                            .focusable()
                            .on_click(cx.listener(|chat, _, window, cx| {
                                chat.close_model_editor(window, cx)
                            })),
                    )
                    .child(
                        chrome_button("save-model-connection", "保存并使用", t)
                            .bg(p.color(p.business_tertiary))
                            .text_color(p.color(p.link))
                            .when(editable, |button| {
                                button
                                    .focusable()
                                    .on_click(cx.listener(|chat, _, window, cx| {
                                        chat.save_model_connection(window, cx);
                                    }))
                            })
                            .when(!editable, |button| button.opacity(0.5).cursor_default()),
                    ),
            )
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_compatible_endpoint_and_rejects_embedded_credentials() {
        assert_eq!(
            base_url("http://127.0.0.1:8080/v1/chat/completions/").unwrap(),
            "http://127.0.0.1:8080/v1",
        );
        for invalid in [
            "https://secret@example.com/v1",
            "https://example.com/v1?key=secret",
            "https://example.com/v1#token",
            "file:///tmp/model",
        ] {
            assert!(base_url(invalid).is_err());
        }
    }

    #[test]
    fn credential_reuse_requires_the_same_origin() {
        let previous = "https://example.com/v1";
        assert!(same_origin(previous, "https://EXAMPLE.com:443/v2"));
        for other in [
            "https://other.example.com/v1",
            "http://example.com/v1",
            "https://example.com:8443/v1",
            "invalid",
        ] {
            assert!(!same_origin(previous, other));
        }
    }

    #[test]
    fn desktop_without_model_options_stays_unconfigured() {
        let settings = ModelSettings::new(Path::new("fixture"), vec![]);
        let options = settings.options();
        assert!(options.is_empty());
        assert!(!DesktopSettings::from_env(&options).model.is_configured());
    }

    #[test]
    fn saved_model_options_keep_environment_and_mock_entries() {
        let mock = Arc::new(DesktopModel::Mock);
        let env = Arc::new(DesktopModel::OpenAi(OpenAiProvider::new("fixture-env-key")));
        let mut settings =
            ModelSettings::new(Path::new("fixture"), vec![env.clone(), mock.clone()]);
        let stored = Connection {
            base_url: "http://127.0.0.1:8080/v1".to_owned(),
            model_id: "fixture/model".to_owned(),
            api_key: "fixture-secret".to_owned(),
        };
        let json = serde_json::to_vec(&stored).unwrap();
        let saved = Connection::decode(&json)
            .unwrap()
            .into_model(&ConnectionId::OpenAi);
        assert_eq!(saved.model.name(), "fixture/model");
        settings.saved.insert(ConnectionId::OpenAi, saved);
        let second = ConnectionId::new_openai().unwrap();
        settings.saved.insert(
            second.clone(),
            Connection {
                base_url: "http://127.0.0.1:8081/v1".to_owned(),
                ..stored
            }
            .into_model(&second),
        );
        let options = settings.options();
        assert!(Arc::ptr_eq(&options[0], &env));
        assert_eq!(
            settings.saved_entry(&options[1]).unwrap().0,
            &ConnectionId::OpenAi
        );
        assert_eq!(settings.saved_entry(&options[2]).unwrap().0, &second);
        assert_eq!(options[1].name(), options[2].name());
        assert_ne!(settings.source(&options[1]), settings.source(&options[2]));
        assert!(Arc::ptr_eq(&options[3], &mock));
        settings.saved.remove(&ConnectionId::OpenAi);
        assert!(Arc::ptr_eq(&settings.options()[1], &options[2]));
    }

    #[test]
    fn connection_ids_preserve_legacy_selection_and_vault_keys() {
        for (json, id, key) in [
            (r#""deepseek""#, ConnectionId::DeepSeek, "0"),
            (r#""openai""#, ConnectionId::OpenAi, "1"),
        ] {
            assert_eq!(serde_json::from_str::<ConnectionId>(json).unwrap(), id);
            assert_eq!(serde_json::to_string(&id).unwrap(), json);
            assert_eq!(id.key(), key);
        }
        let one = ConnectionId::new_openai().unwrap();
        let two = ConnectionId::new_openai().unwrap();
        assert_ne!(one, two);
        assert_eq!(ConnectionId::from_marker(one.key()), Some(one.clone()));
        assert_eq!(
            serde_json::from_value::<ConnectionId>(serde_json::to_value(&one).unwrap()).unwrap(),
            one
        );
        assert!(ConnectionId::from_marker("../1").is_none());
    }
}
