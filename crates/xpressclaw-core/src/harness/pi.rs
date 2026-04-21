//! [`PiHarness`] — pi-agent backend on top of [`C2wHarness`] (ADR-023 task 4).
//!
//! PiHarness is a thin facade that applies pi-specific conventions on top
//! of the generic c2w runtime:
//!
//! - **Image resolution.** `spec.image` can be a local `.wasm` file path
//!   (development) or an OCI image ref like `ghcr.io/xpressai/harnesses/pi:v0.1.0`
//!   (production). The OCI pull path lives in [`HarnessImageResolver`]
//!   below. In this commit it's a stub — file paths work; OCI refs return
//!   a clear error until task 4b lands the real pull.
//! - **Defaults.** pi expects a writable `/workspace`, an `OPENAI_API_BASE`
//!   pointing at the xpressclaw sidecar (see ADR-023 §6), and an
//!   `XCLAW_SOCKET` path where the shell-verb bridge lives (ADR-023 §7).
//!   PiHarness seeds these into the [`ContainerSpec`] before delegating
//!   to [`C2wHarness::launch`].
//! - **Tmux visibility.** pi drives its agent loop inside a tmux session
//!   by default; the Harness trait's `attach_tmux` hook lands in task 9
//!   alongside the frontend view. For now PiHarness just makes sure the
//!   session socket path is exposed as a preopen.
//!
//! Everything else (lifecycle, logs, endpoint port) delegates to
//! [`C2wHarness`]. That means PiHarness is intentionally tiny —
//! pi-specific behavior that doesn't fit here belongs in the guest
//! harness image, not on the host side.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use async_trait::async_trait;
use tracing::debug;

use crate::error::{Error, Result};
use crate::harness::types::{ContainerInfo, ContainerSpec};
use crate::harness::{C2wHarness, Harness};

/// Default mount point inside the guest where per-agent scratch lives.
pub const DEFAULT_WORKSPACE_MOUNT: &str = "/workspace";

/// Default guest path for the `xclaw` CLI bridge socket (ADR-023 §7).
/// Host-side wiring lands in task 5; this constant reserves the guest
/// end today so pi harness images can bake it in.
pub const DEFAULT_XCLAW_SOCKET: &str = "/run/xclaw.sock";

/// Environment variable pi (and other shell-native harnesses) read to
/// find the xpressclaw LLM sidecar (ADR-023 §6). Full routing lands in
/// task 6; PiHarness reserves the name today.
pub const LLM_ENDPOINT_ENV: &str = "OPENAI_API_BASE";

/// Environment variable the guest-side `xclaw` CLI uses to find its
/// host socket.
pub const XCLAW_SOCKET_ENV: &str = "XCLAW_SOCKET";

/// Resolves a `spec.image` reference (file path or OCI ref) to a
/// concrete on-disk WASM module path.
///
/// Resolution order:
/// 1. If `image_ref` is an existing filesystem path, use it directly.
///    (The dev and smoke-test flow.)
/// 2. If the fallback mode is enabled (constructor
///    [`HarnessImageResolver::with_fallback`]) and the ref isn't a
///    file, write the bundled noop-harness WASM into the cache dir
///    and return that. This keeps the desktop app runnable end-to-end
///    before a real pi image exists on GHCR.
/// 3. Otherwise return a clear error naming the OCI-pull follow-up
///    (task 10 phase 2).
pub struct HarnessImageResolver {
    cache_dir: PathBuf,
    use_bundled_fallback: bool,
}

/// Minimal WASI preview-1 guest used as a stand-in until the real pi
/// harness image is published to GHCR. Its `_start` returns immediately,
/// so agents "launch" cleanly and the lifecycle UI works; conversations
/// fall back to the LLM router because the guest doesn't host an
/// endpoint. This is enough to exercise the desktop app end-to-end.
const BUNDLED_FALLBACK_WAT: &str = r#"
    (module
      (memory (export "memory") 1)
      (func (export "_start")))
"#;

const BUNDLED_FALLBACK_FILENAME: &str = "bundled-noop-harness.wasm";

impl HarnessImageResolver {
    pub fn new(cache_dir: PathBuf) -> Self {
        Self {
            cache_dir,
            use_bundled_fallback: false,
        }
    }

    /// Construct a resolver that falls back to the bundled noop harness
    /// when the image ref can't be resolved. Used by the real server
    /// startup path — OCI pulls aren't implemented yet, and without
    /// this fallback every agent launch would fail.
    pub fn with_fallback(cache_dir: PathBuf) -> Self {
        Self {
            cache_dir,
            use_bundled_fallback: true,
        }
    }

    pub fn cache_dir(&self) -> &Path {
        &self.cache_dir
    }

