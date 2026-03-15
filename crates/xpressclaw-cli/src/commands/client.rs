//! HTTP client for talking to a running xpressclaw server.

use anyhow::{Context, Result};
use reqwest::Client;
use serde::de::DeserializeOwned;

/// A thin wrapper around reqwest that hits the local xpressclaw API.
pub struct ApiClient {
    client: Client,
    base_url: String,
}

impl ApiClient {
    pub fn new(port: u16) -> Self {
        Self {
            client: Client::new(),
            base_url: format!("http://127.0.0.1:{port}/api"),
        }
    }

    pub async fn get<T: DeserializeOwned>(&self, path: &str) -> Result<T> {
        let url = format!("{}{path}", self.base_url);
        let resp = self
            .client
            .get(&url)
            .send()
            .await
            .with_context(|| format!("failed to connect to xpressclaw at {url}"))?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            anyhow::bail!("API error {status}: {body}");
        }

        resp.json().await.context("failed to parse API response")
    }

    pub async fn post<T: DeserializeOwned>(
        &self,
        path: &str,
        body: &serde_json::Value,
    ) -> Result<T> {
        let url = format!("{}{path}", self.base_url);
        let resp = self
            .client
            .post(&url)
            .json(body)
            .send()
            .await
            .with_context(|| format!("failed to connect to xpressclaw at {url}"))?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            anyhow::bail!("API error {status}: {body}");
        }

        resp.json().await.context("failed to parse API response")
    }

    pub async fn post_empty(&self, path: &str) -> Result<serde_json::Value> {
        let url = format!("{}{path}", self.base_url);
        let resp = self
            .client
            .post(&url)
            .send()
            .await
            .with_context(|| format!("failed to connect to xpressclaw at {url}"))?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            anyhow::bail!("API error {status}: {body}");
        }

        resp.json().await.context("failed to parse API response")
    }

    pub async fn patch<T: DeserializeOwned>(
        &self,
        path: &str,
        body: &serde_json::Value,
    ) -> Result<T> {
        let url = format!("{}{path}", self.base_url);
        let resp = self
            .client
            .patch(&url)
            .json(body)
            .send()
            .await
            .with_context(|| format!("failed to connect to xpressclaw at {url}"))?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            anyhow::bail!("API error {status}: {body}");
        }

        resp.json().await.context("failed to parse API response")
    }

    pub async fn delete(&self, path: &str) -> Result<()> {
        let url = format!("{}{path}", self.base_url);
        let resp = self
            .client
            .delete(&url)
            .send()
            .await
            .with_context(|| format!("failed to connect to xpressclaw at {url}"))?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            anyhow::bail!("API error {status}: {body}");
        }

        Ok(())
    }
}

/// Try to connect to the running server and return a client.
pub async fn connect(port: u16) -> Result<ApiClient> {
    let client = ApiClient::new(port);

    // Verify the server is running
    let _: serde_json::Value = client
        .get("/health")
        .await
        .context("xpressclaw is not running. Start it with `xpressclaw up`")?;

    Ok(client)
}
