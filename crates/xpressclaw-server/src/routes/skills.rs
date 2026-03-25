use axum::extract::Path;
use axum::http::StatusCode;
use axum::routing::get;
use axum::{Json, Router};
use serde_json::{json, Value};

use crate::state::AppState;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/", get(list_skills))
        .route("/{name}", get(get_skill))
}

async fn list_skills() -> Json<Vec<Value>> {
    let skills = load_all_skills();
    let index: Vec<Value> = skills
        .iter()
        .map(|(name, desc, _)| {
            json!({
                "name": name,
                "description": desc,
            })
        })
        .collect();
    Json(index)
}

async fn get_skill(Path(name): Path<String>) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let skills = load_all_skills();
    let skill = skills.iter().find(|(n, _, _)| n == &name);
    match skill {
        Some((name, description, content)) => Ok(Json(json!({
            "name": name,
            "description": description,
            "content": content,
        }))),
        None => Err((
            StatusCode::NOT_FOUND,
            Json(json!({ "error": format!("Skill '{}' not found", name) })),
        )),
    }
}

/// Load all skills from templates/skills/. Returns (name, description, body).
fn load_all_skills() -> Vec<(String, String, String)> {
    let skill_dirs = [
        std::path::Path::new("templates/skills").to_path_buf(),
        std::env::current_exe()
            .ok()
            .and_then(|p| p.parent().map(|d| d.join("../Resources/templates/skills")))
            .unwrap_or_default(),
    ];

    let mut skills = Vec::new();

    for base in &skill_dirs {
        if !base.is_dir() {
            continue;
        }
        if let Ok(dirs) = std::fs::read_dir(base) {
            for entry in dirs.flatten() {
                let skill_file = entry.path().join("SKILL.md");
                if skill_file.is_file() {
                    if let Ok(content) = std::fs::read_to_string(&skill_file) {
                        if content.starts_with("---") {
                            let parts: Vec<&str> = content.splitn(3, "---").collect();
                            if parts.len() >= 3 {
                                let fm = parts[1];
                                let body = parts[2].trim();
                                let mut name = String::new();
                                let mut desc = String::new();
                                for line in fm.lines() {
                                    if let Some(v) = line.strip_prefix("name:") {
                                        name = v.trim().to_string();
                                    } else if let Some(v) = line.strip_prefix("description:") {
                                        desc = v.trim().to_string();
                                    }
                                }
                                if !name.is_empty() {
                                    skills.push((name, desc, body.to_string()));
                                }
                            }
                        }
                    }
                }
            }
        }
        if !skills.is_empty() {
            break;
        }
    }

    skills
}