    /// Resolve `image_ref` to a local `.wasm` path.
    ///
    /// Dispatches on shape:
    /// - Existing filesystem path → use directly.
    /// - `registry/repo:tag` (contains `/` and `:`) → OCI pull.
    /// - Otherwise, with fallback enabled, write the bundled noop
    ///   wasm; without, error.
    pub async fn resolve(&self, image_ref: &str) -> Result<PathBuf> {
        let as_path = PathBuf::from(image_ref);
        if as_path.is_file() {
            return Ok(as_path);
        }
        if looks_like_oci_ref(image_ref) {
            return self.pull_oci_artifact(image_ref).await;
        }
        if self.use_bundled_fallback {
            return self.materialize_bundled_fallback();
        }
        Err(Error::Container(format!(
            "could not resolve image {image_ref:?}: not a file, not an OCI ref, \
             and no fallback configured. Cache dir would be {}.",
            self.cache_dir.display()
        )))
    }

    /// Pull `image_ref` as an OCI artifact and cache the first non-empty
    /// layer's blob to disk. Suitable for the "single WASM module shipped
    /// as an OCI artifact" pattern that `oras push` produces, and what
    /// GHCR stores for harness images.
    ///
    /// Cached by manifest digest so repeated resolves are fast and we
    /// don't redownload when a tag points to the same content.
    async fn pull_oci_artifact(&self, image_ref: &str) -> Result<PathBuf> {
        use oci_client::secrets::RegistryAuth;
        use oci_client::{Client, Reference};

        let reference: Reference = image_ref
            .parse()
            .map_err(|e| Error::Container(format!("invalid OCI ref {image_ref:?}: {e}")))?;

        let mut client_config = oci_client::client::ClientConfig::default();
        // Local podman/docker registries typically expose HTTP on
        // localhost without TLS. Allow that for test workflows.
        if is_local_registry(reference.registry()) {
            client_config.protocol = oci_client::client::ClientProtocol::Http;
        }
        let client = Client::new(client_config);

        // Token auth from env var — simplest path for GHCR. Falls back
        // to anonymous for public images and local registries.
        let auth = match std::env::var("XPRESSCLAW_REGISTRY_TOKEN").ok() {
            Some(tok) => RegistryAuth::Bearer(tok),
            None => RegistryAuth::Anonymous,
        };

        let (manifest, manifest_digest) = client
            .pull_image_manifest(&reference, &auth)
            .await
            .map_err(|e| Error::Container(format!("OCI manifest for {image_ref}: {e}")))?;

        // Cache path keyed by digest so retagged-but-unchanged content
        // doesn't re-pull and stale cache entries don't serve the wrong bytes.
        let safe_digest = manifest_digest.replace(':', "-");
        let dest = self.cache_dir.join(format!("{safe_digest}.wasm"));
        if dest.is_file() {
            return Ok(dest);
        }
        std::fs::create_dir_all(&self.cache_dir).map_err(|e| {
            Error::Container(format!(
                "create cache dir {}: {e}",
                self.cache_dir.display()
            ))
        })?;

        let layer = manifest.layers.first().ok_or_else(|| {
            Error::Container(format!("OCI manifest for {image_ref} has no layers"))
        })?;
        let mut blob = Vec::new();
        client
            .pull_blob(&reference, layer, &mut blob)
            .await
            .map_err(|e| Error::Container(format!("OCI blob pull for {image_ref}: {e}")))?;

        std::fs::write(&dest, &blob)
            .map_err(|e| Error::Container(format!("write {}: {e}", dest.display())))?;
        tracing::info!(
            image = image_ref,
            digest = manifest_digest,
            size = blob.len(),
            path = %dest.display(),
            "pulled OCI artifact"
        );
        Ok(dest)
    }

    fn materialize_bundled_fallback(&self) -> Result<PathBuf> {
        let dest = self.cache_dir.join(BUNDLED_FALLBACK_FILENAME);
        if dest.is_file() {
            return Ok(dest);
        }
        std::fs::create_dir_all(&self.cache_dir).map_err(|e| {
            Error::Container(format!(
                "create cache dir {}: {e}",
                self.cache_dir.display()
            ))
        })?;
        let wasm = wat::parse_str(BUNDLED_FALLBACK_WAT)
            .map_err(|e| Error::Container(format!("bundled harness wat: {e}")))?;
        std::fs::write(&dest, &wasm)
            .map_err(|e| Error::Container(format!("write {}: {e}", dest.display())))?;
        Ok(dest)
    }
}

