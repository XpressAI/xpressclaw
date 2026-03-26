//! Built-in skills embedded at compile time and extracted to the data directory.

use rust_embed::Embed;
use std::path::{Path, PathBuf};
use tracing::info;

/// Embedded skill files from templates/skills/.
#[derive(Embed)]
#[folder = "../../templates/skills/"]
#[prefix = ""]
struct EmbeddedSkills;

/// Extract built-in skills to `{data_dir}/skills/` if they don't already exist.
/// User-created skills in the same directory are preserved.
pub fn extract_skills(data_dir: &Path) {
    let skills_dir = data_dir.join("skills");

    for filename in EmbeddedSkills::iter() {
        let filename = filename.as_ref();
        // Only extract SKILL.md files (not scripts, schemas, etc.)
        if !filename.ends_with("SKILL.md") {
            continue;
        }

        let dest = skills_dir.join(filename);
        if dest.exists() {
            continue; // Don't overwrite user modifications
        }

        if let Some(parent) = dest.parent() {
            let _ = std::fs::create_dir_all(parent);
        }

        if let Some(file) = EmbeddedSkills::get(filename) {
            let _ = std::fs::write(&dest, file.data.as_ref());
        }
    }

    // Also extract reference docs and scripts that skills depend on
    for filename in EmbeddedSkills::iter() {
        let filename = filename.as_ref();
        if filename.ends_with("SKILL.md") {
            continue; // Already handled
        }

        let dest = skills_dir.join(filename);
        if dest.exists() {
            continue;
        }

        if let Some(parent) = dest.parent() {
            let _ = std::fs::create_dir_all(parent);
        }

        if let Some(file) = EmbeddedSkills::get(filename) {
            let _ = std::fs::write(&dest, file.data.as_ref());
        }
    }

    info!(path = %skills_dir.display(), "skills extracted");
}

/// Get the skills directory path for a given data directory.
pub fn skills_dir(data_dir: &Path) -> PathBuf {
    data_dir.join("skills")
}
