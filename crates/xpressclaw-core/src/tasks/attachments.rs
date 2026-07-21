use base64::engine::general_purpose::STANDARD;
use base64::Engine as _;
use serde::{Deserialize, Serialize};

pub const MAX_IMAGES_PER_MESSAGE: usize = 5;
pub const MAX_IMAGE_BYTES: usize = 5 * 1024 * 1024;
pub const MAX_TOTAL_IMAGE_BYTES: usize = 20 * 1024 * 1024;

const ALLOWED_IMAGE_TYPES: &[&str] = &["image/png", "image/jpeg", "image/gif", "image/webp"];

/// Base64 image submitted by an API client.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ImageAttachmentInput {
    #[serde(default)]
    pub name: String,
    #[serde(alias = "mimeType")]
    pub mime_type: String,
    pub data: String,
}

/// Validated image bytes ready to persist with a task message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DecodedImageAttachment {
    pub name: String,
    pub mime_type: String,
    pub data: Vec<u8>,
}

/// Validate and decode image inputs before they reach SQLite or ACP.
pub fn decode_image_attachments(
    attachments: &[ImageAttachmentInput],
) -> std::result::Result<Vec<DecodedImageAttachment>, String> {
    if attachments.len() > MAX_IMAGES_PER_MESSAGE {
        return Err(format!(
            "a message can include at most {MAX_IMAGES_PER_MESSAGE} images"
        ));
    }

    let mut total_size = 0usize;
    let mut decoded = Vec::with_capacity(attachments.len());
    for attachment in attachments {
        if !ALLOWED_IMAGE_TYPES.contains(&attachment.mime_type.as_str()) {
            return Err(format!(
                "unsupported image type '{}'; use PNG, JPEG, GIF, or WebP",
                attachment.mime_type
            ));
        }
        if attachment.name.chars().count() > 255 {
            return Err("image filename cannot exceed 255 characters".to_string());
        }

        // Reject oversized encoded values before allocating their decoded form.
        let max_encoded_len = MAX_IMAGE_BYTES.div_ceil(3) * 4 + 4;
        if attachment.data.len() > max_encoded_len {
            return Err(format!(
                "image '{}' exceeds the {} MiB limit",
                display_name(&attachment.name),
                MAX_IMAGE_BYTES / 1024 / 1024
            ));
        }
        let data = STANDARD.decode(&attachment.data).map_err(|_| {
            format!(
                "image '{}' does not contain valid base64 data",
                display_name(&attachment.name)
            )
        })?;
        if data.len() > MAX_IMAGE_BYTES {
            return Err(format!(
                "image '{}' exceeds the {} MiB limit",
                display_name(&attachment.name),
                MAX_IMAGE_BYTES / 1024 / 1024
            ));
        }
        if !matches_image_signature(&attachment.mime_type, &data) {
            return Err(format!(
                "image '{}' does not match its declared {} type",
                display_name(&attachment.name),
                attachment.mime_type
            ));
        }

        total_size = total_size.saturating_add(data.len());
        if total_size > MAX_TOTAL_IMAGE_BYTES {
            return Err(format!(
                "images in one message cannot exceed {} MiB in total",
                MAX_TOTAL_IMAGE_BYTES / 1024 / 1024
            ));
        }
        decoded.push(DecodedImageAttachment {
            name: attachment.name.clone(),
            mime_type: attachment.mime_type.clone(),
            data,
        });
    }
    Ok(decoded)
}

fn display_name(name: &str) -> &str {
    if name.trim().is_empty() {
        "attachment"
    } else {
        name
    }
}

fn matches_image_signature(mime_type: &str, data: &[u8]) -> bool {
    match mime_type {
        "image/png" => data.starts_with(b"\x89PNG\r\n\x1a\n"),
        "image/jpeg" => data.starts_with(&[0xff, 0xd8, 0xff]),
        "image/gif" => data.starts_with(b"GIF87a") || data.starts_with(b"GIF89a"),
        "image/webp" => data.len() >= 12 && data.starts_with(b"RIFF") && &data[8..12] == b"WEBP",
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn png(name: &str) -> ImageAttachmentInput {
        ImageAttachmentInput {
            name: name.to_string(),
            mime_type: "image/png".to_string(),
            data: STANDARD.encode(b"\x89PNG\r\n\x1a\nimage-data"),
        }
    }

    #[test]
    fn decodes_valid_images() {
        let decoded = decode_image_attachments(&[png("screenshot.png")]).unwrap();
        assert_eq!(decoded[0].name, "screenshot.png");
        assert_eq!(decoded[0].mime_type, "image/png");
        assert_eq!(decoded[0].data, b"\x89PNG\r\n\x1a\nimage-data");
    }

    #[test]
    fn rejects_mismatched_or_excess_images() {
        let mut wrong_type = png("not-a-jpeg.jpg");
        wrong_type.mime_type = "image/jpeg".to_string();
        assert!(decode_image_attachments(&[wrong_type])
            .unwrap_err()
            .contains("does not match"));

        let too_many = vec![png("image.png"); MAX_IMAGES_PER_MESSAGE + 1];
        assert!(decode_image_attachments(&too_many)
            .unwrap_err()
            .contains("at most"));
    }
}
