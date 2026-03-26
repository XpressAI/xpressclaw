use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::routing::get;
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::{json, Value};

use crate::state::AppState;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/", get(list_skills).post(create_skill))
        .route("/{name}", get(get_skill).delete(delete_skill))
}

async fn list_skills(State(state): State<AppState>) -> Json<Vec<Value>> {
    let skills = load_all_skills(&state);
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

async fn get_skill(
    State(state): State<AppState>,
    Path(name): Path<String>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let skills = load_all_skills(&state);
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

#[derive(Debug, Deserialize)]
struct CreateSkillRequest {
    name: String,
    description: String,
    content: String,
}

async fn create_skill(
    State(state): State<AppState>,
    Json(req): Json<CreateSkillRequest>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let skills_dir = state
        .config_path
        .parent()
        .map(|d| d.join("skills"))
        .unwrap_or_default();

    let skill_dir = skills_dir.join(&req.name);
    std::fs::create_dir_all(&skill_dir).map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": e.to_string() })),
        )
    })?;

    let skill_md = format!(
        "---\nname: {}\ndescription: {}\n---\n\n{}",
        req.name, req.description, req.content
    );

    std::fs::write(skill_dir.join("SKILL.md"), &skill_md).map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": e.to_string() })),
        )
    })?;

    Ok(Json(json!({ "created": true, "name": req.name })))
}

async fn delete_skill(
    State(state): State<AppState>,
    Path(name): Path<String>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let skills_dir = state
        .config_path
        .parent()
        .map(|d| d.join("skills"))
        .unwrap_or_default();

    let skill_dir = skills_dir.join(&name);
    if !skill_dir.exists() {
        return Err((
            StatusCode::NOT_FOUND,
            Json(json!({ "error": format!("Skill '{}' not found", name) })),
        ));
    }

    std::fs::remove_dir_all(&skill_dir).map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": e.to_string() })),
        )
    })?;

    Ok(Json(json!({ "deleted": true, "name": name })))
}

/// Load all skills from the data directory (~/.xpressclaw/skills/).
fn load_all_skills(state: &AppState) -> Vec<(String, String, String)> {
    let skills_dir = state
        .config_path
        .parent()
        .map(|d| d.join("skills"))
        .unwrap_or_default();

    if !skills_dir.is_dir() {
        return Vec::new();
    }

    let mut skills = Vec::new();

    if let Ok(dirs) = std::fs::read_dir(&skills_dir) {
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

    skills.sort_by(|a, b| a.0.cmp(&b.0));
    skills
}
