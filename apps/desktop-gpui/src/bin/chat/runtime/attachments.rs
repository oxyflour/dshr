use crate::MAX_ATTACHMENTS;
use dshr_agent_context::{Retention, SpillRef, SpillStore};
use dshr_agent_core::AttachmentFormat;
use serde_json::json;
use std::io::Read;
use std::path::PathBuf;
use std::sync::Arc;

pub(crate) struct DraftAttachment {
    pub(crate) name: String,
    pub(crate) content: AttachmentContent,
}

impl DraftAttachment {
    pub(crate) fn new(name: String, content: AttachmentContent) -> Self {
        Self { name, content }
    }
}

pub(crate) enum AttachmentContent {
    Text(String),
    Media {
        format: AttachmentFormat,
        bytes: Arc<[u8]>,
    },
}

pub(crate) enum AttachmentSource {
    Picker,
    Paths(Vec<PathBuf>),
    ClipboardImage(Vec<u8>),
}

fn attachment_format(bytes: &[u8]) -> Option<AttachmentFormat> {
    if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        Some(AttachmentFormat::Png)
    } else if bytes.starts_with(&[0xff, 0xd8, 0xff]) {
        Some(AttachmentFormat::Jpeg)
    } else if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
        Some(AttachmentFormat::Gif)
    } else if bytes.starts_with(b"RIFF") && bytes.get(8..12) == Some(b"WEBP") {
        Some(AttachmentFormat::Webp)
    } else if bytes.starts_with(b"%PDF-") {
        Some(AttachmentFormat::Pdf)
    } else {
        None
    }
}

pub(crate) fn read_attachments(
    paths: Vec<PathBuf>,
    remaining_files: usize,
    mut remaining_text: usize,
    mut remaining_media: usize,
) -> Result<Vec<DraftAttachment>, String> {
    if paths.len() > remaining_files {
        return Err(format!("每条消息最多添加 {MAX_ATTACHMENTS} 个附件。"));
    }
    paths
        .into_iter()
        .map(|path| {
            let name = path
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned();
            let read_error = |error: std::io::Error| format!("无法读取 {name}：{error}");
            if !path.metadata().map_err(read_error)?.is_file() {
                return Err(format!("{name} 不是普通文件。"));
            }
            let file = std::fs::File::open(&path).map_err(read_error)?;
            let max_bytes = remaining_text.max(remaining_media);
            let mut bytes = Vec::new();
            file.take(max_bytes as u64 + 1)
                .read_to_end(&mut bytes)
                .map_err(read_error)?;
            if bytes.len() > max_bytes {
                return Err(format!("{name} 超出剩余附件大小。"));
            }
            let content = if let Some(format) = attachment_format(&bytes) {
                if bytes.len() > remaining_media {
                    return Err("图片和 PDF 附件总大小不能超过 12 MiB。".to_owned());
                }
                remaining_media -= bytes.len();
                AttachmentContent::Media {
                    format,
                    bytes: bytes.into(),
                }
            } else {
                if bytes.len() > remaining_text {
                    return Err("文本附件总大小不能超过 32 KiB。".to_owned());
                }
                remaining_text -= bytes.len();
                let text = String::from_utf8(bytes)
                    .ok()
                    .filter(|text| !text.contains('\0'))
                    .ok_or_else(|| format!("{name} 不是受支持的图片、PDF 或 UTF-8 文本文件。"))?;
                AttachmentContent::Text(text)
            };
            Ok(DraftAttachment::new(name, content))
        })
        .collect()
}

pub(crate) fn read_clipboard_image(
    mut bytes: Vec<u8>,
    remaining: usize,
) -> Result<DraftAttachment, String> {
    if bytes.starts_with(b"BM") {
        use image::{DynamicImage, ImageDecoder, ImageFormat, ImageReader};
        use std::io::Cursor;

        let decode_error = |error: image::ImageError| format!("无法读取剪贴板图片：{error}");
        let decoder = ImageReader::with_format(Cursor::new(&bytes), ImageFormat::Bmp)
            .into_decoder()
            .map_err(decode_error)?;
        let (width, height) = decoder.dimensions();
        if u64::from(width) * u64::from(height) > 16 * 1024 * 1024 {
            return Err("剪贴板图片像素过多，请缩小图片。".to_owned());
        }
        let bitmap = DynamicImage::from_decoder(decoder).map_err(decode_error)?;
        let mut output = Cursor::new(Vec::new());
        bitmap
            .write_to(&mut output, ImageFormat::Png)
            .map_err(decode_error)?;
        bytes = output.into_inner();
    }
    if bytes.len() > remaining {
        return Err("图片和 PDF 附件总大小不能超过 12 MiB。".to_owned());
    }
    let (format, extension) = match attachment_format(&bytes) {
        Some(AttachmentFormat::Png) => (AttachmentFormat::Png, "png"),
        Some(AttachmentFormat::Jpeg) => (AttachmentFormat::Jpeg, "jpg"),
        Some(AttachmentFormat::Gif) => (AttachmentFormat::Gif, "gif"),
        Some(AttachmentFormat::Webp) => (AttachmentFormat::Webp, "webp"),
        _ => return Err("剪贴板图片格式不受支持。".to_owned()),
    };
    Ok(DraftAttachment::new(
        format!("剪贴板图片.{extension}"),
        AttachmentContent::Media {
            format,
            bytes: bytes.into(),
        },
    ))
}

pub(crate) fn user_message_payload(
    id: &str,
    text: &str,
    attachments: &[DraftAttachment],
    store: &SpillStore,
) -> Result<(serde_json::Value, Vec<SpillRef>), String> {
    if attachments.is_empty() {
        return Ok((json!({ "message_id": id, "text": text }), Vec::new()));
    }
    let mut content = vec![json!({ "type": "text", "text": text })];
    let mut objects = Vec::new();
    for attachment in attachments {
        match &attachment.content {
            AttachmentContent::Text(text) => content.push(json!({
                "type": "text",
                "attachment_name": attachment.name,
                "text": format!("附件 {}：\n{text}", json!(&attachment.name)),
            })),
            AttachmentContent::Media { format, bytes } => {
                let blob = SpillRef::from_payload(bytes, format.mime(), Retention::Session);
                store
                    .put(&blob, bytes)
                    .map_err(|err| format!("无法保存附件 {}：{err}", attachment.name))?;
                content.push(json!({
                    "type": "attachment",
                    "attachment_name": attachment.name,
                    "mime": blob.mime,
                    "$attachment": blob.id,
                }));
                objects.push(blob);
            }
        }
    }
    Ok((json!({ "message_id": id, "content": content }), objects))
}
