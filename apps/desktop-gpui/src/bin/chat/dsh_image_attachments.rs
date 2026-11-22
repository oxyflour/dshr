use std::collections::HashMap;
use std::fs::{self, File};
use std::io::{Cursor, Read};
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;

use base64::{engine::general_purpose::STANDARD, Engine as _};
use dshr_agent_context::spill::{Retention, SpillRef, SpillStore, EVENT_SPILL_CREATED};
use dshr_compat_dsh_host::{
    HostService, HostServiceKind, HostServiceMethod, HostServiceTable, ServiceCallContext,
    ServiceError, SessionAccess, IMAGE_ATTACHMENTS_V1,
};
use image::{imageops::FilterType, DynamicImage, ImageDecoder, ImageFormat, ImageReader, Limits};
use serde::Deserialize;
use serde_json::{json, Value};

use super::ChatSessionAccess;

const MAX_BYTES: u64 = 8 * 1024 * 1024;
const MAX_DIMENSION: u32 = 8192;
const MAX_PIXELS: u64 = 16 * 1024 * 1024;
const MAX_OUTPUT_DIMENSION: u32 = 2048;
const MAX_RENDERED_COUNT: usize = 20;
const MAX_RENDERED_PAGE: usize = 48 * 1024;

pub(super) struct ImageAttachmentService {
    table: HostServiceTable,
    root: PathBuf,
    store: SpillStore,
    access: Arc<ChatSessionAccess>,
    rendered: HashMap<String, RenderTransfer>,
}

#[derive(Deserialize)]
struct SaveArgs {
    path: PathBuf,
    sha256: String,
    media_type: String,
}

#[derive(Default)]
struct RenderTransfer {
    pending: Vec<u8>,
    input_bytes: u64,
    stored_bytes: u64,
    saved: usize,
}

#[derive(Deserialize)]
struct RenderPage {
    offset: usize,
    b64: String,
    done: bool,
    name: Option<String>,
}

impl ImageAttachmentService {
    pub(super) fn new(
        workspace: &Path,
        store: SpillStore,
        access: Arc<ChatSessionAccess>,
    ) -> std::io::Result<Self> {
        Ok(Self {
            table: HostServiceTable::new(
                IMAGE_ATTACHMENTS_V1,
                vec![
                    method(
                        "route",
                        HostServiceKind::Read,
                        5000,
                        json!({ "type": "object", "additionalProperties": false, "properties": {} }),
                        json!({
                            "type": "object", "additionalProperties": false,
                            "required": ["image"], "properties": { "image": { "type": "boolean" } },
                        }),
                    ),
                    method(
                        "save",
                        HostServiceKind::Write,
                        10000,
                        json!({
                            "type": "object", "additionalProperties": false,
                            "required": ["path", "sha256", "media_type"],
                            "properties": {
                                "path": { "type": "string" },
                                "sha256": { "type": "string" },
                                "media_type": { "type": "string" },
                            },
                        }),
                        json!({
                            "type": "object", "additionalProperties": false,
                            "required": ["attachmentId", "mediaType", "bytes", "width", "height", "name"],
                            "properties": {
                                "attachmentId": { "type": "string" },
                                "mediaType": { "type": "string" },
                                "name": { "type": "string" },
                                "bytes": { "type": "integer" },
                                "width": { "type": "integer" },
                                "height": { "type": "integer" },
                                "originalDimensions": {
                                    "type": "object", "additionalProperties": false,
                                    "required": ["width", "height"],
                                    "properties": {
                                        "width": { "type": "integer" },
                                        "height": { "type": "integer" },
                                    },
                                },
                            },
                        }),
                    ),
                    method(
                        "rendered.page",
                        HostServiceKind::Write,
                        10000,
                        json!({
                            "type": "object", "additionalProperties": false,
                            "required": ["offset", "b64", "done"],
                            "properties": {
                                "offset": { "type": "integer", "minimum": 0, "maximum": MAX_BYTES },
                                "b64": { "type": "string", "minLength": 1, "maxLength": MAX_RENDERED_PAGE * 4 / 3 },
                                "done": { "type": "boolean" },
                                "name": { "type": "string", "minLength": 1, "maxLength": 256 },
                            },
                        }),
                        json!({
                            "type": "object", "additionalProperties": false,
                            "properties": { "reference": { "type": "object" } },
                        }),
                    ),
                ],
            ),
            root: workspace.canonicalize()?,
            store,
            access,
            rendered: HashMap::new(),
        })
    }

