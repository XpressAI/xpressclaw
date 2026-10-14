//! Apply changed launch settings without discarding an Agent's filesystem.
//! Callers hold the exclusive Agent runtime boundary until ACP initializes.
use super::*;
use bollard::container::RenameContainerOptions;
use bollard::image::{CommitContainerOptions, RemoveImageOptions};

/// An old environment retained because stopped helpers still refer to its ID.
/// These are visible in Harness settings and are never pruned automatically.
#[derive(Debug, Clone, Serialize)]
pub struct PreservedEnvironment {
    pub name: String,
    pub container_id: String,
    pub dependents: Vec<String>,
}

const RECOVERY_SOURCE_LABEL: &str = "io.xpressclaw.recovery-source";
const RECOVERY_SNAPSHOT_LABEL: &str = "io.xpressclaw.recovery-snapshot";

impl DockerManager {
    /// Rebuild only the current environment. Historical environments with
    /// helper dependencies stay discoverable and retain their writable layers.
    pub async fn rebuild_project_environment(
        &self,
        agent: &str,
    ) -> Result<Vec<PreservedEnvironment>> {
        let mut preserved = self.preserved_project_environments(agent).await?;
        let name = self.owned_project_container_name(agent)?;
        let mut candidates = Vec::new();
        for reference in [
            name.clone(),
            format!("{name}-recovery"),
            format!("xpressclaw-{agent}"),
        ] {
            let Some(container) = self.inspect_optional(&reference).await? else {
                continue;
            };
            let labels = container
                .config
                .as_ref()
                .and_then(|config| config.labels.as_ref())
                .cloned()
                .unwrap_or_default();
            if !workload_ownership_matches(
                &reference,
                &labels,
                self.installation_id.as_deref(),
                agent,
            ) {
                return Err(Error::Container(format!("Cannot rebuild environment {reference}: ownership does not match this Agent. Its files have been preserved.")));
            }
            if !self.ensure_dependents_stopped(&reference).await?.is_empty() {
                self.validate_recovery_owner(&container, agent)?;
            }
            candidates.push(
                container
                    .id
                    .ok_or_else(|| Error::Container("Environment has no container ID".into()))?,
            );
        }
        // Preflight the entire set before stopping or renaming anything.
        for id in candidates {
            if let Some(saved) = self.remove_or_preserve_environment(&id, agent).await? {
                preserved.push(saved);
            }
        }
        self.stop_agent_forwards(agent).await;
        Ok(preserved)
    }

    pub async fn preserved_project_environments(
        &self,
        agent: &str,
    ) -> Result<Vec<PreservedEnvironment>> {
        let Some(installation) = self.installation_id.as_deref() else {
            return Ok(Vec::new());
        };
        let prefix = format!("{}-preserved-", project_container_name(installation, agent));
        let containers = self
            .docker
            .list_containers(Some(ListContainersOptions::<String> {
                all: true,
                filters: HashMap::from([(
                    "label".into(),
                    vec![
                        format!("{INSTALLATION_LABEL}={installation}"),
                        format!("{AGENT_ID_LABEL}={agent}"),
                    ],
                )]),
                ..Default::default()
            }))
            .await
            .map_err(|error| {
                Error::Container(format!("Could not list preserved environments: {error}"))
            })?;
        let mut saved = Vec::new();
        for entry in containers {
            let Some(name) = entry.names.as_ref().and_then(|names| {
                names
                    .iter()
                    .map(|name| name.trim_start_matches('/'))
                    .find(|name| name.starts_with(&prefix))
            }) else {
                continue;
            };
            let Some(container) = self.inspect_optional(name).await? else {
                continue;
            };
            self.validate_recovery_owner(&container, agent)?;
            let container_id = container.id.unwrap();
            if name != format!("{prefix}{container_id}") {
                continue;
            }
            let dependents = self
                .container_dependents(&container_id)
                .await?
                .iter()
                .map(dependent_name)
                .collect();
            saved.push(PreservedEnvironment {
                name: name.into(),
                container_id,
                dependents,
            });
        }
        saved.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(saved)
    }

