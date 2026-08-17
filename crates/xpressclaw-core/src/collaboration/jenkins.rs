use std::time::Duration;

use async_trait::async_trait;
use serde::Deserialize;

use super::{Build, BuildCapabilities, BuildProvider, BuildRequest, GITBUCKET_INTERNAL_URL};
use crate::error::{Error, Result};

const JOB: &str = "xpressclaw-local-build";

#[derive(Clone)]
pub struct JenkinsProvider {
    client: reqwest::Client,
    base_url: String,
    username: String,
    password: String,
}

impl JenkinsProvider {
    pub fn new(base_url: &str, username: &str, password: &str) -> Self {
        Self {
            client: reqwest::Client::new(),
            base_url: base_url.trim_end_matches('/').to_string(),
            username: username.to_string(),
            password: password.to_string(),
        }
    }

    fn authenticated(&self, request: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        request.basic_auth(&self.username, Some(&self.password))
    }

    async fn crumb(&self) -> Result<Option<(String, String)>> {
        let response = self
            .authenticated(
                self.client
                    .get(format!("{}/crumbIssuer/api/json", self.base_url)),
            )
            .send()
            .await
            .map_err(|error| Error::ToolExecution(format!("Jenkins request failed: {error}")))?;
        if response.status() == reqwest::StatusCode::NOT_FOUND {
            return Ok(None);
        }
        if !response.status().is_success() {
            return Err(Error::ToolExecution(format!(
                "Jenkins crumb request returned HTTP {}",
                response.status()
            )));
        }
        let crumb: Crumb = response.json().await.map_err(|error| {
            Error::ToolExecution(format!("Jenkins returned an invalid crumb: {error}"))
        })?;
        Ok(Some((crumb.crumb_request_field, crumb.crumb)))
    }

    async fn post(&self, url: String) -> Result<reqwest::Response> {
        let mut request = self.authenticated(self.client.post(url));
        if let Some((field, crumb)) = self.crumb().await? {
            request = request.header(field, crumb);
        }
        let response = request
            .send()
            .await
            .map_err(|error| Error::ToolExecution(format!("Jenkins request failed: {error}")))?;
        if !response.status().is_success() {
            return Err(Error::ToolExecution(format!(
                "Jenkins returned HTTP {}",
                response.status()
            )));
        }
        Ok(response)
    }