    fn save(&self, session: &str, args: SaveArgs) -> Result<Value, ServiceError> {
        if !self.access.image_input(session)? {
            return Err(ServiceError::new(
                "E_SERVICE_DENIED",
                "current model does not accept images",
            ));
        }
        let name = args
            .path
            .file_name()
            .and_then(|part| part.to_str())
            .ok_or_else(|| {
                ServiceError::args("/args/path", "expected a workspace-relative file")
            })?;
        if args
            .path
            .components()
            .any(|part| !matches!(part, Component::Normal(_)))
        {
            return Err(ServiceError::args(
                "/args/path",
                "expected a workspace-relative file",
            ));
        }
        let path = self
            .root
            .join(&args.path)
            .canonicalize()
            .map_err(image_read_error)?;
        if !path.starts_with(&self.root) || path == self.root {
            return Err(ServiceError::new(
                "E_SERVICE_DENIED",
                "image path escapes the workspace",
            ));
        }
        if !fs::metadata(&path).map_err(image_read_error)?.is_file() {
            return Err(attachment_error(
                "INVALID_IMAGE",
                "image path must be a regular file",
            ));
        }
        let mut bytes = Vec::new();
        File::open(path)
            .map_err(image_read_error)?
            .take(MAX_BYTES + 1)
            .read_to_end(&mut bytes)
            .map_err(image_read_error)?;
        if bytes.len() as u64 > MAX_BYTES {
            return Err(attachment_error(
                "IMAGE_TOO_LARGE",
                "image exceeds the 8 MiB attachment limit",
            ));
        }
        self.store_image(
            session,
            name,
            &args.media_type,
            bytes,
            Some(&args.sha256),
            MAX_BYTES,
        )
    }

    fn store_image(
        &self,
        session: &str,
        name: &str,
        media_type: &str,
        bytes: Vec<u8>,
        expected_hash: Option<&str>,
        remaining: u64,
    ) -> Result<Value, ServiceError> {
        let original = SpillRef::from_payload(&bytes, media_type, Retention::Session);
        if expected_hash.is_some_and(|hash| original.hash != hash) {
            return Err(attachment_error(
                "INVALID_IMAGE",
                "image changed while it was being read",
            ));
        }
        let decoder = checked_decoder(&bytes, media_type)?;
        let (width, height) = decoder.dimensions();
        let image = DynamicImage::from_decoder(decoder).map_err(invalid_image)?;

        let (bytes, spill, dimensions, original_dimensions) =
            if width.max(height) > MAX_OUTPUT_DIMENSION {
                let scaled = image.resize(
                    MAX_OUTPUT_DIMENSION,
                    MAX_OUTPUT_DIMENSION,
                    FilterType::Triangle,
                );
                let mut out = Cursor::new(Vec::new());
                scaled
                    .write_to(&mut out, ImageFormat::Png)
                    .map_err(image_write_error)?;
                let bytes = out.into_inner();
                if bytes.len() as u64 > MAX_BYTES {
                    return Err(attachment_error(
                        "IMAGE_TOO_LARGE",
                        "scaled image exceeds the attachment limit",
                    ));
                }
                let spill = SpillRef::from_payload(&bytes, "image/png", Retention::Session);
                (
                    bytes,
                    spill,
                    (scaled.width(), scaled.height()),
                    Some(json!({ "width": width, "height": height })),
                )
            } else {
                (bytes, original, (width, height), None)
            };
        if spill.size > remaining {
            return Err(attachment_error(
                "IMAGE_TOO_LARGE",
                "image exceeds the remaining attachment budget",
            ));
        }
        self.access.image_input(session)?;
        self.store.put(&spill, &bytes).map_err(image_write_error)?;
        self.access.image_input(session)?;
        if !self.access.has_image(session, &spill.id) {
            let mut record = spill.user_attachment_payload();
            record["kind"] = json!("tool.image");
            self.access
                .append_event(session, EVENT_SPILL_CREATED, record)?;
        }

        let mut reference = json!({
            "attachmentId": spill.id, "mediaType": spill.mime, "bytes": spill.size,
            "width": dimensions.0, "height": dimensions.1, "name": name,
        });
        if let Some(original_dimensions) = original_dimensions {
            reference["originalDimensions"] = original_dimensions;
        }
        Ok(reference)
    }

    fn push_rendered_page(
        &mut self,
        session: &str,
        request: &str,
        page: RenderPage,
    ) -> Result<Value, ServiceError> {
        if !self.access.image_input(session)? {
            return Err(ServiceError::new(
                "E_SERVICE_DENIED",
                "current model does not accept images",
            ));
        }
        let name = match (page.done, page.name.as_deref()) {
            (true, Some(name)) if !name.contains(['/', '\\']) && name.ends_with(".png") => name,
            (false, None) => "",
            _ => {
                return Err(ServiceError::args(
                    "/args/name",
                    "only the final page requires a PNG filename",
                ))
            }
        };
        let data = STANDARD
            .decode(page.b64)
            .map_err(|_| ServiceError::args("/args/b64", "expected standard base64"))?;
        if data.is_empty() || data.len() > MAX_RENDERED_PAGE {
            return Err(ServiceError::args(
                "/args/b64",
                "rendered image page is empty or too large",
            ));
        }
        if self.rendered.len() >= 16 && !self.rendered.contains_key(request) {
            return Err(ServiceError::new(
                "E_SERVICE_BUSY",
                "too many active rendered image transfers",
            ));
        }
        let transfer = self.rendered.entry(request.to_owned()).or_default();
        if page.offset != transfer.pending.len() {
            return Err(ServiceError::args(
                "/args/offset",
                "rendered image page is out of order",
            ));
        }
        let total = transfer.input_bytes + data.len() as u64;
        if transfer.saved >= MAX_RENDERED_COUNT || total > MAX_BYTES {
            return Err(attachment_error(
                "IMAGE_TOO_LARGE",
                "rendered image batch exceeds its attachment budget",
            ));
        }
        transfer.input_bytes = total;
        transfer.pending.extend(data);
        if !page.done {
            return Ok(json!({}));
        }
        let image = std::mem::take(&mut transfer.pending);
        let remaining = MAX_BYTES - transfer.stored_bytes;
        let reference = self.store_image(session, name, "image/png", image, None, remaining)?;
        let transfer = self
            .rendered
            .get_mut(request)
            .expect("active render transfer");
        transfer.saved += 1;
        transfer.stored_bytes += reference["bytes"].as_u64().expect("image bytes");
        Ok(json!({ "reference": reference }))
    }
}

