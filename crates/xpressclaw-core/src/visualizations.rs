//! Durable ingestion for Codex inline visualization content references.
//!
//! Final assistant messages may contain exact `visualize` references to files
//! inside the runner's writable workspace. The browser must never dereference
//! those paths. This module parses the references, reads them through an
//! explicitly mapped capability root, and stores bounded HTML fragments beside
//! their owning task or Conversation message.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use cap_std::ambient_authority;
use cap_std::fs::Dir;
use rusqlite::OptionalExtension;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::db::Database;
use crate::error::{Error, Result};

pub const MAX_VISUALIZATION_BYTES: usize = 1024 * 1024;
pub const MAX_VISUALIZATIONS_PER_MESSAGE: usize = 8;
const REFERENCE_START: &str = "visualize";
const REFERENCE_END: &str = "";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct MessageVisualization {
    pub id: String,
    pub reference_index: i64,
    pub title: String,
    pub mode: String,
    pub status: String,
    pub error_code: Option<String>,
    pub size: Option<usize>,
    /// Opaque capability supplied as a request header by the first-party UI.
    /// Keeping it out of the URL avoids referrer and request-log disclosure.
    pub retrieval_token: String,
}

#[derive(Debug, Clone)]
pub struct VisualizationArtifact {
    pub visualization: MessageVisualization,
    pub content: String,
}

/// A runner-visible writable root and the host directory mounted there.
///
/// Callers construct these only from the Agent's primary workspace and its
/// explicitly configured writable volume mounts. Authentication/config mounts
/// injected by XpressClaw are deliberately excluded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VisualizationSourceRoot {
    /// POSIX path used inside the Linux runner container. This remains POSIX
    /// even when the XpressClaw control plane itself runs on Windows.
    pub runner_root: String,
    pub host_root: PathBuf,
}

