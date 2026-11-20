//! Durable publication of bounded files referenced by final Agent messages.
//!
//! Runner-visible absolute paths are never exposed to the browser. A valid
//! XpressClaw reference is opened through an explicitly mapped writable root,
//! validated, copied into the owning message transaction, and removed from the
//! rendered prose. Invalid references remain text; valid references that fail
//! capture become a short, actionable fallback.

use std::collections::HashSet;
use std::io::{Cursor, Read};
use std::path::{Path, PathBuf};

use cap_std::ambient_authority;
use cap_std::fs::Dir;
use serde_json::Value;

use crate::visualizations::VisualizationSourceRoot;

pub const MAX_PUBLISHED_FILE_BYTES: usize = 20 * 1024 * 1024;
pub const MAX_PUBLISHED_FILES_PER_MESSAGE: usize = 8;
const MAX_PUBLISHED_FILE_NAME_BYTES: usize = 255;
const MAX_OOXML_PACKAGE_ENTRIES: u16 = 4096;
const REFERENCE_START: &str = "xpressclaw-file";
const REFERENCE_END: &str = "";
const PPTX_MIME_TYPE: &str =
    "application/vnd.openxmlformats-officedocument.presentationml.presentation";
const DOCX_MIME_TYPE: &str =
    "application/vnd.openxmlformats-officedocument.wordprocessingml.document";