/// True if `image_ref` looks like an OCI registry reference
/// (host[:port]/path[:tag]). Heuristic: contains a `/`, and the part
/// before the first `/` has a `.` or `:` or is `localhost`. This
/// distinguishes `registry:5000/foo:tag` from a filesystem path like
/// `/tmp/foo:tag` (no authority component).
fn looks_like_oci_ref(image_ref: &str) -> bool {
    let Some(slash_pos) = image_ref.find('/') else {
        return false;
    };
    let authority = &image_ref[..slash_pos];
    if authority.is_empty() {
        return false;
    }
    authority == "localhost" || authority.contains('.') || authority.contains(':')
}

/// Heuristic: is this a local registry address? Used to allow plain
/// HTTP for `podman run -p 5000:5000 registry:2` style test setups.
fn is_local_registry(host_port: &str) -> bool {
    let host = host_port.split(':').next().unwrap_or(host_port);
    host == "localhost" || host == "127.0.0.1" || host == "::1"
}

/// Pi-agent harness. Wraps [`C2wHarness`] and applies pi's expected
/// environment on every launch.
pub struct PiHarness {
    inner: Arc<C2wHarness>,
    resolver: HarnessImageResolver,
    /// Parent directory where per-agent workspace dirs live on the host.
    /// Each agent gets `<workspaces_root>/<agent_id>/` mounted into the
    /// guest as [`DEFAULT_WORKSPACE_MOUNT`].
    workspaces_root: PathBuf,
}

impl PiHarness {
    pub fn new(
        inner: Arc<C2wHarness>,
        resolver: HarnessImageResolver,
        workspaces_root: PathBuf,
    ) -> Self {
        Self {
            inner,
            resolver,
            workspaces_root,
        }
    }

    /// Ensure the per-agent workspace dir exists on the host.
    fn ensure_workspace(&self, agent_id: &str) -> Result<PathBuf> {
        let dir = self.workspaces_root.join(agent_id);
        std::fs::create_dir_all(&dir).map_err(|e| {
            Error::Container(format!(
                "creating workspace {} for agent {}: {}",
                dir.display(),
                agent_id,
                e
            ))
        })?;
        Ok(dir)
    }

    /// Seed pi-expected env vars and mounts onto the caller-provided
    /// spec, replacing `spec.image` with the resolved on-disk module
    /// path.
    async fn build_pi_spec(&self, agent_id: &str, spec: &ContainerSpec) -> Result<ContainerSpec> {
        let wasm_path = self.resolver.resolve(&spec.image).await?;
        let workspace = self.ensure_workspace(agent_id)?;

        let mut built = spec.clone();
        built.image = wasm_path.to_string_lossy().into_owned();

        // Preopen workspace. Don't duplicate if the caller already asked
        // for the same target.
        let ws_target = DEFAULT_WORKSPACE_MOUNT.to_string();
        if !built.volumes.iter().any(|v| v.target == ws_target) {
            built.volumes.push(crate::harness::types::VolumeMount {
                source: workspace.to_string_lossy().into_owned(),
                target: ws_target,
                read_only: false,
            });
        }

        // Seed env defaults. Caller wins if they set the same name.
        let set_env = |env: &mut Vec<String>, name: &str, default: &str| {
            if !env.iter().any(|e| e.starts_with(&format!("{name}="))) {
                env.push(format!("{name}={default}"));
            }
        };
        // NB: tasks 5/6 make these endpoints real. Today the values are
        // placeholders so pi harness images can be built expecting the
        // right names.
        set_env(
            &mut built.environment,
            LLM_ENDPOINT_ENV,
            "http://127.0.0.1:8935/v1",
        );
        set_env(
            &mut built.environment,
            XCLAW_SOCKET_ENV,
            DEFAULT_XCLAW_SOCKET,
        );

        Ok(built)
    }
}