impl VisualizationSourceRoot {
    pub fn new(runner_root: impl Into<String>, host_root: impl Into<PathBuf>) -> Self {
        Self {
            runner_root: runner_root.into(),
            host_root: host_root.into(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreparedVisualization {
    pub reference_index: i64,
    pub title: String,
    pub mode: String,
    pub status: String,
    pub error_code: Option<String>,
    pub content: Option<String>,
    pub content_sha256: Option<String>,
    pub size: Option<usize>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ParsedVisualizationReference {
    reference_index: i64,
    path: String,
    title: Option<String>,
    mode: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CaptureError {
    OutsidePermittedRoots,
    Missing,
    Unreadable,
    NonHtml,
    Oversize,
    Malformed,
}

impl CaptureError {
    fn code(self) -> &'static str {
        match self {
            Self::OutsidePermittedRoots => "outside_permitted_roots",
            Self::Missing => "missing",
            Self::Unreadable => "unreadable",
            Self::NonHtml => "non_html",
            Self::Oversize => "oversize",
            Self::Malformed => "malformed_html",
        }
    }
}

pub fn prepare_message_visualizations(
    content: &str,
    source_roots: &[VisualizationSourceRoot],
) -> Vec<PreparedVisualization> {
    parse_visualization_references(content)
        .into_iter()
        .map(|reference| prepare_reference(reference, source_roots))
        .collect()
}

fn parse_visualization_references(content: &str) -> Vec<ParsedVisualizationReference> {
    let mut references = Vec::new();
    let mut cursor = 0;
    while let Some(relative_start) = content[cursor..].find(REFERENCE_START) {
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
        let Ok(serde_json::Value::Object(payload)) =
            serde_json::from_str::<serde_json::Value>(&content[payload_start..end])
        else {
            continue;
        };
        let Some(path) = payload.get("path").and_then(serde_json::Value::as_str) else {
            continue;
        };
        let path = path.trim();
        let title = match payload.get("title") {
            None => None,
            Some(serde_json::Value::String(title)) => Some(title),
            Some(_) => continue,
        };
        if path.is_empty()
            || path.len() > 4096
            || path.chars().any(char::is_control)
            || title.is_some_and(|title| {
                title.chars().count() > 250 || title.chars().any(char::is_control)
            })
        {
            continue;
        }
        let mode = match payload.get("mode") {
            None => "normal",
            Some(serde_json::Value::String(mode)) if mode == "wide" => "wide",
            Some(_) => continue,
        };
        references.push(ParsedVisualizationReference {
            reference_index: references.len() as i64,
            path: path.to_string(),
            title: title
                .map(|title| title.trim().to_string())
                .filter(|title| !title.is_empty()),
            mode: mode.to_string(),
        });
        if references.len() == MAX_VISUALIZATIONS_PER_MESSAGE {
            break;
        }
    }
    references
}

fn is_backslash_escaped(content: &str, index: usize) -> bool {
    let preceding = content[..index]
        .chars()
        .rev()
        .take_while(|character| *character == '\\')
        .count();
    preceding % 2 == 1
}

fn prepare_reference(
    reference: ParsedVisualizationReference,
    source_roots: &[VisualizationSourceRoot],
) -> PreparedVisualization {
    let title = reference.title.unwrap_or_else(|| {
        Path::new(&reference.path)
            .file_stem()
            .and_then(|name| name.to_str())
            .map(str::trim)
            .filter(|name| !name.is_empty() && !name.chars().any(char::is_control))
            .unwrap_or("Visualization")
            .chars()
            .take(250)
            .collect()
    });
    match capture_fragment(&reference.path, source_roots) {
        Ok(content) => {
            let size = content.len();
            let content_sha256 = format!("{:x}", Sha256::digest(content.as_bytes()));
            PreparedVisualization {
                reference_index: reference.reference_index,
                title,
                mode: reference.mode,
                status: "ready".to_string(),
                error_code: None,
                content: Some(content),
                content_sha256: Some(content_sha256),
                size: Some(size),
            }
        }
        Err(error) => PreparedVisualization {
            reference_index: reference.reference_index,
            title,
            mode: reference.mode,
            status: "unavailable".to_string(),
            error_code: Some(error.code().to_string()),
            content: None,
            content_sha256: None,
            size: None,
        },
    }
}

fn capture_fragment(
    raw_path: &str,
    source_roots: &[VisualizationSourceRoot],
) -> std::result::Result<String, CaptureError> {
    let source_components =
        normalize_runner_path(raw_path).ok_or(CaptureError::OutsidePermittedRoots)?;
    if raw_path.ends_with('/')
        || !source_components
            .last()
            .is_some_and(|name| has_html_extension(name))
    {
        return Err(CaptureError::NonHtml);
    }

    let mut candidates = source_roots
        .iter()
        .filter_map(|root| {
            let root_components = normalize_runner_path(&root.runner_root)?;
            if !source_components.starts_with(&root_components) {
                return None;
            }
            let relative = source_components[root_components.len()..]
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
    let canonical_root = root.host_root.canonicalize().map_err(|error| {
        if error.kind() == std::io::ErrorKind::NotFound {
            CaptureError::Missing
        } else {
            CaptureError::Unreadable
        }
    })?;
    let (capability_root, capability_relative) = if relative.as_os_str().is_empty() {
        // Docker also permits a writable bind whose source and target are
        // individual files. The configured source itself is the complete
        // capability in this case; open only its canonical basename through
        // its parent rather than widening discovery to sibling files.
        if !canonical_root.is_file() {
            return Err(CaptureError::NonHtml);
        }
        let parent = canonical_root.parent().ok_or(CaptureError::Unreadable)?;
        let name = canonical_root
            .file_name()
            .ok_or(CaptureError::Unreadable)?
            .into();
        (parent.to_path_buf(), name)
    } else {
        let canonical_candidate =
            canonical_root
                .join(&relative)
                .canonicalize()
                .map_err(|error| {
                    if error.kind() == std::io::ErrorKind::NotFound {
                        CaptureError::Missing
                    } else {
                        CaptureError::Unreadable
                    }
                })?;
        if !canonical_candidate.starts_with(&canonical_root) {
            return Err(CaptureError::OutsidePermittedRoots);
        }
        (canonical_root, relative)
    };

    // Open relative to a capability directory as defense in depth against a
    // symlink swap between canonicalization and opening the file.
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
    read_fragment(file)
}

fn read_fragment(file: cap_std::fs::File) -> std::result::Result<String, CaptureError> {
    let metadata = file.metadata().map_err(|_| CaptureError::Unreadable)?;
    if !metadata.is_file() {
        return Err(CaptureError::NonHtml);
    }
    if metadata.len() > MAX_VISUALIZATION_BYTES as u64 {
        return Err(CaptureError::Oversize);
    }
    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    file.take((MAX_VISUALIZATION_BYTES + 1) as u64)
        .read_to_end(&mut bytes)
        .map_err(|_| CaptureError::Unreadable)?;
    if bytes.len() > MAX_VISUALIZATION_BYTES {
        return Err(CaptureError::Oversize);
    }
    let content = String::from_utf8(bytes).map_err(|_| CaptureError::NonHtml)?;
    if !is_html_fragment(&content) {
        return Err(CaptureError::Malformed);
    }
    Ok(content)
}

/// Parse a runner-visible path independently from host path semantics. Native
/// runners currently use Linux containers on every supported host, including
/// Docker Desktop on Windows, so their mount targets and emitted references
/// are always absolute POSIX paths.
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

pub(crate) fn is_absolute_runner_root(path: &str) -> bool {
    normalize_runner_path(path).is_some()
}

fn has_html_extension(name: &str) -> bool {
    name.rsplit_once('.').is_some_and(|(_, extension)| {
        extension.eq_ignore_ascii_case("html") || extension.eq_ignore_ascii_case("htm")
    })
}

fn is_html_fragment(content: &str) -> bool {
    let trimmed = content.trim();
    if trimmed.is_empty() || trimmed.contains('\0') {
        return false;
    }
    scan_fragment_structure(&trimmed.to_ascii_lowercase()).unwrap_or(false)
}

/// Check enough HTML structure to distinguish a fragment from a full document
/// without treating tag-shaped strings inside scripts, styles, comments, or
/// attributes as document elements. Browsers remain the actual HTML parser;
/// this boundary rejects empty/malformed input and document shell tags.
fn scan_fragment_structure(content: &str) -> Option<bool> {
    let bytes = content.as_bytes();
    let mut cursor = 0;
    let mut found_element = false;
    while let Some(relative_start) = content[cursor..].find('<') {
        let start = cursor + relative_start;
        if content[start..].starts_with("<!--") {
            cursor = start + content[start + 4..].find("-->")? + 7;
            continue;
        }

        let mut name_start = start + 1;
        let closing = matches!(bytes.get(name_start), Some(b'/'));
        if closing {
            name_start += 1;
        }
        if matches!(bytes.get(name_start), Some(b'!')) {
            let declaration_end = find_tag_end(bytes, name_start + 1)?;
            let declaration = content[name_start + 1..declaration_end].trim_start();
            if declaration.strip_prefix("doctype").is_some_and(|suffix| {
                suffix.is_empty() || suffix.chars().next().is_some_and(char::is_whitespace)
            }) {
                return None;
            }
            cursor = declaration_end + 1;
            continue;
        }
        if !bytes.get(name_start).is_some_and(u8::is_ascii_alphabetic) {
            cursor = start + 1;
            continue;
        }
        let mut name_end = name_start + 1;
        while bytes.get(name_end).is_some_and(|character| {
            character.is_ascii_alphanumeric() || matches!(character, b'-' | b':' | b'_')
        }) {
            name_end += 1;
        }
        let name = &content[name_start..name_end];
        if matches!(name, "html" | "head" | "body") {
            return None;
        }
        let tag_end = find_tag_end(bytes, name_end)?;
        found_element = true;
        cursor = tag_end + 1;

        // HTML treats script/style bodies as raw text. Skip them so a JS or
        // CSS string containing "<body>" is not mistaken for a document shell.
        if !closing && matches!(name, "script" | "style") {
            cursor = find_raw_text_end(content, cursor, name)?;
        }
    }
    Some(found_element)
}

fn find_raw_text_end(content: &str, mut cursor: usize, name: &str) -> Option<usize> {
    let bytes = content.as_bytes();
    let closing_prefix = format!("</{name}");
    loop {
        let closing_relative = content[cursor..].find(&closing_prefix)?;
        let closing_start = cursor + closing_relative;
        let name_end = closing_start + closing_prefix.len();
        if bytes.get(name_end).is_some_and(|character| {
            character.is_ascii_whitespace() || matches!(character, b'>' | b'/')
        }) {
            return find_tag_end(bytes, name_end).map(|end| end + 1);
        }
        cursor = name_end;
    }
}

fn find_tag_end(bytes: &[u8], mut cursor: usize) -> Option<usize> {
    let mut quote = None;
    while let Some(character) = bytes.get(cursor).copied() {
        match (quote, character) {
            (Some(active), value) if value == active => quote = None,
            (None, b'\'' | b'"') => quote = Some(character),
            (None, b'>') => return Some(cursor),
            _ => {}
        }
        cursor += 1;
    }
    None
}

pub(crate) fn store_task_message_visualizations(
    transaction: &rusqlite::Transaction<'_>,
    message_id: i64,
    attempt_id: Option<&str>,
    visualizations: &[PreparedVisualization],
) -> Result<Vec<MessageVisualization>> {
    store_visualizations(
        transaction,
        VisualizationOwner::Task(message_id),
        attempt_id,
        None,
        visualizations,
    )
}

pub(crate) fn store_conversation_message_visualizations(
    transaction: &rusqlite::Transaction<'_>,
    message_id: i64,
    attempt_id: Option<&str>,
    conversation_turn_id: Option<&str>,
    visualizations: &[PreparedVisualization],
) -> Result<Vec<MessageVisualization>> {
    store_visualizations(
        transaction,
        VisualizationOwner::Conversation(message_id),
        attempt_id,
        conversation_turn_id,
        visualizations,
    )
}

#[derive(Debug, Clone, Copy)]
enum VisualizationOwner {
    Task(i64),
    Conversation(i64),
}

fn store_visualizations(
    transaction: &rusqlite::Transaction<'_>,
    owner: VisualizationOwner,
    attempt_id: Option<&str>,
    conversation_turn_id: Option<&str>,
    visualizations: &[PreparedVisualization],
) -> Result<Vec<MessageVisualization>> {
    let (owner_kind, owner_id) = match owner {
        VisualizationOwner::Task(id) => ("task", id),
        VisualizationOwner::Conversation(id) => ("conversation", id),
    };
    for visualization in visualizations {
        let stable_key = format!("{owner_kind}:{owner_id}:{}", visualization.reference_index);
        let digest = format!("{:x}", Sha256::digest(stable_key.as_bytes()));
        let id = format!("viz-{}", &digest[..32]);
        let (task_message_id, conversation_message_id) = match owner {
            VisualizationOwner::Task(message_id) => (Some(message_id), None),
            VisualizationOwner::Conversation(message_id) => (None, Some(message_id)),
        };
        let retrieval_token = Uuid::new_v4().simple().to_string();
        transaction.execute(
            "INSERT INTO message_visualizations
             (id, task_message_id, conversation_message_id, attempt_id,
              conversation_turn_id, reference_index, title, display_mode,
              status, error_code, content, content_sha256, size, retrieval_token)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)
             ON CONFLICT(id) DO NOTHING",
            rusqlite::params![
                id,
                task_message_id,
                conversation_message_id,
                attempt_id,
                conversation_turn_id,
                visualization.reference_index,
                visualization.title,
                visualization.mode,
                visualization.status,
                visualization.error_code,
                visualization.content,
                visualization.content_sha256,
                visualization.size.map(|size| size as i64),
                retrieval_token,
            ],
        )?;
    }
    list_for_owner(transaction, owner)
}

fn list_for_owner(
    connection: &rusqlite::Connection,
    owner: VisualizationOwner,
) -> Result<Vec<MessageVisualization>> {
    let (column, owner_id) = match owner {
        VisualizationOwner::Task(id) => ("task_message_id", id),
        VisualizationOwner::Conversation(id) => ("conversation_message_id", id),
    };
    let sql = format!(
        "SELECT id, reference_index, title, display_mode, status, error_code, size,
                retrieval_token
         FROM message_visualizations WHERE {column} = ?1 ORDER BY reference_index"
    );
    let mut statement = connection.prepare(&sql)?;
    let rows = statement
        .query_map([owner_id], row_to_message_visualization)?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(rows)
}

fn row_to_message_visualization(row: &rusqlite::Row<'_>) -> rusqlite::Result<MessageVisualization> {
    Ok(MessageVisualization {
        id: row.get(0)?,
        reference_index: row.get(1)?,
        title: row.get(2)?,
        mode: row.get(3)?,
        status: row.get(4)?,
        error_code: row.get(5)?,
        size: row.get::<_, Option<i64>>(6)?.map(|size| size as usize),
        retrieval_token: row.get(7)?,
    })
}

pub struct VisualizationManager {
    db: Arc<Database>,
}

impl VisualizationManager {
    pub fn new(db: Arc<Database>) -> Self {
        Self { db }
    }

    pub fn list_for_task_message(&self, message_id: i64) -> Result<Vec<MessageVisualization>> {
        self.db.with_conn(|connection| {
            list_for_owner(connection, VisualizationOwner::Task(message_id))
        })
    }

    pub fn list_for_conversation_message(
        &self,
        message_id: i64,
    ) -> Result<Vec<MessageVisualization>> {
        self.db.with_conn(|connection| {
            list_for_owner(connection, VisualizationOwner::Conversation(message_id))
        })
    }

    pub fn task_artifact(
        &self,
        task_id: &str,
        message_id: i64,
        artifact_id: &str,
        retrieval_token: &str,
    ) -> Result<Option<VisualizationArtifact>> {
        self.db.with_conn(|connection| {
            connection
                .query_row(
                    "SELECT visualization.id, visualization.reference_index,
                            visualization.title, visualization.display_mode,
                            visualization.status, visualization.error_code,
                            visualization.size, visualization.retrieval_token,
                            visualization.content
                     FROM message_visualizations visualization
                     JOIN task_messages message
                       ON message.id = visualization.task_message_id
                     WHERE visualization.id = ?1 AND message.id = ?2
                       AND message.task_id = ?3 AND visualization.status = 'ready'
                       AND visualization.retrieval_token = ?4",
                    rusqlite::params![artifact_id, message_id, task_id, retrieval_token],
                    row_to_artifact,
                )
                .optional()
                .map_err(Error::from)
        })
    }

    pub fn conversation_artifact(
        &self,
        conversation_id: &str,
        message_id: i64,
        artifact_id: &str,
        retrieval_token: &str,
    ) -> Result<Option<VisualizationArtifact>> {
        self.db.with_conn(|connection| {
            connection
                .query_row(
                    "SELECT visualization.id, visualization.reference_index,
                            visualization.title, visualization.display_mode,
                            visualization.status, visualization.error_code,
                            visualization.size, visualization.retrieval_token,
                            visualization.content
                     FROM message_visualizations visualization
                     JOIN conversation_messages message
                       ON message.id = visualization.conversation_message_id
                     WHERE visualization.id = ?1 AND message.id = ?2
                       AND message.conversation_id = ?3
                       AND visualization.status = 'ready'
                       AND visualization.retrieval_token = ?4",
                    rusqlite::params![artifact_id, message_id, conversation_id, retrieval_token],
                    row_to_artifact,
                )
                .optional()
                .map_err(Error::from)
        })
    }
}

fn row_to_artifact(row: &rusqlite::Row<'_>) -> rusqlite::Result<VisualizationArtifact> {
    let visualization = row_to_message_visualization(row)?;
    let content = row.get::<_, Option<String>>(8)?.unwrap_or_default();
    Ok(VisualizationArtifact {
        visualization,
        content,
    })
}

#[cfg(test)]
mod tests {
    use std::fs;

    use tempfile::tempdir;

    use super::*;

    fn marker(payload: &str) -> String {
        format!("{REFERENCE_START}{payload}{REFERENCE_END}")
    }

    fn root(host_root: &Path) -> VisualizationSourceRoot {
        VisualizationSourceRoot::new("/workspace", host_root)
    }

    #[test]
    fn parses_ordered_mixed_references_without_assuming_plugin_version() {
        let content = format!(
            "**Before**\n\n{}\nBetween 😀\n{}\nAfter",
            marker(r#"{"path":"/workspace/one.html","title":"One","future":true}"#),
            marker(r#"{"path":"/workspace/two.htm","mode":"wide"}"#),
        );
        let references = parse_visualization_references(&content);
        assert_eq!(references.len(), 2);
        assert_eq!(references[0].reference_index, 0);
        assert_eq!(references[0].title.as_deref(), Some("One"));
        assert_eq!(references[0].mode, "normal");
        assert_eq!(references[1].reference_index, 1);
        assert_eq!(references[1].path, "/workspace/two.htm");
        assert_eq!(references[1].mode, "wide");
    }

    #[test]
    fn malformed_escaped_and_lookalike_references_are_not_parsed() {
        let content = format!(
            "\\{}\n{}\n{}\n{}\n{}\n{}",
            marker(r#"{"path":"/workspace/escaped.html"}"#),
            marker(r#"{"path":"","title":"Empty"}"#),
            marker(r#"{"path":"/workspace/mode.html","mode":"fullscreen"}"#),
            marker(r#"{"path":"/workspace/null-title.html","title":null}"#),
            marker(r#"{"path":"/workspace/null-mode.html","mode":null}"#),
            "visualise{\"path\":\"/workspace/lookalike.html\"}",
        );
        assert!(parse_visualization_references(&content).is_empty());
        assert!(
            parse_visualization_references(&format!(
                "\\\\{}",
                marker(r#"{"path":"/workspace/valid.html"}"#)
            ))
            .len()
                == 1
        );
    }

    #[test]
    fn bounds_the_number_of_captured_references_without_dropping_source_text() {
        let content = (0..(MAX_VISUALIZATIONS_PER_MESSAGE + 2))
            .map(|index| marker(&format!(r#"{{"path":"/workspace/{index}.html"}}"#)))
            .collect::<Vec<_>>()
            .join("\n");
        let references = parse_visualization_references(&content);
        assert_eq!(references.len(), MAX_VISUALIZATIONS_PER_MESSAGE);
        assert_eq!(references.last().unwrap().reference_index, 7);
    }

    #[test]
    fn captures_a_bounded_fragment_and_derives_metadata() {
        let directory = tempdir().unwrap();
        fs::write(
            directory.path().join("chart.html"),
            "<section><svg aria-label=\"Chart\"></svg></section>",
        )
        .unwrap();
        let prepared = prepare_message_visualizations(
            &marker(r#"{"path":"/workspace/chart.html","mode":"wide"}"#),
            &[root(directory.path())],
        );
        assert_eq!(prepared.len(), 1);
        assert_eq!(prepared[0].status, "ready");
        assert_eq!(prepared[0].title, "chart");
        assert_eq!(prepared[0].mode, "wide");
        assert_eq!(
            prepared[0].size,
            prepared[0].content.as_ref().map(String::len)
        );
        assert_eq!(
            prepared[0].content_sha256.as_deref().map(str::len),
            Some(64)
        );
        assert!(is_html_fragment("<html-chart>custom element</html-chart>"));
        assert!(is_html_fragment(
            r#"<!-- <body> is text --><div data-template="<body>"></div><script>const template = "<body>";</script>"#
        ));
        assert!(!is_html_fragment("<div"));
        assert!(!is_html_fragment("plain text only"));
    }

    #[test]
    fn rejects_traversal_non_html_full_documents_and_oversize_files() {
        let parent = tempdir().unwrap();
        let workspace = parent.path().join("workspace");
        fs::create_dir(&workspace).unwrap();
        fs::write(parent.path().join("secret.html"), "<div>secret</div>").unwrap();
        fs::write(workspace.join("note.txt"), "<div>text</div>").unwrap();
        fs::write(workspace.join("page.html"), "<!doctype html><html></html>").unwrap();
        let mut oversize = vec![b'x'; MAX_VISUALIZATION_BYTES + 1];
        oversize[0..5].copy_from_slice(b"<div>");
        fs::write(workspace.join("large.html"), oversize).unwrap();

        let content = [
            marker(r#"{"path":"/workspace/../secret.html"}"#),
            marker(r#"{"path":"/workspace/note.txt"}"#),
            marker(r#"{"path":"/workspace/page.html"}"#),
            marker(r#"{"path":"/workspace/large.html"}"#),
            marker(r#"{"path":"/workspace/missing.html"}"#),
        ]
        .join("\n");
        let prepared = prepare_message_visualizations(&content, &[root(&workspace)]);
        assert_eq!(
            prepared
                .iter()
                .map(|item| item.error_code.as_deref().unwrap())
                .collect::<Vec<_>>(),
            vec![
                "outside_permitted_roots",
                "non_html",
                "malformed_html",
                "oversize",
                "missing",
            ]
        );
    }

    #[test]
    fn chooses_the_most_specific_runner_mount_deterministically() {
        let primary = tempdir().unwrap();
        let nested = tempdir().unwrap();
        fs::create_dir(primary.path().join("nested")).unwrap();
        fs::write(
            primary.path().join("nested/chart.html"),
            "<div>primary</div>",
        )
        .unwrap();
        fs::write(nested.path().join("chart.html"), "<div>mounted</div>").unwrap();
        let roots = [
            VisualizationSourceRoot::new("/workspace", primary.path()),
            VisualizationSourceRoot::new("/workspace/nested", nested.path()),
        ];
        let prepared = prepare_message_visualizations(
            &marker(r#"{"path":"/workspace/nested/chart.html"}"#),
            &roots,
        );
        assert_eq!(prepared[0].content.as_deref(), Some("<div>mounted</div>"));
    }

    #[test]
    fn captures_an_exact_writable_file_bind_mount() {
        let directory = tempdir().unwrap();
        let source = directory.path().join("mounted-chart.html");
        fs::write(&source, "<figure>file bind</figure>").unwrap();
        let prepared = prepare_message_visualizations(
            &marker(r#"{"path":"/workspace/chart.html"}"#),
            &[VisualizationSourceRoot::new(
                "/workspace/chart.html",
                source,
            )],
        );
        assert_eq!(prepared.len(), 1);
        assert_eq!(prepared[0].status, "ready");
        assert_eq!(
            prepared[0].content.as_deref(),
            Some("<figure>file bind</figure>")
        );
    }

    #[cfg(unix)]
    #[test]
    fn rejects_a_symlink_escape() {
        use std::os::unix::fs::symlink;

        let directory = tempdir().unwrap();
        let outside = tempdir().unwrap();
        fs::write(outside.path().join("secret.html"), "<div>secret</div>").unwrap();
        symlink(
            outside.path().join("secret.html"),
            directory.path().join("link.html"),
        )
        .unwrap();
        let prepared = prepare_message_visualizations(
            &marker(r#"{"path":"/workspace/link.html"}"#),
            &[root(directory.path())],
        );
        assert_eq!(
            prepared[0].error_code.as_deref(),
            Some("outside_permitted_roots")
        );
    }

    #[test]
    fn stores_task_and_conversation_artifacts_idempotently_and_scopes_retrieval() {
        let db = Arc::new(Database::open_memory().unwrap());
        let stored_content = "<div id=\"map\"></div>";
        let prepared = PreparedVisualization {
            reference_index: 0,
            title: "Dependency map".into(),
            mode: "normal".into(),
            status: "ready".into(),
            error_code: None,
            content: Some(stored_content.into()),
            content_sha256: Some(format!("{:x}", Sha256::digest(stored_content.as_bytes()))),
            size: Some(stored_content.len()),
        };
        let (task_message_id, conversation_message_id) = db
            .with_conn(|connection| -> Result<(i64, i64)> {
                let transaction = connection.unchecked_transaction()?;
                transaction.execute(
                    "INSERT INTO tasks (id, title) VALUES ('task', 'Task'), ('other', 'Other')",
                    [],
                )?;
                transaction.execute(
                    "INSERT INTO task_messages (task_id, role, content)
                     VALUES ('task', 'assistant', 'result')",
                    [],
                )?;
                let task_message_id = transaction.last_insert_rowid();
                let first = store_task_message_visualizations(
                    &transaction,
                    task_message_id,
                    None,
                    std::slice::from_ref(&prepared),
                )?;
                let unavailable_replay = PreparedVisualization {
                    status: "unavailable".into(),
                    error_code: Some("missing".into()),
                    content: None,
                    content_sha256: None,
                    size: None,
                    ..prepared.clone()
                };
                let repeated = store_task_message_visualizations(
                    &transaction,
                    task_message_id,
                    None,
                    std::slice::from_ref(&unavailable_replay),
                )?;
                assert_eq!(first[0].id, repeated[0].id);
                assert_eq!(first[0].retrieval_token, repeated[0].retrieval_token);
                assert_eq!(repeated[0].status, "ready");

                transaction.execute(
                    "INSERT INTO conversations (id, title) VALUES ('conversation', 'Conversation')",
                    [],
                )?;
                transaction.execute(
                    "INSERT INTO conversation_messages
                     (conversation_id, sender_type, sender_id, content)
                     VALUES ('conversation', 'agent', 'atlas', 'result')",
                    [],
                )?;
                let conversation_message_id = transaction.last_insert_rowid();
                store_conversation_message_visualizations(
                    &transaction,
                    conversation_message_id,
                    None,
                    None,
                    std::slice::from_ref(&prepared),
                )?;
                transaction.commit()?;
                Ok((task_message_id, conversation_message_id))
            })
            .unwrap();

        let manager = VisualizationManager::new(db.clone());
        let task_visualization = manager
            .list_for_task_message(task_message_id)
            .unwrap()
            .pop()
            .unwrap();
        assert!(manager
            .task_artifact(
                "task",
                task_message_id,
                &task_visualization.id,
                &task_visualization.retrieval_token,
            )
            .unwrap()
            .is_some());
        assert!(manager
            .task_artifact(
                "other",
                task_message_id,
                &task_visualization.id,
                &task_visualization.retrieval_token
            )
            .unwrap()
            .is_none());
        assert!(manager
            .task_artifact("task", task_message_id, &task_visualization.id, "wrong")
            .unwrap()
            .is_none());

        let conversation_visualization = manager
            .list_for_conversation_message(conversation_message_id)
            .unwrap()
            .pop()
            .unwrap();
        assert_ne!(task_visualization.id, conversation_visualization.id);
        assert!(manager
            .conversation_artifact(
                "conversation",
                conversation_message_id,
                &conversation_visualization.id,
                &conversation_visualization.retrieval_token,
            )
            .unwrap()
            .is_some());
    }

    #[test]
    fn stored_artifact_survives_a_control_plane_restart() {
        let directory = tempdir().unwrap();
        let workspace = directory.path().join("workspace");
        fs::create_dir(&workspace).unwrap();
        let source = workspace.join("durable.html");
        fs::write(&source, "<section>still here</section>").unwrap();
        let prepared = prepare_message_visualizations(
            &marker(r#"{"path":"/workspace/durable.html","title":"Durable visual"}"#),
            &[root(&workspace)],
        );
        assert_eq!(prepared[0].status, "ready");
        fs::remove_file(source).unwrap();

        let database_path = directory.path().join("xpressclaw.db");
        let (message_id, artifact_id, retrieval_token) = {
            let db = Arc::new(Database::open(&database_path).unwrap());
            let message_id = db
                .with_conn(|connection| -> Result<i64> {
                    let transaction = connection.unchecked_transaction()?;
                    transaction.execute(
                        "INSERT INTO tasks (id, title) VALUES ('durable-task', 'Durable')",
                        [],
                    )?;
                    transaction.execute(
                        "INSERT INTO task_messages (task_id, role, content)
                         VALUES ('durable-task', 'assistant', 'visual')",
                        [],
                    )?;
                    let message_id = transaction.last_insert_rowid();
                    store_task_message_visualizations(&transaction, message_id, None, &prepared)?;
                    transaction.commit()?;
                    Ok(message_id)
                })
                .unwrap();
            let artifact = VisualizationManager::new(db)
                .list_for_task_message(message_id)
                .unwrap()
                .pop()
                .unwrap();
            (message_id, artifact.id, artifact.retrieval_token)
        };

        let reopened = Arc::new(Database::open(&database_path).unwrap());
        let artifact = VisualizationManager::new(reopened)
            .task_artifact("durable-task", message_id, &artifact_id, &retrieval_token)
            .unwrap()
            .unwrap();
        assert_eq!(artifact.content, "<section>still here</section>");
    }
}
