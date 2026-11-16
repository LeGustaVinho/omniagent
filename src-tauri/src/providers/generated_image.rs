use base64::{Engine, engine::general_purpose::STANDARD};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

use super::ProviderEventData;

const MAX_BYTES: usize = 8 * 1024 * 1024;
const MAX_DIMENSION: u32 = 8192;
const MAX_PIXELS: u64 = 16 * 1024 * 1024;
const PNG: &[u8] = b"\x89PNG\r\n\x1a\n\x00\x00\x00\x0dIHDR";
const JPEG: &[u8] = b"\xff\xd8\xff";

#[derive(Debug, Clone, Copy, Deserialize, Eq, PartialEq, Serialize)]
pub enum GeneratedImageMime {
    #[serde(rename = "image/png")]
    Png,
    #[serde(rename = "image/jpeg")]
    Jpeg,
}

#[derive(Debug, Clone, Copy, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ImageCaptureIssue {
    GenerationNotCompleted,
    InlineImageUnavailable,
    SizeLimitExceeded,
    DimensionLimitExceeded,
    InvalidInlineImage,
    UnsupportedImageFormat,
}

#[derive(Debug, Clone, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum GeneratedImageCapture {
    Saved(InlineGeneratedImage),
    Unavailable { reason: ImageCaptureIssue },
}

#[derive(Clone, Deserialize, Eq, PartialEq, Serialize)]
pub struct InlineGeneratedImage {
    pub mime: GeneratedImageMime,
    pub byte_len: usize,
    pub sha256: String,
    #[serde(skip)]
    bytes: Vec<u8>,
}

impl std::fmt::Debug for InlineGeneratedImage {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("InlineGeneratedImage")
            .field("mime", &self.mime)
            .field("byte_len", &self.byte_len)
            .field("sha256", &self.sha256)
            .finish_non_exhaustive()
    }
}

pub(crate) fn take_generated_image_bytes(data: &mut ProviderEventData) -> Option<Vec<u8>> {
    let ProviderEventData::MediaGeneration {
        media_type,
        tool,
        image: Some(GeneratedImageCapture::Saved(image)),
        ..
    } = data
    else {
        return None;
    };
    (media_type == "image" && tool == "imageGeneration" && !image.bytes.is_empty())
        .then(|| std::mem::take(&mut image.bytes))
}

pub(crate) fn capture_codex_image(item: &Value) -> GeneratedImageCapture {
    match read_inline_image(item) {
        Ok(image) => GeneratedImageCapture::Saved(image),
        Err(reason) => GeneratedImageCapture::Unavailable { reason },
    }
}

fn read_inline_image(item: &Value) -> Result<InlineGeneratedImage, ImageCaptureIssue> {
    if item["status"] != "completed" || !item["failure"].is_null() {
        return Err(ImageCaptureIssue::GenerationNotCompleted);
    }
    // The authenticated App Server returned raw inline base64. Files may live outside the
    // checkout, so this evidence path never reads a savedPath or resolves an opaque fileId.
    let content = item["result"]
        .as_str()
        .filter(|value| !value.is_empty())
        .ok_or(ImageCaptureIssue::InlineImageUnavailable)?;
    if content.len() > MAX_BYTES.div_ceil(3) * 4 {
        return Err(ImageCaptureIssue::SizeLimitExceeded);
    }
    let bytes = STANDARD
        .decode(content)
        .map_err(|_| ImageCaptureIssue::InvalidInlineImage)?;
    if bytes.len() > MAX_BYTES {
        return Err(ImageCaptureIssue::SizeLimitExceeded);
    }
    let (mime, dimensions) = if bytes.starts_with(PNG) && bytes.ends_with(b"IEND\xaeB`\x82") {
        (GeneratedImageMime::Png, png_dimensions(&bytes))
    } else if bytes.starts_with(JPEG) && bytes.ends_with(b"\xff\xd9") {
        (GeneratedImageMime::Jpeg, jpeg_dimensions(&bytes))
    } else {
        return Err(ImageCaptureIssue::UnsupportedImageFormat);
    };
    let (width, height) = dimensions.ok_or(ImageCaptureIssue::InvalidInlineImage)?;
    if width == 0 || height == 0 {
        return Err(ImageCaptureIssue::InvalidInlineImage);
    }
    if width > MAX_DIMENSION
        || height > MAX_DIMENSION
        || u64::from(width) * u64::from(height) > MAX_PIXELS
    {
        return Err(ImageCaptureIssue::DimensionLimitExceeded);
    }
    Ok(InlineGeneratedImage {
        mime,
        byte_len: bytes.len(),
        sha256: format!("{:x}", Sha256::digest(&bytes)),
        bytes,
    })
}

