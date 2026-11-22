use std::future::Future;
use std::io::Cursor;
use std::path::PathBuf;
use std::sync::Arc;

use dshr_agent_context::{SpillRef, SpillStore};
use dshr_agent_core::AttachmentFormat;
use gpui::{App, Asset, RenderImage};
use image::{DynamicImage, Frame, ImageDecoder, ImageFormat, ImageReader, Limits};

pub(super) const THUMBNAIL_EDGE: u32 = 200;
const PREVIEW_EDGE: u32 = 1920;

#[derive(Clone, PartialEq, Eq, Hash)]
pub(super) struct StoredAttachment {
    pub root: PathBuf,
    pub file: SpillRef,
}

impl StoredAttachment {
    fn load(&self, edge: u32) -> Result<Arc<RenderImage>, String> {
        if self.file.size > super::MAX_MEDIA_ATTACHMENT_BYTES as u64 {
            return Err("图片文件过大，无法预览。".to_owned());
        }
        let format = AttachmentFormat::from_mime(&self.file.mime)
            .ok_or_else(|| "图片格式不受支持。".to_owned())?;
        let bytes = SpillStore::new(&self.root)
            .get_verified(&self.file)
            .map_err(|_| "本地附件缺失或已损坏。".to_owned())?;
        decode_image(&bytes, format, edge)
    }
}

pub(super) struct AttachmentThumbnail;

impl Asset for AttachmentThumbnail {
    type Source = StoredAttachment;
    type Output = Result<Arc<RenderImage>, String>;

    #[expect(
        clippy::manual_async_fn,
        reason = "The returned future is static and does not capture the App borrow."
    )]
    fn load(
        source: Self::Source,
        _: &mut App,
    ) -> impl Future<Output = Self::Output> + Send + 'static {
        async move { source.load(THUMBNAIL_EDGE) }
    }
}

pub(super) enum PreviewSource {
    Draft {
        format: AttachmentFormat,
        bytes: Arc<[u8]>,
    },
    Stored(StoredAttachment),
}

impl PreviewSource {
    pub(super) fn load(self) -> Result<Arc<RenderImage>, String> {
        match self {
            Self::Draft { format, bytes } => decode_image(&bytes, format, PREVIEW_EDGE),
            Self::Stored(file) => file.load(PREVIEW_EDGE),
        }
    }
}

pub(super) fn decode_image(
    bytes: &[u8],
    format: AttachmentFormat,
    edge: u32,
) -> Result<Arc<RenderImage>, String> {
    let image_format = match format {
        AttachmentFormat::Png => ImageFormat::Png,
        AttachmentFormat::Jpeg => ImageFormat::Jpeg,
        AttachmentFormat::Gif => ImageFormat::Gif,
        AttachmentFormat::Webp => ImageFormat::WebP,
        AttachmentFormat::Pdf => return Err("图片格式不受支持。".to_owned()),
    };
    let mut reader = ImageReader::with_format(Cursor::new(bytes), image_format);
    let mut limits = Limits::default();
    limits.max_image_width = Some(16_384);
    limits.max_image_height = Some(16_384);
    limits.max_alloc = Some(128 * 1024 * 1024);
    reader.limits(limits);
    let decode_error = |_: image::ImageError| "无法解码图片。".to_owned();
    let mut decoder = reader.into_decoder().map_err(decode_error)?;
    let (width, height) = decoder.dimensions();
    if u64::from(width) * u64::from(height) > 24_000_000 {
        return Err("图片像素过多，无法预览。".to_owned());
    }
    let orientation = decoder.orientation().ok();
    let mut image = DynamicImage::from_decoder(decoder).map_err(decode_error)?;
    if let Some(orientation) = orientation {
        image.apply_orientation(orientation);
    }
    if width.max(height) > edge {
        image = image.thumbnail(edge, edge);
    }
    let mut pixels = image.into_rgba8();
    for pixel in pixels.as_chunks_mut::<4>().0 {
        pixel.swap(0, 2);
    }
    Ok(Arc::new(RenderImage::new([Frame::new(pixels)])))
}
