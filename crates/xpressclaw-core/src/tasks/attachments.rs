use base64::engine::general_purpose::STANDARD;
use base64::Engine as _;
use serde::{Deserialize, Serialize};

pub const MAX_IMAGES_PER_MESSAGE: usize = 5;
pub const MAX_IMAGE_BYTES: usize = 5 * 1024 * 1024;
pub const MAX_TOTAL_IMAGE_BYTES: usize = 20 * 1024 * 1024;

const ALLOWED_IMAGE_TYPES: &[&str] = &["image/png", "image/jpeg", "image/gif", "image/webp"];

/// Base64 attachment submitted by an API client (legacy type name).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ImageAttachmentInput {
    #[serde(default)]
    pub name: String,
    #[serde(alias = "mimeType")]
    pub mime_type: String,
    pub data: String,
}

/// Validated attachment bytes ready to persist with a task message.
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
    if let Some(attachment) = attachments
        .iter()
        .find(|attachment| !ALLOWED_IMAGE_TYPES.contains(&attachment.mime_type.as_str()))
    {
        return Err(format!("unsupported image type '{}'", attachment.mime_type));
    }
    decode_attachments(attachments)
}

/// Decode uploads of any file type; only supported raster formats become ACP images.
pub fn decode_attachments(
    attachments: &[ImageAttachmentInput],
) -> std::result::Result<Vec<DecodedImageAttachment>, String> {
    if attachments.len() > MAX_IMAGES_PER_MESSAGE {
        return Err(format!(
            "a message can include at most {MAX_IMAGES_PER_MESSAGE} files"
        ));
    }

    let mut total_size = 0usize;
    let mut decoded = Vec::with_capacity(attachments.len());
    for attachment in attachments {
        if attachment.mime_type.len() > 128
            || !attachment.mime_type.contains('/')
            || !attachment
                .mime_type
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || b"/!#$&^_.+-".contains(&byte))
        {
            return Err("invalid attachment MIME type".into());
        }
        let image = is_prompt_image(&attachment.mime_type);
        let limit = if image {
            MAX_IMAGE_BYTES
        } else {
            MAX_TOTAL_IMAGE_BYTES
        };
        if attachment.name.chars().count() > 255 {
            return Err("attachment filename cannot exceed 255 characters".to_string());
        }

        // Reject oversized encoded values before allocating their decoded form.
        let max_encoded_len = limit.div_ceil(3) * 4 + 4;
        if attachment.data.len() > max_encoded_len {
            return Err(format!(
                "attachment '{}' exceeds the {} MiB limit",
                display_name(&attachment.name),
                limit / 1024 / 1024
            ));
        }
        let data = STANDARD.decode(&attachment.data).map_err(|_| {
            format!(
                "attachment '{}' does not contain valid base64 data",
                display_name(&attachment.name)
            )
        })?;
        if data.len() > limit {
            return Err(format!(
                "attachment '{}' exceeds the {} MiB limit",
                display_name(&attachment.name),
                limit / 1024 / 1024
            ));
        }
        if image && !matches_image_signature(&attachment.mime_type, &data) {
            return Err(format!(
                "image '{}' does not match its declared {} type",
                display_name(&attachment.name),
                attachment.mime_type
            ));
        }

        total_size = total_size.saturating_add(data.len());
        if total_size > MAX_TOTAL_IMAGE_BYTES {
            return Err(format!(
                "files in one message cannot exceed {} MiB in total",
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

pub fn is_prompt_image(mime_type: &str) -> bool {
    ALLOWED_IMAGE_TYPES.contains(&mime_type)
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

    #[test]
    fn general_files_preserve_bytes_and_validate_mime_headers() {
        let mut file = ImageAttachmentInput {
            name: "data.bin".into(),
            mime_type: "application/octet-stream".into(),
            data: STANDARD.encode([0, 255, 1]),
        };
        assert_eq!(
            decode_attachments(&[file.clone()]).unwrap()[0].data,
            [0, 255, 1]
        );
        assert!(decode_image_attachments(&[file.clone()]).is_err());
        file.mime_type = "text/plain\r\nx-injected: yes".into();
        assert!(decode_attachments(&[file]).is_err());
        assert!(!is_prompt_image("image/svg+xml"));
    }

    #[test]
    fn file_validation_errors_describe_attachments() {
        let mut file = ImageAttachmentInput {
            name: "report.pdf".into(),
            mime_type: "application/pdf".into(),
            data: "invalid!".into(),
        };
        let error = decode_attachments(&[file.clone()]).unwrap_err();
        assert!(error.contains("attachment 'report.pdf'"));
        assert!(!error.contains("image"));
        file.name = "a".repeat(256);
        assert_eq!(
            decode_attachments(&[file]).unwrap_err(),
            "attachment filename cannot exceed 255 characters"
        );
    }
}