    async fn ensure_dependents_stopped(
        &self,
        reference: &str,
    ) -> Result<Vec<ContainerInspectResponse>> {
        let dependents = self.container_dependents(reference).await?;
        let active = dependents
            .iter()
            .filter(|container| {
                container.state.as_ref().is_none_or(|state| {
                    state.running != Some(false)
                        || state.paused == Some(true)
                        || state.restarting == Some(true)
                })
            })
            .map(dependent_name)
            .collect::<Vec<_>>();
        if !active.is_empty() {
            return Err(Error::Container(format!("Stop these helper containers before changing the environment: {}. Their files will be preserved automatically; no tasks need to be completed or deleted.", active.join(", "))));
        }
        if !dependents.is_empty() {
            let parent = self.inspect_optional(reference).await?.ok_or_else(|| {
                Error::Container("Environment disappeared while checking helpers".into())
            })?;
            let name = parent
                .name
                .as_deref()
                .unwrap_or(reference)
                .trim_start_matches('/');
            // A name-based reference would silently attach to the replacement
            // after renaming. Only ID-bound helpers can safely keep the parent.
            let named = dependents
                .iter()
                .filter(|container| {
                    container
                        .host_config
                        .as_ref()
                        .is_some_and(|host| container_depends_on(host, "", name))
                })
                .map(dependent_name)
                .collect::<Vec<_>>();
            if !named.is_empty() {
                return Err(Error::Container(format!("These helper containers refer to the environment by name and need to be recreated using its container ID before replacement: {}. All files have been preserved.", named.join(", "))));
            }
        }
        Ok(dependents)
    }

    async fn remove_or_preserve_environment(
        &self,
        reference: &str,
        agent: &str,
    ) -> Result<Option<PreservedEnvironment>> {
        let dependents = self.ensure_dependents_stopped(reference).await?;
        if dependents.is_empty() {
            return self
                .remove_container_reference(reference, agent)
                .await
                .map(|_| None);
        }
        let container = self.inspect_optional(reference).await?.ok_or_else(|| {
            Error::Container("Environment disappeared while preserving helper containers".into())
        })?;
        let id = container
            .id
            .as_deref()
            .ok_or_else(|| Error::Container("Environment has no container ID".into()))?;
        let name = format!(
            "{}-preserved-{id}",
            self.owned_project_container_name(agent)?
        );
        // Keep the exact parent ID, helper containers, and all their mounted
        // and unmounted files. Never use recursive removal or clone helpers
        // whose launch configuration the Agent may have changed independently.
        match self
            .docker
            .stop_container(id, Some(StopContainerOptions { t: 10 }))
            .await
        {
            Ok(())
            | Err(BollardError::DockerResponseServerError {
                status_code: 304, ..
            }) => {}
            Err(error) => {
                return Err(Error::Container(format!(
                    "Could not preserve old environment: {error}"
                )))
            }
        }
        self.docker.rename_container(id, RenameContainerOptions { name: name.as_str() }).await
            .map_err(|error| Error::Container(format!("Could not preserve old environment as {name}: {error}. Its files and helper containers remain intact.")))?;
        info!(
            agent_id = agent,
            environment = name,
            "preserved old environment for stopped helper containers"
        );
        Ok(Some(PreservedEnvironment {
            name,
            container_id: id.into(),
            dependents: dependents.iter().map(dependent_name).collect(),
        }))
    }

    pub async fn project_configuration_needs_recovery(
        &self,
        agent: &str,
        requested: &ContainerSpec,
    ) -> Result<bool> {
        let name = self.owned_project_container_name(agent)?;
        if let Some(backup) = self.inspect_optional(&format!("{name}-recovery")).await? {
            self.validate_recovery_owner(&backup, agent)?;
            return Ok(true);
        }
        match self.inspect_optional(&name).await? {
            Some(container) => Ok(!retained_spec_matches(
                &container,
                requested,
                self.installation_id()?,
                agent,
            )?),
            None => Ok(false),
        }
    }

