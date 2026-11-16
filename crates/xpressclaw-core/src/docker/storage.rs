//! Inventory and explicit cleanup of this installation's recovery images.
use super::*;
use bollard::image::{ListImagesOptions, RemoveImageOptions};

#[derive(Clone, Debug, Serialize)]
pub struct RecoveryImage {
    pub id: String,
    pub agent_id: String,
    pub name: String,
    pub created_at: i64,
    pub bytes: Option<u64>,
    pub eligible: bool,
    pub reasons: Vec<String>,
}
#[derive(Debug, Serialize)]
pub struct EngineStorage {
    pub image_bytes: Option<u64>,
    pub volume_bytes: u64,
    pub unused_volume_bytes: u64,
    pub recovery_images: Vec<RecoveryImage>,
}

fn same_id(left: &str, right: &str) -> bool {
    left.trim_start_matches("sha256:") == right.trim_start_matches("sha256:")
}

impl DockerManager {
    /// Running helper mounts and shell processes also keep worktrees alive,
    /// even when no ACP turn currently owns the Agent runtime lease.
    pub async fn active_worktree_uses(
        &self,
        paths: &[PathBuf],
        inspected_agents: &[String],
    ) -> Result<HashMap<PathBuf, String>> {
        let containers = self
            .docker
            .list_containers(Some(ListContainersOptions::<String> {
                all: false,
                ..Default::default()
            }))
            .await
            .map_err(|error| Error::Container(error.to_string()))?;
        let mut kept = HashMap::new();
        for container in containers {
            let mounts = container.mounts.unwrap_or_default();
            let mut affected = Vec::new();
            for mount in mounts {
                let (Some(source), Some(destination)) = (mount.source, mount.destination) else {
                    continue;
                };
                let Ok(source) = PathBuf::from(source).canonicalize() else {
                    if mount.typ == Some(bollard::models::MountPointTypeEnum::BIND) {
                        for path in paths {
                            kept.insert(
                                path.clone(),
                                "A running container has a bind mount that could not be resolved"
                                    .into(),
                            );
                        }
                    }
                    continue;
                };
                for path in paths {
                    let canonical = path.canonicalize().unwrap_or_else(|_| path.clone());
                    if let Ok(relative) = canonical.strip_prefix(&source) {
                        affected.push((path.clone(), PathBuf::from(&destination).join(relative)));
                    } else if source.starts_with(&canonical) {
                        kept.insert(
                            path.clone(),
                            "A running container mounts files inside this worktree".into(),
                        );
                    }
                }
            }
            if affected.is_empty() {
                continue;
            }
            let labels = container.labels.unwrap_or_default();
            let owned_agent = labels.get(AGENT_ID_LABEL).filter(|agent| {
                inspected_agents.contains(agent)
                    && labels.get(INSTALLATION_LABEL) == self.installation_id.as_ref()
                    && self
                        .owned_project_container_name(agent)
                        .ok()
                        .is_some_and(|name| {
                            container
                                .names
                                .iter()
                                .flatten()
                                .any(|actual| actual.trim_start_matches('/') == name)
                        })
            });
            let Some(agent) = owned_agent else {
                for (path, _) in affected {
                    kept.insert(
                        path,
                        "A running helper container mounts this workspace".into(),
                    );
                }
                continue;
            };
            let running = self
                .container_files(agent, serde_json::json!({"operation":"process_paths"}))
                .await;
            match running {
                Ok(value) => {
                    let paths = value["paths"].as_array().ok_or_else(|| {
                        Error::Container("Missing container process paths".into())
                    })?;
                    for (path, inside) in affected {
                        if paths
                            .iter()
                            .filter_map(serde_json::Value::as_str)
                            .any(|active| Path::new(active).starts_with(&inside))
                        {
                            kept.insert(path, "A container process uses this worktree".into());
                        }
                    }
                }
                Err(_) => {
                    for (path, _) in affected {
                        kept.insert(
                            path,
                            "Active container processes could not be checked".into(),
                        );
                    }
                }
            }
        }
        Ok(kept)
    }