impl HostService for ImageAttachmentService {
    fn table(&self) -> &HostServiceTable {
        &self.table
    }

    fn dispatch(
        &mut self,
        name: &str,
        args: &Value,
        context: &ServiceCallContext,
    ) -> Result<Value, ServiceError> {
        let session = context.session_id.as_deref().ok_or_else(|| {
            ServiceError::new(
                "E_SERVICE_ARGS",
                "image attachment service requires a session",
            )
        })?;
        match name {
            "route" => Ok(json!({ "image": self.access.image_input(session)? })),
            "save" => self.save(
                session,
                serde_json::from_value(args.clone())
                    .map_err(|error| ServiceError::args("/args", error.to_string()))?,
            ),
            "rendered.page" => {
                let request = context.tool_request_id.as_deref().ok_or_else(|| {
                    ServiceError::new(
                        "E_SERVICE_DENIED",
                        "rendered images require an active tool call",
                    )
                })?;
                let page = serde_json::from_value(args.clone())
                    .map_err(|error| ServiceError::args("/args", error.to_string()))?;
                self.push_rendered_page(session, request, page)
            }
            _ => Err(ServiceError::new(
                "E_SERVICE_METHOD",
                format!("unknown image method {name}"),
            )),
        }
    }

    fn release_tool_call(&mut self, request_id: &str) {
        self.rendered.remove(request_id);
    }
}

fn method(
    name: &str,
    kind: HostServiceKind,
    timeout_ms: u64,
    args_schema: Value,
    result_schema: Value,
) -> HostServiceMethod {
    HostServiceMethod {
        name: name.to_owned(),
        kind,
        timeout_ms,
        args_schema,
        result_schema,
        idempotent: name != "rendered.page",
        requires_session: true,
    }
}

fn checked_decoder<'a>(
    bytes: &'a [u8],
    media_type: &str,
) -> Result<impl ImageDecoder + 'a, ServiceError> {
    let format = match media_type {
        "image/png" => ImageFormat::Png,
        "image/jpeg" => ImageFormat::Jpeg,
        "image/gif" => ImageFormat::Gif,
        "image/webp" => ImageFormat::WebP,
        _ => {
            return Err(attachment_error(
                "UNSUPPORTED_IMAGE_TYPE",
                "unsupported image media type",
            ))
        }
    };
    if image::guess_format(bytes).map_err(invalid_image)? != format {
        return Err(attachment_error(
            "IMAGE_TYPE_MISMATCH",
            "image bytes do not match the declared format",
        ));
    }
    let mut limits = Limits::default();
    limits.max_image_width = Some(MAX_DIMENSION);
    limits.max_image_height = Some(MAX_DIMENSION);
    limits.max_alloc = Some(128 * 1024 * 1024);
    let mut reader = ImageReader::with_format(Cursor::new(bytes), format);
    reader.limits(limits);
    let decoder = reader.into_decoder().map_err(invalid_image)?;
    let (width, height) = decoder.dimensions();
    if width.max(height) > MAX_DIMENSION {
        return Err(attachment_error(
            "IMAGE_DIMENSION_TOO_LARGE",
            "image exceeds the dimension limit",
        ));
    }
    if u64::from(width) * u64::from(height) > MAX_PIXELS {
        return Err(attachment_error(
            "IMAGE_TOO_MANY_PIXELS",
            "image exceeds the decoded pixel limit",
        ));
    }
    Ok(decoder)
}

fn attachment_error(code: &str, message: impl ToString) -> ServiceError {
    ServiceError::new("E_SERVICE_ARGS", message.to_string())
        .with_detail(json!({ "attachment_code": code }))
}

fn image_read_error(error: std::io::Error) -> ServiceError {
    attachment_error("ATTACHMENT_READ_FAILED", error)
}

fn invalid_image(error: image::ImageError) -> ServiceError {
    if matches!(error, image::ImageError::Limits(_)) {
        attachment_error("IMAGE_DIMENSION_TOO_LARGE", error)
    } else {
        attachment_error("INVALID_IMAGE", error)
    }
}

fn image_write_error(error: impl ToString) -> ServiceError {
    attachment_error("ATTACHMENT_WRITE_FAILED", error)
}