    /// Keep the old container under a deterministic name until the replacement
    /// completes its ACP handshake. A crash at any intermediate step leaves
    /// enough ownership information to restore the old environment next turn.
    pub async fn recover_project_configuration(
        &self,
        agent: &str,
        requested: &ContainerSpec,
    ) -> Result<()> {
        self.rollback_project_recovery(agent).await?;
        let installation = self.installation_id()?;
        let name = self.owned_project_container_name(agent)?;
        let Some(original) = self.inspect_optional(&name).await? else {
            return Ok(());
        };
        if retained_spec_matches(&original, requested, installation, agent)? {
            return Ok(());
        }
        let id = original.id.as_deref().ok_or_else(|| {
            Error::Container("Cannot recover an environment without its container ID".into())
        })?;
        self.ensure_dependents_stopped(&name).await?;
        // Check the new mounts and image before interrupting the old process.
        runtime_mounts(&requested.volumes, self.selinux)?;
        let base_image = original
            .config
            .as_ref()
            .and_then(|config| config.labels.as_ref())
            .and_then(|labels| labels.get(BASE_IMAGE_LABEL))
            .or(original.image.as_ref())
            .ok_or_else(|| Error::Container("Environment has no base image".into()))?;
        let image = self
            .docker
            .inspect_image(base_image)
            .await
            .map_err(|error| {
                Error::Container(format!(
                    "Cannot inspect the environment's base image: {error}"
                ))
            })?;
        let image_config = image.config.ok_or_else(|| {
            Error::Container("Environment base image has no configuration".into())
        })?;
        let mut snapshot_config: ContainerConfig<String> = serde_json::from_value(
            serde_json::to_value(image_config)
                .map_err(|error| Error::Container(error.to_string()))?,
        )
        .map_err(|error| Error::Container(error.to_string()))?;
        snapshot_config
            .labels
            .get_or_insert_with(HashMap::new)
            .insert(BASE_IMAGE_LABEL.into(), base_image.clone());
        // Commit merges configuration with the source. Explicitly clear old
        // overrides that no longer exist, including revoked credentials.
        let mut environment = original
            .config
            .as_ref()
            .and_then(|config| config.env.as_ref())
            .into_iter()
            .flatten()
            .filter_map(|entry| {
                entry
                    .split_once('=')
                    .map(|(key, _)| (key.to_string(), String::new()))
            })
            .collect::<std::collections::BTreeMap<_, _>>();
        for entry in snapshot_config.env.as_deref().unwrap_or_default() {
            if let Some((key, value)) = entry.split_once('=') {
                environment.insert(key.to_string(), value.to_string());
            }
        }
        snapshot_config.env = Some(
            environment
                .into_iter()
                .map(|(key, value)| format!("{key}={value}"))
                .collect(),
        );
        let mut effective = requested.clone();
        let configured_mounts = original
            .config
            .as_ref()
            .and_then(|config| config.labels.as_ref())
            .and_then(|labels| labels.get(CONFIGURED_MOUNTS_LABEL))
            .map(|value| serde_json::from_str::<Vec<String>>(value))
            .transpose()
            .map_err(|error| {
                Error::Container(format!("Cannot read retained mount configuration: {error}"))
            })?;
        // Commit excludes mounted data. Reattach image-declared volumes by
        // name so anonymous volumes are preserved as well as explicit mounts.
        for target in snapshot_config
            .volumes
            .as_ref()
            .into_iter()
            .flat_map(|volumes| volumes.keys())
        {
            if effective
                .volumes
                .iter()
                .any(|mount| mount.target == *target)
            {
                continue;
            }
            // An explicitly removed mount is an authority change. Never
            // reattach it merely because the image also declares this target.
            if configured_mounts
                .as_ref()
                .is_some_and(|targets| targets.contains(target))
            {
                continue;
            }
            if configured_mounts.is_none() {
                let host = original.host_config.as_ref();
                let explicit = host
                    .and_then(|host| host.mounts.as_ref())
                    .into_iter()
                    .flatten()
                    .any(|mount| mount.target.as_ref() == Some(target))
                    || host
                        .and_then(|host| host.binds.as_ref())
                        .into_iter()
                        .flatten()
                        .any(|bind| {
                            VolumeMount::parse(bind).is_some_and(|mount| mount.target == *target)
                        });
                if explicit {
                    return Err(Error::Container(format!("Cannot safely recover legacy volume {target} after its mount was removed. The environment and its files have been preserved; restore the mount setting before retrying.")));
                }
            }
            let mount = original
                .mounts
                .as_ref()
                .into_iter()
                .flatten()
                .find(|mount| mount.destination.as_deref() == Some(target.as_str()))
                .ok_or_else(|| {
                    Error::Container(format!(
                        "Cannot locate retained volume {target}; environment preserved"
                    ))
                })?;
            let source = mount
                .name
                .as_ref()
                .or(mount.source.as_ref())
                .ok_or_else(|| {
                    Error::Container(format!(
                        "Cannot identify retained volume {target}; environment preserved"
                    ))
                })?;
            effective.volumes.push(VolumeMount {
                source: source.clone(),
                target: target.clone(),
                read_only: mount.rw == Some(false),
                selinux_relabel: SelinuxRelabel::None,
            });
        }
        self.stop_preserving(agent).await?;
        let snapshot = match self.snapshot_environment(id, snapshot_config).await {
            Ok(snapshot) => snapshot,
            Err(error) => {
                let _ = self.docker.start_container::<String>(id, None).await;
                return Err(Error::Container(format!("Could not save the environment for automatic recovery: {error}. The original environment has been preserved.")));
            }
        };
        let backup = format!("{name}-recovery");
        if let Err(error) = self
            .docker
            .rename_container(id, RenameContainerOptions { name: backup })
            .await
        {
            self.discard_unused_snapshot(&snapshot).await;
            let _ = self.docker.start_container::<String>(id, None).await;
            return Err(Error::Container(format!("Could not prepare environment recovery: {error}. The original environment has been preserved.")));
        }
        effective.image = snapshot;
        let mut labels =
            project_container_labels(&container_spec_fingerprint(requested)?, installation, agent);
        labels.insert(IMAGE_REFERENCE_LABEL.into(), requested.image.clone());
        labels.insert(BASE_IMAGE_LABEL.into(), base_image.clone());
        labels.insert(
            CONFIGURED_MOUNTS_LABEL.into(),
            serde_json::to_string(
                &requested
                    .volumes
                    .iter()
                    .map(|mount| &mount.target)
                    .collect::<Vec<_>>(),
            )
            .map_err(|error| Error::Container(error.to_string()))?,
        );
        labels.insert(RECOVERY_SOURCE_LABEL.into(), id.to_string());
        let result = self
            .create_and_start(&name, agent, &effective, Some(labels))
            .await;
        if let Err(error) = result {
            if let Err(rollback) = self.rollback_project_recovery(agent).await {
                return Err(Error::Container(format!(
                    "Automatic recovery failed: {error}. {rollback}"
                )));
            }
            self.discard_unused_snapshot(&effective.image).await;
            return Err(Error::Container(format!("Automatic recovery failed: {error}. The original environment was restored; retry the work after correcting the settings.")));
        }
        info!(
            agent_id = agent,
            "updated environment settings while preserving installed tools and files"
        );
        Ok(())
    }