    pub async fn project_storage(
        &self,
        agents: &[String],
        configured_images: &[String],
    ) -> Result<EngineStorage> {
        let installation = self.installation_id.as_deref().ok_or_else(|| {
            Error::Container("Storage cleanup requires installation ownership".into())
        })?;
        let usage = self
            .docker
            .df()
            .await
            .map_err(|error| Error::Container(error.to_string()))?;
        let images = self
            .docker
            .list_images(Some(ListImagesOptions::<String> {
                all: true,
                ..Default::default()
            }))
            .await
            .map_err(|error| Error::Container(error.to_string()))?;
        let containers = self
            .docker
            .list_containers(Some(ListContainersOptions::<String> {
                all: true,
                ..Default::default()
            }))
            .await
            .map_err(|error| Error::Container(error.to_string()))?;
        let mut recovery_images = Vec::new();
        let mut seen = std::collections::HashSet::new();
        for image in &images {
            if !seen.insert(image.id.clone())
                || image.labels.get(INSTALLATION_LABEL).map(String::as_str) != Some(installation)
            {
                continue;
            }
            let Some(agent_id) = image
                .labels
                .get(AGENT_ID_LABEL)
                .filter(|id| agents.contains(id))
            else {
                continue;
            };
            let Some(reference) = image
                .labels
                .get(recovery::RECOVERY_SNAPSHOT_LABEL)
                .filter(|name| name.starts_with("localhost/xpressclaw-recovery:"))
            else {
                continue;
            };
            let mut reasons = Vec::new();
            if image
                .repo_tags
                .iter()
                .any(|tag| tag != reference && tag != "<none>:<none>")
            {
                reasons.push("Image has another tag".into());
            }
            if configured_images.iter().any(|configured| {
                same_id(configured, &image.id) || image.repo_tags.contains(configured)
            }) {
                reasons.push("Configured as an Agent image".into());
            }
            if containers.iter().any(|container| {
                container
                    .image_id
                    .as_ref()
                    .is_some_and(|id| same_id(id, &image.id))
            }) {
                reasons.push("Used by a running or stopped container".into());
            }
            if images
                .iter()
                .any(|child| same_id(&child.parent_id, &image.id))
            {
                reasons.push("Another image depends on this snapshot".into());
            }
            let bytes = usage
                .images
                .as_ref()
                .and_then(|images| images.iter().find(|entry| same_id(&entry.id, &image.id)))
                .filter(|entry| entry.shared_size >= 0)
                .map(|entry| entry.size.saturating_sub(entry.shared_size).max(0) as u64);
            recovery_images.push(RecoveryImage {
                id: image.id.clone(),
                agent_id: agent_id.clone(),
                name: reference.clone(),
                created_at: image.created,
                bytes,
                eligible: reasons.is_empty(),
                reasons,
            });
        }
        let mut volume_bytes = 0u64;
        let mut unused_volume_bytes = 0u64;
        for volume in usage.volumes.iter().flatten() {
            if let Some(usage) = &volume.usage_data {
                let bytes = usage.size.max(0) as u64;
                volume_bytes = volume_bytes.saturating_add(bytes);
                if usage.ref_count == 0 {
                    unused_volume_bytes = unused_volume_bytes.saturating_add(bytes);
                }
            }
        }
        Ok(EngineStorage {
            image_bytes: usage
                .layers_size
                .filter(|size| *size >= 0)
                .map(|size| size as u64),
            volume_bytes,
            unused_volume_bytes,
            recovery_images,
        })
    }

