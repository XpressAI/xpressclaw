//! Apply changed launch settings without discarding an Agent's filesystem.
//! Callers hold the exclusive Agent runtime boundary until ACP initializes.
use super::*;
use bollard::container::RenameContainerOptions;
use bollard::image::{CommitContainerOptions, RemoveImageOptions};

const RECOVERY_SOURCE_LABEL: &str = "io.xpressclaw.recovery-source";

impl DockerManager {
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
        self.ensure_no_container_dependents(&name).await?;
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
        self.remove_container_reference(backup.id.as_deref().unwrap(), agent)
            .await
    }

    pub async fn rollback_project_recovery(&self, agent: &str) -> Result<()> {
        let name = self.owned_project_container_name(agent)?;
        let Some(backup) = self.inspect_optional(&format!("{name}-recovery")).await? else {
            return Ok(());
        };
        self.validate_recovery_owner(&backup, agent)?;
        let mut snapshot = None;
        if let Some(replacement) = self.inspect_optional(&name).await? {
            self.validate_recovery_pair(&backup, &replacement, agent)?;
            snapshot = replacement.image;
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
        if let Some(snapshot) = snapshot {
            self.discard_unused_snapshot(&snapshot).await;
        }
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

    async fn discard_unused_snapshot(&self, image: &str) {
        // Never force deletion of an image still used by any container.
        let _ = self
            .docker
            .remove_image(
                image,
                Some(RemoveImageOptions {
                    force: false,
                    noprune: true,
                }),
                None,
            )
            .await;
    }

    async fn snapshot_environment(
        &self,
        container: &str,
        config: ContainerConfig<String>,
    ) -> Result<String> {
        // Podman's compatibility API can return `ID` instead of Docker's
        // `Id`. Resolve a unique tag instead of relying on that response field.
        let tag = uuid::Uuid::new_v4().to_string();
        let repo = "localhost/xpressclaw-recovery";
        let reference = format!("{repo}:{tag}");
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
        self.docker
            .inspect_image(&reference)
            .await
            .map_err(|error| {
                Error::Container(format!("Could not verify environment snapshot: {error}"))
            })?
            .id
            .filter(|id| !id.is_empty())
            .ok_or_else(|| Error::Container("Environment snapshot has no image ID".into()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures_util::FutureExt;

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
        let mut volume = None;
        let result = std::panic::AssertUnwindSafe(async {
            run(&docker, "const fs=require('fs');fs.writeFileSync('/tmp/installed-tool','keep');fs.writeFileSync('/persist/session','native-session');").await;
            let before = docker.inspect_optional(&name).await.unwrap().unwrap();
            volume = before.mounts.as_ref().unwrap().iter().find(|mount| mount.destination.as_deref()==Some("/persist")).unwrap().name.clone();
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
        }).catch_unwind().await;
        // Cleanup only this test installation's containers, volume and images.
        let _ = docker.rollback_project_recovery("agent").await;
        let _ = docker.remove_owned_workload("agent").await;
        if let Some(volume) = volume {
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