    pub async fn finish_project_recovery(&self, agent: &str) -> Result<()> {
        let name = self.owned_project_container_name(agent)?;
        let Some(backup) = self.inspect_optional(&format!("{name}-recovery")).await? else {
            return Ok(());
        };
        self.validate_recovery_owner(&backup, agent)?;
        let replacement = self.inspect_optional(&name).await?.ok_or_else(|| {
            Error::Container(
                "Recovered environment disappeared; original environment preserved".into(),
            )
        })?;
        self.validate_recovery_pair(&backup, &replacement, agent)?;
        self.remove_or_preserve_environment(backup.id.as_deref().unwrap(), agent)
            .await
            .map(|_| ())
    }

    pub async fn rollback_project_recovery(&self, agent: &str) -> Result<()> {
        let name = self.owned_project_container_name(agent)?;
        let Some(backup) = self.inspect_optional(&format!("{name}-recovery")).await? else {
            return Ok(());
        };
        self.validate_recovery_owner(&backup, agent)?;
        if let Some(replacement) = self.inspect_optional(&name).await? {
            self.validate_recovery_pair(&backup, &replacement, agent)?;
            self.remove_container_reference(&name, agent).await?;
        }
        self.docker.rename_container(backup.id.as_deref().unwrap(), RenameContainerOptions { name: name.as_str() })
            .await.map_err(|error| Error::Container(format!("Could not restore the original environment name: {error}. Its files remain in {name}-recovery; retry recovery.")))?;
        self.docker
            .start_container::<String>(&name, None)
            .await
            .map_err(|error| {
                Error::Container(format!(
                    "Original environment preserved, but could not restart it: {error}"
                ))
            })?;
        Ok(())
    }

    fn validate_recovery_owner(
        &self,
        container: &ContainerInspectResponse,
        agent: &str,
    ) -> Result<()> {
        let owned = container
            .config
            .as_ref()
            .and_then(|config| config.labels.as_ref())
            .is_some_and(|labels| {
                project_ownership_matches(
                    labels,
                    self.installation_id.as_deref().unwrap_or_default(),
                    agent,
                )
            });
        if !owned || container.id.as_deref().is_none_or(str::is_empty) {
            return Err(Error::Container(
                "Recovery container ownership could not be verified; environment preserved".into(),
            ));
        }
        Ok(())
    }

    fn validate_recovery_pair(
        &self,
        backup: &ContainerInspectResponse,
        replacement: &ContainerInspectResponse,
        agent: &str,
    ) -> Result<()> {
        self.validate_recovery_owner(replacement, agent)?;
        let source = replacement
            .config
            .as_ref()
            .and_then(|config| config.labels.as_ref())
            .and_then(|labels| labels.get(RECOVERY_SOURCE_LABEL));
        if source.map(String::as_str) != backup.id.as_deref() {
            return Err(Error::Container("Recovery container does not match the original environment; both have been preserved".into()));
        }
        Ok(())
    }