    pub async fn remove_recovery_image(&self, image: &RecoveryImage) -> Result<()> {
        if !image.eligible {
            return Err(Error::Container("Recovery image is still in use".into()));
        }
        let current = self
            .docker
            .inspect_image(&image.id)
            .await
            .map_err(|error| Error::Container(error.to_string()))?;
        let labels = current
            .config
            .as_ref()
            .and_then(|config| config.labels.as_ref())
            .ok_or_else(|| Error::Container("Image ownership is unavailable".into()))?;
        if labels.get(INSTALLATION_LABEL) != self.installation_id.as_ref()
            || labels.get(AGENT_ID_LABEL) != Some(&image.agent_id)
            || labels.get(recovery::RECOVERY_SNAPSHOT_LABEL) != Some(&image.name)
            || current
                .repo_tags
                .iter()
                .flatten()
                .any(|tag| tag != &image.name && tag != "<none>:<none>")
        {
            return Err(Error::Container(
                "Image ownership changed; image was kept".into(),
            ));
        }
        self.docker
            .remove_image(
                &image.id,
                Some(RemoveImageOptions {
                    force: false,
                    noprune: true,
                }),
                None,
            )
            .await
            .map_err(|error| Error::Container(error.to_string()))?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bollard::models::{ContainerSummary, ImageSummary, MountPoint, MountPointTypeEnum};
    use serde_json::json;

    #[tokio::test]
    async fn helper_mounts_protect_only_the_affected_worktrees() {
        let root = tempfile::tempdir().unwrap();
        let worktree = root.path().join("worktree");
        std::fs::create_dir(&worktree).unwrap();
        let unrelated = tempfile::tempdir().unwrap();
        let (docker, _, server) = super::super::tests::mock_engine(HashMap::from([(
            "GET /containers/json".into(),
            (
                200,
                json!([ContainerSummary {
                    names: Some(vec!["/external-build".into()]),
                    mounts: Some(vec![MountPoint {
                        typ: Some(MountPointTypeEnum::BIND),
                        source: Some(root.path().display().to_string()),
                        destination: Some("/workspace".into()),
                        ..Default::default()
                    }]),
                    ..Default::default()
                }]),
            ),
        )]))
        .await;
        let kept = docker
            .active_worktree_uses(&[worktree.clone(), unrelated.path().to_owned()], &[])
            .await
            .unwrap();
        assert_eq!(kept.len(), 1);
        assert!(kept.contains_key(&worktree));
        server.abort();
    }

    #[tokio::test]
    async fn cleanup_preserves_foreign_retagged_configured_and_referenced_images() {
        let image = |id: &str, installation: &str| ImageSummary {
            id: id.into(),
            size: 100,
            shared_size: 20,
            labels: HashMap::from([
                (INSTALLATION_LABEL.into(), installation.into()),
                (AGENT_ID_LABEL.into(), "agent".into()),
                (
                    recovery::RECOVERY_SNAPSHOT_LABEL.into(),
                    format!("localhost/xpressclaw-recovery:{id}"),
                ),
            ]),
            repo_tags: vec![format!("localhost/xpressclaw-recovery:{id}")],
            ..Default::default()
        };
        let free = image("free", "installation");
        let used = image("used", "installation");
        let foreign = image("foreign", "other-installation");
        let configured = image("configured", "installation");
        let mut tagged = image("tagged", "installation");
        tagged.repo_tags.push("my-saved-image:latest".into());
        let images = vec![free.clone(), used, foreign.clone(), configured, tagged];
        let responses = HashMap::from([
            (
                "GET /system/df".into(),
                (200, json!({"LayersSize":400,"Images":images})),
            ),
            ("GET /images/json".into(), (200, json!(images))),
            (
                "GET /containers/json".into(),
                (
                    200,
                    json!([ContainerSummary {
                        image_id: Some("used".into()),
                        ..Default::default()
                    }]),
                ),
            ),
            (
                "GET /images/free/json".into(),
                (
                    200,
                    json!({"Id":"free","RepoTags":free.repo_tags,"Config":{"Labels":free.labels}}),
                ),
            ),
            (
                "GET /images/foreign/json".into(),
                (
                    200,
                    json!({"Id":"foreign","RepoTags":foreign.repo_tags,"Config":{"Labels":foreign.labels}}),
                ),
            ),
            (
                "DELETE /images/free".into(),
                (200, json!([{"Deleted":"free"}])),
            ),
        ]);
        let (docker, requests, server) = super::super::tests::mock_engine(responses).await;
        let report = docker
            .project_storage(&["agent".into()], &["configured".into()])
            .await
            .unwrap();
        let eligible = report
            .recovery_images
            .iter()
            .filter(|image| image.eligible)
            .collect::<Vec<_>>();
        assert_eq!(eligible.len(), 1);
        assert_eq!(eligible[0].id, "free");
        assert_eq!(eligible[0].bytes, Some(80));
        docker.remove_recovery_image(eligible[0]).await.unwrap();
        let mut stale = eligible[0].clone();
        stale.id = "foreign".into();
        stale.name = "localhost/xpressclaw-recovery:foreign".into();
        assert!(docker.remove_recovery_image(&stale).await.is_err());
        let deleted = requests
            .lock()
            .unwrap()
            .iter()
            .filter(|request| request.starts_with("DELETE"))
            .cloned()
            .collect::<Vec<_>>();
        assert_eq!(deleted, ["DELETE /images/free"]);
        server.abort();
    }
}
