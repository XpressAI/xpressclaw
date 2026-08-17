use async_trait::async_trait;
use reqwest::header::{HeaderValue, AUTHORIZATION};
use serde::Deserialize;
use serde_json::json;

use super::{ForgeCapabilities, ForgeProvider, Issue, PullRequest, Repository};
use crate::error::{Error, Result};

#[derive(Clone)]
pub struct GitBucketProvider {
    client: reqwest::Client,
    base_url: String,
    owner: String,
    authorization: HeaderValue,
}

impl GitBucketProvider {
    pub fn new(base_url: &str, owner: &str, token: &str) -> Result<Self> {
        let authorization = HeaderValue::from_str(&format!("token {token}"))
            .map_err(|_| Error::Config("invalid GitBucket service token".to_string()))?;
        Ok(Self {
            client: reqwest::Client::new(),
            base_url: base_url.trim_end_matches('/').to_string(),
            owner: owner.to_string(),
            authorization,
        })
    }

    fn api_url(&self, path: &str) -> String {
        format!("{}/api/v3{}", self.base_url, path)
    }

    fn ensure_managed_owner(&self, owner: &str, operation: &str) -> Result<()> {
        if owner != self.owner {
            return Err(Error::ToolPermission(format!(
                "the local forge account may only {operation} in {}/ repositories",
                self.owner
            )));
        }
        Ok(())
    }

    async fn response<T: for<'de> Deserialize<'de>>(
        &self,
        request: reqwest::RequestBuilder,
    ) -> Result<T> {
        let response = request
            .header(AUTHORIZATION, self.authorization.clone())
            .send()
            .await
            .map_err(|error| Error::ToolExecution(format!("GitBucket request failed: {error}")))?;
        let status = response.status();
        if !status.is_success() {
            let detail = response.text().await.unwrap_or_default();
            return Err(Error::ToolExecution(format!(
                "GitBucket returned HTTP {status}: {}",
                detail.chars().take(500).collect::<String>()
            )));
        }
        response.json().await.map_err(|error| {
            Error::ToolExecution(format!("GitBucket returned an invalid response: {error}"))
        })
    }
}

#[derive(Deserialize)]
struct ApiRepository {
    name: String,
    #[serde(default)]
    private: bool,
    html_url: String,
    clone_url: String,
    owner: ApiOwner,
}

#[derive(Deserialize)]
struct ApiOwner {
    login: String,
}

impl From<ApiRepository> for Repository {
    fn from(repository: ApiRepository) -> Self {
        Self {
            owner: repository.owner.login,
            name: repository.name,
            clone_url: repository.clone_url,
            web_url: repository.html_url,
            private: repository.private,
        }
    }
}

#[derive(Deserialize)]
struct ApiPullRequest {
    number: u64,
    title: String,
    state: String,
    html_url: String,
    head: ApiPullRef,
    base: ApiPullRef,
}

#[derive(Deserialize)]
struct ApiPullRef {
    #[serde(rename = "ref")]
    reference: String,
}

#[derive(Deserialize)]
struct ApiIssue {
    number: u64,
    title: String,
    state: String,
    #[serde(default)]
    body: String,
    html_url: String,
}

impl From<ApiIssue> for Issue {
    fn from(issue: ApiIssue) -> Self {
        Self {
            number: issue.number,
            title: issue.title,
            state: issue.state,
            body: issue.body,
            web_url: issue.html_url,
        }
    }
}

impl From<ApiPullRequest> for PullRequest {
    fn from(pull_request: ApiPullRequest) -> Self {
        Self {
            number: pull_request.number,
            title: pull_request.title,
            state: pull_request.state,
            web_url: pull_request.html_url,
            head: pull_request.head.reference,
            base: pull_request.base.reference,
        }
    }
}

#[async_trait]
impl ForgeProvider for GitBucketProvider {
    fn name(&self) -> &'static str {
        "gitbucket"
    }

    fn capabilities(&self) -> ForgeCapabilities {
        ForgeCapabilities {
            repositories: true,
            issues: true,
            pull_requests: true,
            pull_request_comments: true,
            reviews: false,
            events: false,
            commit_statuses: false,
        }
    }

    async fn get_repository(&self, owner: &str, repository: &str) -> Result<Repository> {
        self.response::<ApiRepository>(
            self.client
                .get(self.api_url(&format!("/repos/{owner}/{repository}"))),
        )
        .await
        .map(Into::into)
    }

    async fn create_repository(&self, name: &str, private: bool) -> Result<Repository> {
        self.response::<ApiRepository>(
            self.client
                .post(self.api_url("/user/repos"))
                .json(&json!({ "name": name, "private": private })),
        )
        .await
        .map(Into::into)
    }

    async fn get_issue(&self, owner: &str, repository: &str, number: u64) -> Result<Issue> {
        self.response::<ApiIssue>(
            self.client
                .get(self.api_url(&format!("/repos/{owner}/{repository}/issues/{number}"))),
        )
        .await
        .map(Into::into)
    }

    async fn create_issue(
        &self,
        owner: &str,
        repository: &str,
        title: &str,
        body: &str,
    ) -> Result<Issue> {
        self.ensure_managed_owner(owner, "create issues")?;
        self.response::<ApiIssue>(
            self.client
                .post(self.api_url(&format!("/repos/{owner}/{repository}/issues")))
                .json(&json!({ "title": title, "body": body })),
        )
        .await
        .map(Into::into)
    }

    async fn get_pull_request(
        &self,
        owner: &str,
        repository: &str,
        number: u64,
    ) -> Result<PullRequest> {
        self.response::<ApiPullRequest>(
            self.client
                .get(self.api_url(&format!("/repos/{owner}/{repository}/pulls/{number}"))),
        )
        .await
        .map(Into::into)
    }

    async fn create_pull_request(
        &self,
        owner: &str,
        repository: &str,
        title: &str,
        body: &str,
        head: &str,
        base: &str,
    ) -> Result<PullRequest> {
        self.ensure_managed_owner(owner, "open pull requests")?;
        self.response::<ApiPullRequest>(
            self.client
                .post(self.api_url(&format!("/repos/{owner}/{repository}/pulls")))
                .json(&json!({
                    "title": title,
                    "body": body,
                    "head": head,
                    "base": base,
                })),
        )
        .await
        .map(Into::into)
    }

    async fn comment_on_pull_request(
        &self,
        owner: &str,
        repository: &str,
        number: u64,
        body: &str,
    ) -> Result<()> {
        self.ensure_managed_owner(owner, "comment")?;
        let _: serde_json::Value = self
            .response(
                self.client
                    .post(self.api_url(&format!(
                        "/repos/{owner}/{repository}/issues/{number}/comments"
                    )))
                    .json(&json!({ "body": body })),
            )
            .await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capabilities_do_not_overclaim_github_compatibility() {
        let provider =
            GitBucketProvider::new("http://gitbucket:8080", "xpressclaw", "secret").unwrap();
        let capabilities = provider.capabilities();
        assert!(capabilities.pull_requests);
        assert!(capabilities.pull_request_comments);
        assert!(!capabilities.reviews);
        assert!(!capabilities.events);
        assert!(!capabilities.commit_statuses);
    }

    #[test]
    fn mutations_are_scoped_to_the_managed_owner() {
        let provider =
            GitBucketProvider::new("http://gitbucket:8080", "xpressclaw", "secret").unwrap();
        assert!(provider.ensure_managed_owner("xpressclaw", "write").is_ok());
        assert!(provider.ensure_managed_owner("root", "write").is_err());
    }
}