    pub(super) async fn discard_unused_snapshot(&self, image: &str) {
        let mut current = image.to_string();
        let mut visited = std::collections::HashSet::new();
        while !current.is_empty() && visited.insert(current.clone()) {
            let Ok(snapshot) = self.docker.inspect_image(&current).await else {
                break;
            };
            let labels = snapshot
                .config
                .as_ref()
                .and_then(|config| config.labels.as_ref());
            let Some(reference) = labels.and_then(|labels| labels.get(RECOVERY_SNAPSHOT_LABEL))
            else {
                break;
            };
            if !reference.starts_with("localhost/xpressclaw-recovery:")
                || snapshot.repo_tags.as_ref().is_some_and(|tags| {
                    tags.iter()
                        .any(|tag| tag != reference && tag != "<none>:<none>")
                })
            {
                break;
            }
            let options = Some(RemoveImageOptions {
                force: false,
                noprune: true,
            });
            // Docker may keep a snapshot as an intermediate parent of a newer
            // commit. Drop its unique tag now, then retry the parent when its
            // last descendant is removed. Never prune unrelated/base images.
            if snapshot
                .repo_tags
                .as_ref()
                .is_some_and(|tags| tags.contains(reference))
            {
                let _ = self.docker.remove_image(reference, options, None).await;
            }
            match self.docker.remove_image(&current, options, None).await {
                Ok(_)
                | Err(BollardError::DockerResponseServerError {
                    status_code: 404, ..
                }) => {}
                Err(_) => break, // A container, child image, or external tag still needs it.
            }
            let parent = snapshot.parent.unwrap_or_default();
            // Podman returns Parent without the sha256: prefix even when the
            // inspected image ID (and therefore our base label) includes it.
            if labels
                .and_then(|labels| labels.get(BASE_IMAGE_LABEL))
                .is_some_and(|base| {
                    base.trim_start_matches("sha256:") == parent.trim_start_matches("sha256:")
                })
            {
                break;
            }
            current = parent;
        }
    }

    async fn snapshot_environment(
        &self,
        container: &str,
        mut config: ContainerConfig<String>,
    ) -> Result<String> {
        // Podman's compatibility API can return `ID` instead of Docker's
        // `Id`. Resolve a unique tag instead of relying on that response field.
        let tag = uuid::Uuid::new_v4().to_string();
        let repo = "localhost/xpressclaw-recovery";
        let reference = format!("{repo}:{tag}");
        config
            .labels
            .get_or_insert_with(HashMap::new)
            .insert(RECOVERY_SNAPSHOT_LABEL.into(), reference.clone());
        self.docker
            .commit_container(
                CommitContainerOptions {
                    container,
                    repo,
                    tag: &tag,
                    pause: true,
                    ..Default::default()
                },
                config,
            )
            .await
            .map_err(|error| {
                Error::Container(format!("Could not snapshot environment: {error}"))
            })?;
        self.verified_snapshot_id(&reference).await
    }

    async fn verified_snapshot_id(&self, reference: &str) -> Result<String> {
        let verified =
            self.docker
                .inspect_image(reference)
                .await
                .map_err(|error| {
                    Error::Container(format!("Could not verify environment snapshot: {error}"))
                })
                .and_then(|image| {
                    image.id.filter(|id| !id.is_empty()).ok_or_else(|| {
                        Error::Container("Environment snapshot has no image ID".into())
                    })
                });
        if verified.is_err() {
            // The unique tag is known even if inspection is unavailable. Do
            // not require another inspection to clean up this failed commit.
            let _ = self
                .docker
                .remove_image(
                    reference,
                    Some(RemoveImageOptions {
                        force: false,
                        noprune: true,
                    }),
                    None,
                )
                .await;
        }
        verified
    }
}

pub(super) fn container_snapshot(container: &ContainerInspectResponse) -> Option<&str> {
    let labels = container.config.as_ref()?.labels.as_ref()?;
    let image = container.image.as_deref()?;
    (labels.contains_key(RECOVERY_SOURCE_LABEL)
        && labels.get(BASE_IMAGE_LABEL).map(String::as_str) != Some(image))
    .then_some(image)
}