const XLSX_MIME_TYPE: &str = "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet";
const PDF_MIME_TYPE: &str = "application/pdf";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PublishedFileAttachment {
    pub name: String,
    pub mime_type: String,
    pub data: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreparedMessageArtifacts {
    pub content: String,
    pub attachments: Vec<PublishedFileAttachment>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct FileReference {
    path: String,
    title: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CaptureError {
    OutsidePermittedRoots,
    Missing,
    Unreadable,
    Unsupported,
    Oversize,
    Malformed,
}

impl CaptureError {
    fn message(self) -> &'static str {
        match self {
            Self::OutsidePermittedRoots => "the file is outside this Agent's writable workspace",
            Self::Missing => "the file no longer exists",
            Self::Unreadable => "the file could not be read",
            Self::Unsupported => "only PPTX, DOCX, XLSX, and PDF artifacts can be published",
            Self::Oversize => "the file exceeds the 20 MiB publication limit",
            Self::Malformed => "the file does not match its declared artifact format",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ArtifactFormat {
    extension: &'static str,
    mime_type: &'static str,
    required_package_entry: Option<&'static [u8]>,
}

/// Parse and capture exact assistant-authored XpressClaw file references.
/// Callers deliberately invoke this only for a final Agent response; user and
/// streaming content therefore cannot turn a path-shaped string into a read.
pub fn prepare_message_artifacts(
    content: &str,
    source_roots: &[VisualizationSourceRoot],
) -> PreparedMessageArtifacts {
    let mut rendered = String::with_capacity(content.len());
    let mut attachments = Vec::new();
    let mut captured_paths = HashSet::new();
    let mut captured_bytes = 0usize;
    let mut cursor = 0;
    let mut text_start = 0;

    while cursor < content.len() {
        let Some(relative_start) = content[cursor..].find(REFERENCE_START) else {
            break;
        };
        let start = cursor + relative_start;
        let payload_start = start + REFERENCE_START.len();
        let Some(relative_end) = content[payload_start..].find(REFERENCE_END) else {
            break;
        };
        let end = payload_start + relative_end;
        cursor = end + REFERENCE_END.len();
        if is_backslash_escaped(content, start) {
            continue;
        }
        let Some(reference) = parse_reference(&content[payload_start..end]) else {
            continue;
        };

        rendered.push_str(&content[text_start..start]);
        let title = display_title(&reference);
        if captured_paths.insert(reference.path.clone()) {
            if attachments.len() >= MAX_PUBLISHED_FILES_PER_MESSAGE {
                rendered.push_str(&format!(
                    "\n\n> Artifact unavailable: **{}** — a message can publish at most eight files.\n\n",
                    escape_markdown(&title),
                ));
            } else {
                match capture_presentation(&reference, source_roots) {
                Ok(attachment)
                    if captured_bytes
                        .checked_add(attachment.data.len())
                        .is_some_and(|total| total <= MAX_PUBLISHED_FILE_BYTES) =>
                {
                    captured_bytes += attachment.data.len();
                    attachments.push(attachment);
                }
                Ok(_) => rendered.push_str(&format!(
                    "\n\n> Artifact unavailable: **{}** — the published files exceed the combined 20 MiB message limit.\n\n",
                    escape_markdown(&title),
                )),
                Err(error) => rendered.push_str(&format!(
                    "\n\n> Artifact unavailable: **{}** — {}.\n\n",
                    escape_markdown(&title),
                    error.message()
                )),
                }
            }
        }
        text_start = cursor;
    }
    rendered.push_str(&content[text_start..]);

    PreparedMessageArtifacts {
        content: rendered,
        attachments,
    }
}

fn parse_reference(raw: &str) -> Option<FileReference> {
    let Value::Object(payload) = serde_json::from_str::<Value>(raw).ok()? else {
        return None;
    };
    let path = payload.get("path")?.as_str()?.trim();
    if path.is_empty() || path.len() > 4096 || path.chars().any(char::is_control) {
        return None;
    }
    let title = match payload.get("title") {
        None => None,
        Some(Value::String(title)) => {
            let title = title.trim();
            if title.is_empty() {
                None
            } else if title.chars().count() > 250 || title.chars().any(char::is_control) {
                return None;
            } else {
                Some(title.to_string())
            }
        }
        Some(_) => return None,
    };
    Some(FileReference {
        path: path.to_string(),
        title,
    })
}

fn capture_presentation(
    reference: &FileReference,
    source_roots: &[VisualizationSourceRoot],
) -> std::result::Result<PublishedFileAttachment, CaptureError> {
    let path_components =
        normalize_runner_path(&reference.path).ok_or(CaptureError::OutsidePermittedRoots)?;
    let source_name = path_components.last().ok_or(CaptureError::Unsupported)?;
    let format = artifact_format(source_name).ok_or(CaptureError::Unsupported)?;

    let mut candidates = source_roots
        .iter()
        .filter_map(|root| {
            let root_components = normalize_runner_path(&root.runner_root)?;
            if !path_components.starts_with(&root_components) {
                return None;
            }
            let relative = path_components[root_components.len()..]
                .iter()
                .collect::<PathBuf>();
            Some((root_components.len(), root, relative))
        })
        .collect::<Vec<_>>();
    candidates.sort_by(|left, right| {
        right
            .0
            .cmp(&left.0)
            .then_with(|| left.1.runner_root.cmp(&right.1.runner_root))
    });
    let Some((_, root, relative)) = candidates.into_iter().next() else {
        return Err(CaptureError::OutsidePermittedRoots);
    };
    let canonical_root = root.host_root.canonicalize().map_err(map_io_error)?;
    let (capability_root, capability_relative) = if relative.as_os_str().is_empty() {
        if !canonical_root.is_file() {
            return Err(CaptureError::Unsupported);
        }
        let parent = canonical_root.parent().ok_or(CaptureError::Unreadable)?;
        let name = canonical_root
            .file_name()
            .ok_or(CaptureError::Unreadable)?
            .into();
        (parent.to_path_buf(), name)
    } else {
        let canonical_candidate = canonical_root
            .join(&relative)
            .canonicalize()
            .map_err(map_io_error)?;
        if !canonical_candidate.starts_with(&canonical_root) {
            return Err(CaptureError::OutsidePermittedRoots);
        }
        (canonical_root, relative)
    };

    let directory = Dir::open_ambient_dir(&capability_root, ambient_authority())
        .map_err(|_| CaptureError::Unreadable)?;
    let file = directory.open(&capability_relative).map_err(|error| {
        if error.kind() == std::io::ErrorKind::NotFound {
            CaptureError::Missing
        } else if error.kind() == std::io::ErrorKind::PermissionDenied {
            CaptureError::OutsidePermittedRoots
        } else {
            CaptureError::Unreadable
        }
    })?;
    let data = read_artifact(file, format)?;
    Ok(PublishedFileAttachment {
        name: attachment_name(reference, source_name, format.extension),
        mime_type: format.mime_type.to_string(),
        data,
    })
}

fn artifact_format(name: &str) -> Option<ArtifactFormat> {
    let extension = name.rsplit_once('.')?.1;
    if extension.eq_ignore_ascii_case("pptx") {
        Some(ArtifactFormat {
            extension: "pptx",
            mime_type: PPTX_MIME_TYPE,
            required_package_entry: Some(b"ppt/presentation.xml"),
        })
    } else if extension.eq_ignore_ascii_case("docx") {
        Some(ArtifactFormat {
            extension: "docx",
            mime_type: DOCX_MIME_TYPE,
            required_package_entry: Some(b"word/document.xml"),
        })
    } else if extension.eq_ignore_ascii_case("xlsx") {
        Some(ArtifactFormat {
            extension: "xlsx",
            mime_type: XLSX_MIME_TYPE,
            required_package_entry: Some(b"xl/workbook.xml"),
        })
    } else if extension.eq_ignore_ascii_case("pdf") {
        Some(ArtifactFormat {
            extension: "pdf",
            mime_type: PDF_MIME_TYPE,
            required_package_entry: None,
        })
    } else {
        None
    }
}

fn read_artifact(
    file: cap_std::fs::File,
    format: ArtifactFormat,
) -> std::result::Result<Vec<u8>, CaptureError> {
    let metadata = file.metadata().map_err(|_| CaptureError::Unreadable)?;
    if !metadata.is_file() {
        return Err(CaptureError::Unsupported);
    }
    if metadata.len() > MAX_PUBLISHED_FILE_BYTES as u64 {
        return Err(CaptureError::Oversize);
    }
    let mut data = Vec::with_capacity(metadata.len() as usize);
    file.take((MAX_PUBLISHED_FILE_BYTES + 1) as u64)
        .read_to_end(&mut data)
        .map_err(|_| CaptureError::Unreadable)?;
    if data.len() > MAX_PUBLISHED_FILE_BYTES {
        return Err(CaptureError::Oversize);
    }
    if !matches_artifact_format(&data, format) {
        return Err(CaptureError::Malformed);
    }
    Ok(data)
}

fn matches_artifact_format(data: &[u8], format: ArtifactFormat) -> bool {
    let Some(required_entry) = format.required_package_entry else {
        return data.starts_with(b"%PDF-")
            && data
                .get(data.len().saturating_sub(1024)..)
                .is_some_and(|tail| contains_bytes(tail, b"%%EOF"));
    };
    if !has_bounded_zip_directory(data) {
        return false;
    }
    let Ok(archive) = zip::ZipArchive::new(Cursor::new(data)) else {
        return false;
    };
    let required_entry = std::str::from_utf8(required_entry).expect("package entry is UTF-8");
    let mut content_types = false;
    let mut document_root = false;
    for name in archive.file_names() {
        content_types |= name == "[Content_Types].xml";
        document_root |= name == required_entry;
    }
    content_types && document_root
}

/// Reject multi-disk, ZIP64, or implausibly entry-heavy packages before the
/// ZIP reader allocates its central-directory index. A 20 MiB Office artifact
/// has no legitimate reason to contain thousands of package parts.
fn has_bounded_zip_directory(data: &[u8]) -> bool {
    const EOCD_LEN: usize = 22;
    const EOCD_SIGNATURE: &[u8; 4] = b"PK\x05\x06";

    if data.len() < EOCD_LEN {
        return false;
    }
    let search_start = data.len().saturating_sub(EOCD_LEN + usize::from(u16::MAX));
    for offset in (search_start..=data.len() - EOCD_LEN).rev() {
        if data.get(offset..offset + 4) != Some(EOCD_SIGNATURE) {
            continue;
        }
        let Some(comment_len) = read_u16_le(data, offset + 20) else {
            continue;
        };
        if offset.checked_add(EOCD_LEN + usize::from(comment_len)) != Some(data.len()) {
            continue;
        }
        let Some(disk_number) = read_u16_le(data, offset + 4) else {
            continue;
        };
        let Some(directory_disk) = read_u16_le(data, offset + 6) else {
            continue;
        };
        let Some(entries_on_disk) = read_u16_le(data, offset + 8) else {
            continue;
        };
        let Some(total_entries) = read_u16_le(data, offset + 10) else {
            continue;
        };
        if disk_number != 0
            || directory_disk != 0
            || entries_on_disk != total_entries
            || total_entries == 0
            || total_entries > MAX_OOXML_PACKAGE_ENTRIES
        {
            continue;
        }
        let Some(directory_size) = read_u32_le(data, offset + 12) else {
            continue;
        };
        let Some(directory_offset) = read_u32_le(data, offset + 16) else {
            continue;
        };
        if usize::try_from(directory_offset)
            .ok()
            .and_then(|start| start.checked_add(directory_size as usize))
            .is_some_and(|end| end <= offset)
        {
            return true;
        }
    }
    false
}

fn read_u16_le(data: &[u8], offset: usize) -> Option<u16> {
    Some(u16::from_le_bytes(
        data.get(offset..offset + 2)?.try_into().ok()?,
    ))
}

fn read_u32_le(data: &[u8], offset: usize) -> Option<u32> {
    Some(u32::from_le_bytes(
        data.get(offset..offset + 4)?.try_into().ok()?,
    ))
}

fn contains_bytes(haystack: &[u8], needle: &[u8]) -> bool {
    haystack
        .windows(needle.len())
        .any(|window| window == needle)
}

fn map_io_error(error: std::io::Error) -> CaptureError {
    if error.kind() == std::io::ErrorKind::NotFound {
        CaptureError::Missing
    } else {
        CaptureError::Unreadable
    }
}

/// Runner paths always use absolute POSIX semantics, including when the host
/// is Windows and Docker Desktop maps the backing directory.
fn normalize_runner_path(path: &str) -> Option<Vec<&str>> {
    if !path.starts_with('/') || path.contains('\\') || path.chars().any(char::is_control) {
        return None;
    }
    let mut components = Vec::new();
    for component in path.split('/').skip(1) {
        match component {
            "" | "." => {}
            ".." => return None,
            value => components.push(value),
        }
    }
    Some(components)
}

fn is_backslash_escaped(content: &str, index: usize) -> bool {
    content[..index]
        .chars()
        .rev()
        .take_while(|character| *character == '\\')
        .count()
        % 2
        == 1
}

fn display_title(reference: &FileReference) -> String {
    reference.title.clone().unwrap_or_else(|| {
        Path::new(&reference.path)
            .file_stem()
            .and_then(|name| name.to_str())
            .map(str::trim)
            .filter(|name| !name.is_empty())
            .unwrap_or("Presentation")
            .chars()
            .take(250)
            .collect()
    })
}

fn attachment_name(reference: &FileReference, source_name: &str, extension: &str) -> String {
    let mut base = reference.title.as_deref().unwrap_or_else(|| {
        Path::new(source_name)
            .file_stem()
            .and_then(|name| name.to_str())
            .unwrap_or("artifact")
    });
    let suffix = format!(".{extension}");
    if base
        .get(base.len().saturating_sub(suffix.len())..)
        .is_some_and(|candidate| candidate.eq_ignore_ascii_case(&suffix))
    {
        base = &base[..base.len() - suffix.len()];
    }
    let mut sanitized = base
        .chars()
        .map(|character| match character {
            '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|' => '_',
            character if character.is_control() => '_',
            character => character,
        })
        .collect::<String>();
    sanitized = sanitized.trim().trim_end_matches('.').to_string();
    if sanitized.is_empty() {
        sanitized = "artifact".into();
    }
    let sanitized = sanitized.chars().take(200).collect::<String>();
    bound_published_file_name(&format!("{sanitized}.{extension}"))
}

pub(crate) fn bound_published_file_name(name: &str) -> String {
    if name.len() <= MAX_PUBLISHED_FILE_NAME_BYTES {
        return name.to_string();
    }

    let (stem, suffix) = name
        .rsplit_once('.')
        .filter(|(stem, extension)| {
            !stem.is_empty()
                && !extension.is_empty()
                && extension.is_ascii()
                && extension.len() + 1 < MAX_PUBLISHED_FILE_NAME_BYTES
        })
        .map_or((name, String::new()), |(stem, extension)| {
            (stem, format!(".{extension}"))
        });
    let byte_budget = MAX_PUBLISHED_FILE_NAME_BYTES - suffix.len();
    let end = stem
        .char_indices()
        .map(|(index, character)| index + character.len_utf8())
        .take_while(|end| *end <= byte_budget)
        .last()
        .unwrap_or(0);
    format!("{}{suffix}", &stem[..end])
}

fn escape_markdown(value: &str) -> String {
    value
        .replace('\\', "\\\\")
        .replace('*', "\\*")
        .replace('_', "\\_")
        .replace('`', "\\`")
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::io::Write;

    use tempfile::tempdir;

    use super::*;

    fn package_bytes(entry: &[u8]) -> Vec<u8> {
        let mut archive = zip::ZipWriter::new(Cursor::new(Vec::new()));
        let options = zip::write::SimpleFileOptions::default();
        archive.start_file("[Content_Types].xml", options).unwrap();
        archive.write_all(b"<Types />").unwrap();
        archive
            .start_file(std::str::from_utf8(entry).unwrap(), options)
            .unwrap();
        archive.write_all(b"<document />").unwrap();
        archive.finish().unwrap().into_inner()
    }

    fn pptx_bytes() -> Vec<u8> {
        package_bytes(b"ppt/presentation.xml")
    }

    fn root(path: &Path) -> Vec<VisualizationSourceRoot> {
        vec![VisualizationSourceRoot::new("/workspace", path)]
    }

    #[test]
    fn captures_presentation_and_removes_exact_marker_from_prose() {
        let directory = tempdir().unwrap();
        fs::write(directory.path().join("deck.pptx"), pptx_bytes()).unwrap();
        let result = prepare_message_artifacts(
            "Built and checked.\n\nxpressclaw-file{\"path\":\"/workspace/deck.pptx\",\"title\":\"Launch / plan\"}",
            &root(directory.path()),
        );
        assert_eq!(result.content, "Built and checked.\n\n");
        assert_eq!(result.attachments.len(), 1);
        assert_eq!(result.attachments[0].name, "Launch _ plan.pptx");
        assert_eq!(result.attachments[0].mime_type, PPTX_MIME_TYPE);
        assert_eq!(result.attachments[0].data, pptx_bytes());
    }

    #[test]
    fn published_names_preserve_extensions_within_conversation_byte_limit() {
        let name = attachment_name(
            &FileReference {
                path: "/workspace/deck.pptx".into(),
                title: Some("📊".repeat(64)),
            },
            "deck.pptx",
            "pptx",
        );

        assert!(name.len() <= MAX_PUBLISHED_FILE_NAME_BYTES);
        assert!(name.ends_with(".pptx"));
        assert_eq!(
            name.chars().filter(|character| *character == '📊').count(),
            62
        );
    }

    #[test]
    fn malformed_escaped_and_non_artifact_markers_stay_inert() {
        let content = concat!(
            "\\xpressclaw-file{\"path\":\"/workspace/deck.pptx\"}\n",
            "xpressclaw-filenot-json\n",
            "xpressclaw-file{\"path\":\"/workspace/deck.pptx\",\"title\":4}"
        );
        let result = prepare_message_artifacts(content, &[]);
        assert_eq!(result.content, content);
        assert!(result.attachments.is_empty());
    }

    #[test]
    fn rejects_traversal_symlink_escape_wrong_type_and_malformed_zip() {
        let directory = tempdir().unwrap();
        let outside = tempdir().unwrap();
        fs::write(outside.path().join("secret.pptx"), pptx_bytes()).unwrap();
        fs::write(directory.path().join("notes.txt"), b"notes").unwrap();
        fs::write(directory.path().join("bad.pptx"), b"not a zip").unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(
            outside.path().join("secret.pptx"),
            directory.path().join("linked.pptx"),
        )
        .unwrap();

        let references = [
            "/workspace/../secret.pptx",
            "/workspace/notes.txt",
            "/workspace/bad.pptx",
            #[cfg(unix)]
            "/workspace/linked.pptx",
        ];
        for path in references {
            let result = prepare_message_artifacts(
                &format!("xpressclaw-file{{\"path\":\"{path}\"}}"),
                &root(directory.path()),
            );
            assert!(result.attachments.is_empty(), "accepted {path}");
            assert!(result.content.contains("Artifact unavailable"));
        }
    }

    #[test]
    fn captures_supported_office_and_pdf_artifact_formats() {
        let directory = tempdir().unwrap();
        let fixtures = [
            (
                "brief.docx",
                package_bytes(b"word/document.xml"),
                DOCX_MIME_TYPE,
            ),
            (
                "budget.xlsx",
                package_bytes(b"xl/workbook.xml"),
                XLSX_MIME_TYPE,
            ),
            (
                "export.pdf",
                b"%PDF-1.7\nfixture\n%%EOF\n".to_vec(),
                PDF_MIME_TYPE,
            ),
        ];
        for (name, bytes, mime_type) in fixtures {
            fs::write(directory.path().join(name), &bytes).unwrap();
            let result = prepare_message_artifacts(
                &format!("xpressclaw-file{{\"path\":\"/workspace/{name}\",\"title\":\"Checked {name}\"}}"),
                &root(directory.path()),
            );
            assert!(result.content.is_empty());
            assert_eq!(result.attachments.len(), 1);
            assert_eq!(result.attachments[0].name, format!("Checked {name}"));
            assert_eq!(result.attachments[0].mime_type, mime_type);
            assert_eq!(result.attachments[0].data, bytes);
        }
    }

    #[test]
    fn rejects_office_packages_with_unbounded_central_directories() {
        let mut bytes = pptx_bytes();
        let eocd = bytes
            .windows(4)
            .rposition(|window| window == b"PK\x05\x06")
            .unwrap();
        let excessive_entries = (MAX_OOXML_PACKAGE_ENTRIES + 1).to_le_bytes();
        bytes[eocd + 8..eocd + 10].copy_from_slice(&excessive_entries);
        bytes[eocd + 10..eocd + 12].copy_from_slice(&excessive_entries);

        assert!(!matches_artifact_format(
            &bytes,
            artifact_format("deck.pptx").unwrap()
        ));
    }

    #[test]
    fn duplicate_reference_copies_once_and_missing_file_has_fallback() {
        let directory = tempdir().unwrap();
        fs::write(directory.path().join("deck.pptx"), pptx_bytes()).unwrap();
        let marker = "xpressclaw-file{\"path\":\"/workspace/deck.pptx\"}";
        let result = prepare_message_artifacts(
            &format!("{marker}\n{marker}\nxpressclaw-file{{\"path\":\"/workspace/missing.pptx\",\"title\":\"Lost deck\"}}"),
            &root(directory.path()),
        );
        assert_eq!(result.attachments.len(), 1);
        assert_eq!(result.content.matches("Artifact unavailable").count(), 1);
        assert!(result.content.contains("Lost deck"));
    }

    #[test]
    fn strips_every_valid_reference_after_the_attachment_cap() {
        let directory = tempdir().unwrap();
        let mut content = String::new();
        for index in 0..=MAX_PUBLISHED_FILES_PER_MESSAGE {
            let name = format!("deck-{index}.pptx");
            fs::write(directory.path().join(&name), pptx_bytes()).unwrap();
            content.push_str(&format!(
                "before {index} xpressclaw-file{{\"path\":\"/workspace/{name}\"}} after {index}\n"
            ));
        }

        let result = prepare_message_artifacts(&content, &root(directory.path()));

        assert_eq!(result.attachments.len(), MAX_PUBLISHED_FILES_PER_MESSAGE);
        assert!(!result.content.contains(REFERENCE_START));
        assert!(!result.content.contains("/workspace/"));
        assert_eq!(
            result
                .content
                .matches("a message can publish at most eight files")
                .count(),
            1
        );
        assert!(result.content.contains("before 8"));
        assert!(result.content.contains("after 8"));
    }
}
