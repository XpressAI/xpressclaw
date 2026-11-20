use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::error::Result;

/// Capabilities are explicit because GitBucket implements a useful, but not
/// complete, subset of GitHub's API.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ForgeCapabilities {
    pub repositories: bool,
    pub issues: bool,
    pub pull_requests: bool,
    pub pull_request_comments: bool,
    pub reviews: bool,
    pub events: bool,
    pub commit_statuses: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct BuildCapabilities {
    pub trigger: bool,
    pub logs: bool,
    pub artifacts: bool,
    pub cancel: bool,
    pub retry: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Repository {
    pub owner: String,
    pub name: String,
    pub clone_url: String,
    pub web_url: String,
    pub private: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Issue {
    pub number: u64,
    pub title: String,
    pub state: String,
    pub body: String,
    pub web_url: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PullRequest {
    pub number: u64,
    pub title: String,
    pub state: String,
    pub web_url: String,
    pub head: String,
    pub base: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BuildRequest {
    pub repository: String,
    pub git_ref: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Build {
    pub number: u64,
    pub state: String,
    pub url: String,
}

#[async_trait]
pub trait ForgeProvider: Send + Sync {
    fn name(&self) -> &'static str;
    fn capabilities(&self) -> ForgeCapabilities;
    async fn get_repository(&self, owner: &str, repository: &str) -> Result<Repository>;
    async fn create_repository(&self, name: &str, private: bool) -> Result<Repository>;
    async fn get_issue(&self, owner: &str, repository: &str, number: u64) -> Result<Issue>;
    async fn create_issue(
        &self,
        owner: &str,
        repository: &str,
        title: &str,
        body: &str,
    ) -> Result<Issue>;
    async fn get_pull_request(
        &self,
        owner: &str,
        repository: &str,
        number: u64,
    ) -> Result<PullRequest>;
    async fn create_pull_request(
        &self,
        owner: &str,
        repository: &str,
        title: &str,
        body: &str,
        head: &str,
        base: &str,
    ) -> Result<PullRequest>;
    async fn comment_on_pull_request(
        &self,
        owner: &str,
        repository: &str,
        number: u64,
        body: &str,
    ) -> Result<()>;
}

#[async_trait]
pub trait BuildProvider: Send + Sync {
    fn name(&self) -> &'static str;
    fn capabilities(&self) -> BuildCapabilities;
    async fn trigger(&self, request: &BuildRequest) -> Result<Build>;
    async fn get(&self, number: u64) -> Result<Build>;
    async fn logs(&self, number: u64, max_bytes: usize) -> Result<String>;
    async fn cancel(&self, number: u64) -> Result<()>;
    async fn retry(&self, number: u64) -> Result<Build>;
}