fn dependent_name(container: &ContainerInspectResponse) -> String {
    container
        .name
        .as_deref()
        .or(container.id.as_deref())
        .unwrap_or("unknown")
        .trim_start_matches('/')
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures_util::FutureExt;

    #[tokio::test]
    async fn failed_snapshot_verification_removes_the_committed_tag() {
        for (status, body) in [
            (
                500,
                serde_json::json!({"message": "inspection unavailable"}),
            ),
            (200, serde_json::json!({})),
            (200, serde_json::json!({"Id": ""})),
            (200, serde_json::json!({"Id": "sha256:verified"})),
        ] {
            let (docker, requests, server) = super::super::tests::mock_engine(HashMap::from([
                (
                    "GET /images/snapshot-verification/json".into(),
                    (status, body.clone()),
                ),
                (
                    "DELETE /images/snapshot-verification".into(),
                    (200, serde_json::json!([])),
                ),
            ]))
            .await;
            let result = docker.verified_snapshot_id("snapshot-verification").await;
            let valid = body["Id"] == "sha256:verified";
            assert_eq!(result.is_ok(), valid);
            let requests = requests.lock().unwrap();
            assert_eq!(
                requests
                    .iter()
                    .filter(|request| request.starts_with("DELETE "))
                    .count(),
                usize::from(!valid)
            );
            assert_eq!(
                requests
                    .iter()
                    .filter(|request| request.starts_with("GET "))
                    .count(),
                1
            );
            server.abort();
        }
    }

    async fn run(docker: &DockerManager, script: &str) -> String {
        let mut process = docker
            .open_project_process(
                "agent",
                &["node".into(), "-e".into(), script.into()],
                None,
                &[],
            )
            .await
            .unwrap();
        tokio::time::timeout(Duration::from_secs(10), async {
            let mut output = String::new();
            while let Some(chunk) = process.output.next().await {
                output.push_str(&chunk.unwrap().to_string());
            }
            output
        })
        .await
        .unwrap()
    }

    async fn exec_output(docker: &DockerManager, container: &str, script: &str) -> String {
        let exec = docker
            .docker
            .create_exec(
                container,
                CreateExecOptions {
                    attach_stdout: Some(true),
                    attach_stderr: Some(true),
                    cmd: Some(vec!["node", "-e", script]),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        let StartExecResults::Attached { mut output, .. } =
            docker.docker.start_exec(&exec.id, None).await.unwrap()
        else {
            panic!("expected output")
        };
        let mut text = String::new();
        while let Some(chunk) = output.next().await {
            text.push_str(&chunk.unwrap().to_string());
        }
        text
    }

    #[tokio::test]
    #[ignore = "requires Docker/Podman and XPRESSCLAW_RETENTION_TEST_IMAGE"]
    async fn stopped_helpers_preserve_their_files_without_blocking_recovery_or_rebuild() {
        let image = std::env::var("XPRESSCLAW_RETENTION_TEST_IMAGE").unwrap();
        let installation = format!("helpers-test-{}", uuid::Uuid::new_v4());
        let docker = DockerManager::connect_for_installation(&installation)
            .await
            .unwrap();
        let spec = ContainerSpec {
            image: image.clone(),
            expose_port: None,
            cmd: Some(vec![
                "node".into(),
                "-e".into(),
                "setInterval(()=>{},1000)".into(),
            ]),
            ..Default::default()
        };
        docker
            .launch_project_attached("agent", &spec)
            .await
            .unwrap();
        // Start with a recovered parent, so preserving it for helpers must
        // also preserve its snapshot until the archive itself is removed.
        let initial = ContainerSpec {
            environment: vec!["INITIAL=yes".into()],
            ..spec.clone()
        };
        docker
            .recover_project_configuration("agent", &initial)
            .await
            .unwrap();
        docker.finish_project_recovery("agent").await.unwrap();
        let original = docker
            .launch_project_attached("agent", &initial)
            .await
            .unwrap();
        let name = docker.owned_project_container_name("agent").unwrap();
        let parent_snapshot = docker
            .inspect_optional(&name)
            .await
            .unwrap()
            .unwrap()
            .image
            .unwrap();
        let child = docker
            .docker
            .create_container(
                Some(CreateContainerOptions {
                    name: format!("helper-{installation}"),
                    platform: None,
                }),
                ContainerConfig {
                    image: Some(image),
                    cmd: spec.cmd.clone(),
                    host_config: Some(HostConfig {
                        network_mode: Some(format!("container:{}", original.info.container_id)),
                        ..Default::default()
                    }),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        let mut snapshots = vec![parent_snapshot.clone()];
        let result = std::panic::AssertUnwindSafe(async {
            docker.docker.start_container::<String>(&child.id, None).await.unwrap();
            exec_output(&docker, &child.id, "require('fs').writeFileSync('/tmp/helper-file','helper-saved-data')").await;
            run(&docker, "require('fs').writeFileSync('/tmp/agent-file','agent-saved-data')").await;
            let changed = ContainerSpec { environment: vec!["NEW_SETTING=yes".into()], ..spec.clone() };
            let error = docker.recover_project_configuration("agent", &changed).await.unwrap_err();
            assert!(error.to_string().contains(&format!("helper-{installation}")));
            assert!(docker.is_container_running(&original.info.container_id).await);
            docker.docker.stop_container(&child.id, Some(StopContainerOptions { t: 1 })).await.unwrap();

            // Failed replacement restores the exact original, with its helper
            // still pointing to the same ID and with no archive yet committed.
            let broken = ContainerSpec { network_mode: Some(format!("missing-{installation}")), ..changed.clone() };
            assert!(docker.recover_project_configuration("agent", &broken).await.is_err());
            assert_eq!(docker.inspect_optional(&name).await.unwrap().unwrap().id.as_deref(), Some(original.info.container_id.as_str()));
            assert!(docker.preserved_project_environments("agent").await.unwrap().is_empty());

            docker.recover_project_configuration("agent", &changed).await.unwrap();
            snapshots.push(docker.inspect_optional(&name).await.unwrap().unwrap().image.unwrap());
            docker.finish_project_recovery("agent").await.unwrap();
            assert_eq!(run(&docker, "console.log(require('fs').readFileSync('/tmp/agent-file','utf8'),process.env.NEW_SETTING)").await.trim(), "agent-saved-data yes");
            let saved = docker.preserved_project_environments("agent").await.unwrap();
            assert_eq!(saved.len(), 1);
            assert_eq!(saved[0].container_id, original.info.container_id);
            assert_eq!(saved[0].dependents, vec![format!("helper-{installation}")]);
            assert!(!docker.project_configuration_needs_recovery("agent", &changed).await.unwrap());
            assert!(docker.inspect_optional(&format!("{name}-recovery")).await.unwrap().is_none());

            // A subsequent rebuild ignores historical helper dependencies and
            // uses a genuinely new filesystem. The old helper can still be
            // restarted against its original parent without any data loss.
            docker.rebuild_project_environment("agent").await.unwrap();
            assert!(docker.docker.inspect_image(&parent_snapshot).await.is_ok());
            docker.launch_project_attached("agent", &spec).await.unwrap();
            assert_eq!(run(&docker, "console.log(require('fs').existsSync('/tmp/agent-file'))").await.trim(), "false");
            docker.docker.start_container::<String>(&saved[0].container_id, None).await.unwrap();
            docker.docker.start_container::<String>(&child.id, None).await.unwrap();
            assert_eq!(exec_output(&docker, &child.id, "console.log(require('fs').readFileSync('/tmp/helper-file','utf8'))").await.trim(), "helper-saved-data");
            assert_eq!(exec_output(&docker, &saved[0].container_id, "console.log(require('fs').readFileSync('/tmp/agent-file','utf8'))").await.trim(), "agent-saved-data");
            // Agent deletion must continue to protect every preserved helper.
            assert!(docker.remove_owned_workload("agent").await.is_err());
            assert!(docker.is_running("agent").await);
        }).catch_unwind().await;
        let _ = docker
            .docker
            .remove_container(
                &child.id,
                Some(RemoveContainerOptions {
                    force: true,
                    ..Default::default()
                }),
            )
            .await;
        let _ = docker.rollback_project_recovery("agent").await;
        docker.remove_owned_workload("agent").await.unwrap();
        if result.is_ok() {
            for snapshot in &snapshots {
                assert!(matches!(
                    docker.docker.inspect_image(snapshot).await,
                    Err(BollardError::DockerResponseServerError {
                        status_code: 404,
                        ..
                    })
                ));
            }
        }
        for snapshot in snapshots {
            docker.discard_unused_snapshot(&snapshot).await;
        }
        if let Err(panic) = result {
            std::panic::resume_unwind(panic);
        }
    }

    #[tokio::test]
    #[ignore = "requires Docker/Podman and XPRESSCLAW_RETENTION_TEST_IMAGE"]
    async fn automatic_recovery_preserves_files_volumes_and_rolls_back_failures() {
        let image = std::env::var("XPRESSCLAW_RETENTION_TEST_IMAGE").unwrap();
        let installation = format!("recovery-test-{}", uuid::Uuid::new_v4());
        let docker = DockerManager::connect_for_installation(&installation)
            .await
            .unwrap();
        let name = docker.owned_project_container_name("agent").unwrap();
        let backup = format!("{name}-recovery");
        let mut spec = ContainerSpec {
            image,
            expose_port: None,
            cmd: Some(vec![
                "node".into(),
                "-e".into(),
                "setInterval(()=>{},1000)".into(),
            ]),
            ..Default::default()
        };
        // Build an isolated fixture with an anonymous volume. Its contents
        // cannot be saved by commit and must be reattached by volume identity.
        let seed = docker
            .launch_project_attached("agent", &spec)
            .await
            .unwrap();
        let fixture = docker
            .snapshot_environment(
                &seed.info.container_id,
                ContainerConfig::<String> {
                    volumes: Some(HashMap::from([("/persist".into(), HashMap::new())])),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        docker.remove_owned_workload("agent").await.unwrap();
        spec.image = fixture.clone();
        spec.environment = vec!["OLD_TOKEN=revoked-secret".into(), "VALUE=old".into()];
        let original = docker
            .launch_project_attached("agent", &spec)
            .await
            .unwrap();
        let mut snapshots = Vec::new();
        let mut volumes = Vec::new();
        let result = std::panic::AssertUnwindSafe(async {
            run(&docker, "const fs=require('fs');fs.writeFileSync('/tmp/installed-tool','keep');fs.writeFileSync('/persist/session','native-session');").await;
            let before = docker.inspect_optional(&name).await.unwrap().unwrap();
            volumes.push(before.mounts.as_ref().unwrap().iter().find(|mount| mount.destination.as_deref()==Some("/persist")).unwrap().name.clone().unwrap());
            let changed = ContainerSpec { environment: vec!["VALUE=new".into()], ..spec.clone() };
            assert!(docker.project_configuration_needs_recovery("agent", &changed).await.unwrap());
            docker.recover_project_configuration("agent", &changed).await.unwrap();
            let after = docker.inspect_optional(&name).await.unwrap().unwrap();
            snapshots.push(after.image.clone().unwrap());
            assert_ne!(after.id.as_deref(), Some(original.info.container_id.as_str()));
            assert!(docker.inspect_optional(&backup).await.unwrap().is_some());
            let values = run(&docker, "const fs=require('fs');console.log(JSON.stringify([fs.readFileSync('/tmp/installed-tool','utf8'),fs.readFileSync('/persist/session','utf8'),process.env.VALUE,process.env.OLD_TOKEN||'']));").await;
            assert_eq!(values.trim(), r#"["keep","native-session","new",""]"#);
            docker.validate_project_configuration("agent", &changed).await.unwrap();
            docker.finish_project_recovery("agent").await.unwrap();
            assert!(docker.inspect_optional(&backup).await.unwrap().is_none());
            assert!(!docker.project_configuration_needs_recovery("agent", &changed).await.unwrap());
            let kept_id = after.id.unwrap();

            // An invalid replacement network must restore the complete prior
            // container instead of converting a failed retry into data loss.
            let broken = ContainerSpec { network_mode: Some(format!("missing-{installation}")), ..changed.clone() };
            assert!(docker.recover_project_configuration("agent", &broken).await.is_err());
            assert_eq!(docker.inspect_optional(&name).await.unwrap().unwrap().id.as_deref(), Some(kept_id.as_str()));
            assert_eq!(run(&docker, "console.log(require('fs').readFileSync('/tmp/installed-tool','utf8'))").await.trim(), "keep");

            // Simulate control-plane death before ACP initialization commits
            // recovery. The next turn restores the backup and retries safely.
            let next = ContainerSpec { environment: vec!["VALUE=next".into()], ..changed.clone() };
            docker.recover_project_configuration("agent", &next).await.unwrap();
            snapshots.push(docker.inspect_optional(&name).await.unwrap().unwrap().image.unwrap());
            docker.rollback_project_recovery("agent").await.unwrap();
            assert_eq!(docker.inspect_optional(&name).await.unwrap().unwrap().id.as_deref(), Some(kept_id.as_str()));
            docker.recover_project_configuration("agent", &next).await.unwrap();
            snapshots.push(docker.inspect_optional(&name).await.unwrap().unwrap().image.unwrap());
            docker.finish_project_recovery("agent").await.unwrap();
            assert_eq!(run(&docker, "console.log(process.env.VALUE)").await.trim(), "next");
            assert_eq!(docker.retained_project_image("agent").await.unwrap().as_deref(), Some(fixture.as_str()));
            // Superseded snapshots may remain as Docker intermediate parents,
            // but their unique tags must no longer keep them alive forever.
            for snapshot in &snapshots[..snapshots.len() - 1] {
                match docker.docker.inspect_image(snapshot).await {
                    Ok(image) => assert!(image.repo_tags.unwrap_or_default().iter().all(|tag| tag == "<none>:<none>")),
                    Err(BollardError::DockerResponseServerError { status_code: 404, .. }) => {}
                    other => panic!("unexpected snapshot inspection: {other:?}"),
                }
            }
            docker.rebuild_project_environment("agent").await.unwrap();
            for snapshot in &snapshots {
                assert!(matches!(docker.docker.inspect_image(snapshot).await, Err(BollardError::DockerResponseServerError { status_code: 404, .. })));
            }
            assert!(docker.docker.inspect_image(&fixture).await.is_ok());
            // Agent deletion uses the same cleanup, without deleting its base.
            docker.launch_project_attached("agent", &spec).await.unwrap();
            let fresh = docker.inspect_optional(&name).await.unwrap().unwrap();
            volumes.push(fresh.mounts.as_ref().unwrap().iter().find(|mount| mount.destination.as_deref()==Some("/persist")).unwrap().name.clone().unwrap());
            docker.recover_project_configuration("agent", &changed).await.unwrap();
            let deleted_snapshot = docker.inspect_optional(&name).await.unwrap().unwrap().image.unwrap();
            snapshots.push(deleted_snapshot.clone());
            docker.finish_project_recovery("agent").await.unwrap();
            docker.remove_owned_workload("agent").await.unwrap();
            assert!(matches!(docker.docker.inspect_image(&deleted_snapshot).await, Err(BollardError::DockerResponseServerError { status_code: 404, .. })));
            assert!(docker.docker.inspect_image(&fixture).await.is_ok());
        }).catch_unwind().await;
        // Cleanup only this test installation's containers, volume and images.
        let _ = docker.rollback_project_recovery("agent").await;
        let _ = docker.remove_owned_workload("agent").await;
        for volume in volumes {
            let _ = docker.docker.remove_volume(&volume, None).await;
        }
        for snapshot in snapshots.iter().rev() {
            docker.discard_unused_snapshot(snapshot).await;
        }
        docker.discard_unused_snapshot(&fixture).await;
        if let Err(panic) = result {
            std::panic::resume_unwind(panic);
        }
    }
}