#[async_trait]
impl Harness for PiHarness {
    fn kind(&self) -> &'static str {
        "pi"
    }

    async fn launch(&self, agent_id: &str, spec: &ContainerSpec) -> Result<ContainerInfo> {
        let pi_spec = self.build_pi_spec(agent_id, spec).await?;
        debug!(
            agent_id,
            image = %pi_spec.image,
            mounts = pi_spec.volumes.len(),
            env_vars = pi_spec.environment.len(),
            "launching pi harness guest"
        );
        self.inner.launch(agent_id, &pi_spec).await
    }

    async fn stop(&self, agent_id: &str) -> Result<()> {
        self.inner.stop(agent_id).await
    }

    async fn stop_all(&self) -> Result<()> {
        self.inner.stop_all().await
    }

    async fn list(&self) -> Result<Vec<ContainerInfo>> {
        self.inner.list().await
    }

    async fn logs(&self, agent_id: &str, tail: usize) -> Result<String> {
        self.inner.logs(agent_id, tail).await
    }

    async fn is_running(&self, agent_id: &str) -> bool {
        self.inner.is_running(agent_id).await
    }

    async fn uptime_secs(&self, agent_id: &str) -> u64 {
        self.inner.uptime_secs(agent_id).await
    }

    async fn endpoint_port(&self, agent_id: &str) -> Option<u16> {
        self.inner.endpoint_port(agent_id).await
    }

    async fn ensure_image(&self, image: &str) -> Result<()> {
        // Task 4b will actually pull from GHCR here. Today, file paths
        // don't need pulling; any non-file ref is rejected early at
        // resolve-time.
        let _ = self.resolver.resolve(image).await?;
        Ok(())
    }

    async fn image_matches(&self, agent_id: &str, expected: &str) -> Result<bool> {
        // Delegate — C2wHarness stores spec.image on the RunningAgent,
        // which for pi is the resolved on-disk path. Callers comparing
        // against the raw pi image ref should do their own resolution
        // first; task 4b revisits this once OCI digests are in play.
        self.inner.image_matches(agent_id, expected).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::c2w::C2wRuntime;
    use tempfile::{tempdir, NamedTempFile};

    const NOOP_WASM_WAT: &str = r#"
        (module
          (memory (export "memory") 1)
          (func (export "_start")))
    "#;

    #[tokio::test]
    async fn launch_seeds_pi_defaults_and_delegates_to_c2w() {
        let wasm = wat::parse_str(NOOP_WASM_WAT).expect("valid wat");
        let wasm_file = NamedTempFile::new().expect("tmpfile");
        std::fs::write(wasm_file.path(), &wasm).expect("write wasm");

        let cache = tempdir().expect("cache dir");
        let workspaces = tempdir().expect("workspaces dir");

        let runtime = C2wRuntime::new().expect("runtime");
        let c2w = Arc::new(C2wHarness::new(runtime, cache.path().to_path_buf()));
        let pi = PiHarness::new(
            c2w,
            HarnessImageResolver::new(cache.path().to_path_buf()),
            workspaces.path().to_path_buf(),
        );

        let spec = ContainerSpec {
            image: wasm_file.path().to_string_lossy().into_owned(),
            ..ContainerSpec::default()
        };

        let info = pi.launch("piggy", &spec).await.expect("launch");
        assert_eq!(info.agent_id, "piggy");

        // Per-agent workspace should exist on the host.
        assert!(workspaces.path().join("piggy").is_dir());

        // Guest exits immediately; poll briefly.
        for _ in 0..50 {
            if !pi.is_running("piggy").await {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        assert!(!pi.is_running("piggy").await);
        pi.stop("piggy").await.expect("stop");
    }

    #[tokio::test]
    async fn resolver_rejects_unknown_ref_without_fallback() {
        let cache = tempdir().expect("cache");
        let r = HarnessImageResolver::new(cache.path().to_path_buf());
        // Non-OCI, non-file → no resolution possible, no fallback.
        let err = r
            .resolve("placeholder-harness")
            .await
            .expect_err("should error without fallback");
        let msg = format!("{err}");
        assert!(
            msg.contains("could not resolve"),
            "error should explain why, got: {msg}"
        );
    }

    #[tokio::test]
    async fn resolver_with_fallback_materializes_bundled_wasm() {
        let cache = tempdir().expect("cache");
        let r = HarnessImageResolver::with_fallback(cache.path().to_path_buf());
        // Non-OCI, non-file ref → fallback kicks in.
        let path = r
            .resolve("placeholder-harness")
            .await
            .expect("fallback resolves");
        assert!(path.is_file(), "resolved path should exist");
        assert_eq!(
            path.file_name().and_then(|n| n.to_str()),
            Some(BUNDLED_FALLBACK_FILENAME)
        );
    }

    #[test]
    fn oci_ref_heuristic() {
        assert!(looks_like_oci_ref("ghcr.io/foo/bar:tag"));
        assert!(looks_like_oci_ref("localhost:5000/pi:test"));
        assert!(looks_like_oci_ref("registry.example.com/pi"));
        assert!(!looks_like_oci_ref("/tmp/foo.wasm"));
        assert!(!looks_like_oci_ref("pi:latest"));
        assert!(!looks_like_oci_ref("placeholder"));
    }

    #[test]
    fn local_registry_detection() {
        assert!(is_local_registry("localhost:5000"));
        assert!(is_local_registry("127.0.0.1:5000"));
        assert!(is_local_registry("localhost"));
        assert!(!is_local_registry("ghcr.io"));
        assert!(!is_local_registry("registry.example.com:5000"));
    }
}