    fn validate_request(request: &BuildRequest) -> Result<()> {
        let expected = format!("{GITBUCKET_INTERNAL_URL}/xpressclaw-agent/");
        if !request.repository.starts_with(&expected) || !request.repository.ends_with(".git") {
            return Err(Error::ToolPermission(
                "Jenkins builds may only clone repositories owned by the managed local forge account"
                    .to_string(),
            ));
        }
        if request.git_ref.is_empty()
            || request.git_ref.len() > 200
            || request
                .git_ref
                .contains(|character: char| character.is_control())
        {
            return Err(Error::ToolExecution("invalid Git ref".to_string()));
        }
        Ok(())
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Crumb {
    crumb_request_field: String,
    crumb: String,
}

#[derive(Deserialize)]
struct QueueItem {
    executable: Option<QueueExecutable>,
    cancelled: Option<bool>,
}

#[derive(Deserialize)]
struct QueueExecutable {
    number: u64,
    url: String,
}

#[derive(Deserialize)]
struct JenkinsBuild {
    number: u64,
    url: String,
    building: bool,
    result: Option<String>,
}

impl From<JenkinsBuild> for Build {
    fn from(build: JenkinsBuild) -> Self {
        Self {
            number: build.number,
            state: if build.building {
                "running".to_string()
            } else {
                build
                    .result
                    .unwrap_or_else(|| "queued".to_string())
                    .to_lowercase()
            },
            url: build.url,
        }
    }
}

#[async_trait]
impl BuildProvider for JenkinsProvider {
    fn name(&self) -> &'static str {
        "jenkins"
    }

    fn capabilities(&self) -> BuildCapabilities {
        BuildCapabilities {
            trigger: true,
            logs: true,
            artifacts: false,
            cancel: true,
            retry: false,
        }
    }

    async fn trigger(&self, request: &BuildRequest) -> Result<Build> {
        Self::validate_request(request)?;
        let query = serde_urlencoded::to_string([
            ("REPOSITORY_URL", request.repository.as_str()),
            ("GIT_REF", request.git_ref.as_str()),
        ])
        .map_err(|error| Error::ToolExecution(format!("invalid build request: {error}")))?;
        let response = self
            .post(format!(
                "{}/job/{JOB}/buildWithParameters?{query}",
                self.base_url
            ))
            .await?;
        let queue_location = response
            .headers()
            .get(reqwest::header::LOCATION)
            .and_then(|value| value.to_str().ok())
            .map(|value| value.trim_end_matches('/'))
            .map(ToOwned::to_owned)
            .ok_or_else(|| {
                Error::ToolExecution("Jenkins did not return a queue URL".to_string())
            })?;
        let queue_url =
            if queue_location.starts_with("http://") || queue_location.starts_with("https://") {
                queue_location
            } else {
                format!("{}{}", self.base_url, queue_location)
            };

        for _ in 0..30 {
            let item: QueueItem = self
                .authenticated(self.client.get(format!("{queue_url}/api/json")))
                .send()
                .await
                .map_err(|error| Error::ToolExecution(format!("Jenkins request failed: {error}")))?
                .json()
                .await
                .map_err(|error| {
                    Error::ToolExecution(format!("invalid Jenkins queue response: {error}"))
                })?;
            if item.cancelled.unwrap_or(false) {
                return Err(Error::ToolExecution(
                    "Jenkins cancelled the queued build".to_string(),
                ));
            }
            if let Some(executable) = item.executable {
                return Ok(Build {
                    number: executable.number,
                    state: "running".to_string(),
                    url: executable.url,
                });
            }
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
        Err(Error::ToolExecution(
            "Jenkins accepted the build but did not start it within 15 seconds".to_string(),
        ))
    }

    async fn get(&self, number: u64) -> Result<Build> {
        let response = self
            .authenticated(
                self.client
                    .get(format!("{}/job/{JOB}/{number}/api/json", self.base_url)),
            )
            .send()
            .await
            .map_err(|error| Error::ToolExecution(format!("Jenkins request failed: {error}")))?;
        if !response.status().is_success() {
            return Err(Error::ToolExecution(format!(
                "Jenkins returned HTTP {} for build {number}",
                response.status()
            )));
        }
        response
            .json::<JenkinsBuild>()
            .await
            .map(Into::into)
            .map_err(|error| {
                Error::ToolExecution(format!("invalid Jenkins build response: {error}"))
            })
    }

    async fn logs(&self, number: u64, max_bytes: usize) -> Result<String> {
        let response = self
            .authenticated(
                self.client
                    .get(format!("{}/job/{JOB}/{number}/consoleText", self.base_url)),
            )
            .send()
            .await
            .map_err(|error| Error::ToolExecution(format!("Jenkins request failed: {error}")))?;
        if !response.status().is_success() {
            return Err(Error::ToolExecution(format!(
                "Jenkins returned HTTP {} for build logs",
                response.status()
            )));
        }
        let bytes = response.bytes().await.map_err(|error| {
            Error::ToolExecution(format!("failed to read Jenkins logs: {error}"))
        })?;
        let start = bytes.len().saturating_sub(max_bytes.min(1_000_000));
        Ok(String::from_utf8_lossy(&bytes[start..]).to_string())
    }

    async fn cancel(&self, number: u64) -> Result<()> {
        self.post(format!("{}/job/{JOB}/{number}/stop", self.base_url))
            .await?;
        Ok(())
    }

    async fn retry(&self, _number: u64) -> Result<Build> {
        Err(Error::ToolExecution(
            "Jenkins retry is not supported by the pinned minimal plugin set; trigger a new build with the same repository and ref"
                .to_string(),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_requests_are_limited_to_managed_gitbucket_repositories() {
        assert!(JenkinsProvider::validate_request(&BuildRequest {
            repository: "http://gitbucket:8080/xpressclaw-agent/demo.git".to_string(),
            git_ref: "feature/demo".to_string(),
        })
        .is_ok());
        assert!(JenkinsProvider::validate_request(&BuildRequest {
            repository: "https://example.com/owner/demo.git".to_string(),
            git_ref: "main".to_string(),
        })
        .is_err());
    }

    #[test]
    fn capabilities_make_minimal_plugin_limitations_explicit() {
        let provider = JenkinsProvider::new("http://jenkins:8080", "xpressclaw", "secret");
        let capabilities = provider.capabilities();
        assert!(capabilities.trigger);
        assert!(capabilities.logs);
        assert!(capabilities.cancel);
        assert!(!capabilities.artifacts);
        assert!(!capabilities.retry);
    }
}
