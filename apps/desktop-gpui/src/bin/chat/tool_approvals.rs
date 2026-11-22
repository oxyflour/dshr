use super::{data_root, hex_lower, save_atomic, ChatApp, PendingApproval};
use gpui::{Context, Window};
use ring::digest::{digest, SHA256};
use std::path::PathBuf;

const GRANT: &[u8] = b"dshr-tool-approval-v1";

fn directory() -> PathBuf {
    data_root().join("tool-approvals")
}

fn credential_ref(key: &str) -> String {
    let root = data_root().canonicalize().unwrap_or_else(|_| data_root());
    let namespace = hex_lower(digest(&SHA256, root.to_string_lossy().as_bytes()).as_ref());
    format!("dshr://tool-approvals/{namespace}/{key}")
}

fn registered_keys() -> impl Iterator<Item = String> {
    std::fs::read_dir(directory())
        .into_iter()
        .flatten()
        .filter_map(Result::ok)
        .filter_map(|entry| {
            let name = entry.file_name();
            let key = name.to_str()?.strip_suffix(".allow")?;
            if key.len() != 64
                || !key
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
            {
                return None;
            }
            entry.file_type().ok()?.is_file().then(|| key.to_owned())
        })
}

impl ChatApp {
    pub(super) fn load_tool_approvals(
        &mut self,
        autosend: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let reads: Vec<_> = registered_keys()
            .map(|key| {
                let read = cx.read_credentials(&credential_ref(&key));
                (key, read)
            })
            .collect();
        if reads.is_empty() {
            self.load_model_settings(autosend, window, cx);
            return;
        }
        self.tool_approvals_loading = true;
        cx.spawn_in(window, async move |this, cx| {
            let mut grants = Vec::new();
            for (key, read) in reads {
                if let Ok(Some((_, bytes))) = read.await {
                    if bytes == GRANT {
                        grants.push(key);
                    }
                }
            }
            let _ = this.update_in(cx, |chat, window, cx| {
                chat.approvals.restore_grants(grants);
                chat.tool_approvals_loading = false;
                chat.load_model_settings(autosend, window, cx);
                cx.notify();
            });
        })
        .detach();
    }

    pub(super) fn allow_tool_always(&mut self, request: PendingApproval, cx: &mut Context<Self>) {
        if self.approval_saving.is_some() {
            return;
        }
        let Some(key) = self.approvals.always_allow_key(&request) else {
            return;
        };
        // The marker is a lookup hint. A workspace tool that can write it still
        // cannot grant itself approval without a matching credential.
        if let Err(error) = save_atomic(&directory().join(format!("{key}.allow")), b"") {
            self.show_approval_save_error(request, &error, cx);
            return;
        }
        self.approval_error = None;
        self.approval_saving = Some(request.clone());
        let write = cx.write_credentials(&credential_ref(&key), "DSHR", GRANT);
        cx.notify();
        cx.spawn(async move |this, cx| {
            let result = write.await;
            let _ = this.update(cx, |chat, cx| {
                chat.approval_saving = None;
                match result {
                    Ok(()) => {
                        chat.approvals.resolve_always(&request);
                        chat.pending_approvals = 0;
                        cx.notify();
                    }
                    Err(error) => chat.show_approval_save_error(request, &error, cx),
                }
            });
        })
        .detach();
    }

    fn show_approval_save_error(
        &mut self,
        request: PendingApproval,
        error: &dyn std::fmt::Display,
        cx: &mut Context<Self>,
    ) {
        eprintln!("[chat] cannot save tool approval: {error}");
        self.approval_error = Some((
            request,
            "无法保存授权，可重试或选择「允许一次」。".to_owned(),
        ));
        cx.notify();
    }
}