fn png_dimensions(bytes: &[u8]) -> Option<(u32, u32)> {
    let width = u32::from_be_bytes(bytes.get(16..20)?.try_into().ok()?);
    let height = u32::from_be_bytes(bytes.get(20..24)?.try_into().ok()?);
    Some((width, height))
}

fn jpeg_dimensions(mut bytes: &[u8]) -> Option<(u32, u32)> {
    bytes = bytes.strip_prefix(b"\xff\xd8")?;
    loop {
        bytes = bytes.strip_prefix(b"\xff")?;
        let marker = bytes.iter().position(|byte| *byte != 0xff)?;
        let (&marker, rest) = bytes.get(marker..)?.split_first()?;
        bytes = rest;
        match marker {
            // The temporary private marker has no length field.
            0x01 => continue,
            // Scan data and restart/end markers cannot precede the frame header.
            0x00 | 0xd0..=0xda => return None,
            _ => {}
        }
        let length = u16::from_be_bytes(bytes.get(..2)?.try_into().ok()?) as usize;
        let segment = bytes.get(2..length)?;
        if matches!(marker, 0xc0..=0xc3 | 0xc5..=0xc7 | 0xc9..=0xcb | 0xcd..=0xcf) {
            let height = u16::from_be_bytes(segment.get(1..3)?.try_into().ok()?);
            let width = u16::from_be_bytes(segment.get(3..5)?.try_into().ok()?);
            return Some((u32::from(width), u32::from(height)));
        }
        bytes = bytes.get(length..)?;
    }
}

#[cfg(test)]
pub(crate) const TINY_PNG_BASE64: &str =
    "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+jH1sAAAAASUVORK5CYII=";

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn captures_bounded_inline_images_only() {
        let png =
            read_inline_image(&json!({"status":"completed","result":TINY_PNG_BASE64})).unwrap();
        assert_eq!(png.mime, GeneratedImageMime::Png);
        assert_eq!(
            png.byte_len,
            STANDARD.decode(TINY_PNG_BASE64).unwrap().len()
        );
        assert_eq!(png.sha256.len(), 64);
        assert!(!format!("{png:?}").contains(TINY_PNG_BASE64));
        assert!(!serde_json::to_string(&png).unwrap().contains("bytes"));
        assert_eq!(
            read_inline_image(
                &json!({"status":"completed","savedPath":"private.png","fileId":"file"})
            ),
            Err(ImageCaptureIssue::InlineImageUnavailable)
        );
        assert_eq!(
            read_inline_image(&json!({"status":"failed","result":TINY_PNG_BASE64})),
            Err(ImageCaptureIssue::GenerationNotCompleted)
        );
        assert_eq!(
            read_inline_image(&json!({"status":"completed","result":"invalid base64!"})),
            Err(ImageCaptureIssue::InvalidInlineImage)
        );
        assert_eq!(
            read_inline_image(&json!({"status":"completed","result":"PHN2Zy8+"})),
            Err(ImageCaptureIssue::UnsupportedImageFormat)
        );
        assert_eq!(
            read_inline_image(
                &json!({"status":"completed","result":"A".repeat(MAX_BYTES.div_ceil(3)*4+1)})
            ),
            Err(ImageCaptureIssue::SizeLimitExceeded)
        );
    }

    #[test]
    fn checks_dimensions_in_png_and_jpeg_headers() {
        let mut png = STANDARD.decode(TINY_PNG_BASE64).unwrap();
        png[16..20].copy_from_slice(&(MAX_DIMENSION + 1).to_be_bytes());
        assert_eq!(
            read_inline_image(&json!({"status":"completed","result":STANDARD.encode(png)})),
            Err(ImageCaptureIssue::DimensionLimitExceeded)
        );
        let jpg_header = b"\xff\xd8\xff\xe0\x00\x04AB\xff\xc0\x00\x0b\x08\x00\x02\x00\x03\x01\x01\x11\x00\xff\xd9";
        assert_eq!(jpeg_dimensions(jpg_header), Some((3, 2)));
        assert_eq!(jpeg_dimensions(b"\xff\xd8\xff\xc0\x00\x02"), None);
    }
}
