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
            client: Client::builder()
                .connect_timeout(std::time::Duration::from_secs(3))
                .timeout(std::time::Duration::from_secs(10))
                .build()
                .expect("failed to build HTTP client"),
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

    // Verify it's actually an xpressclaw server by hitting the health endpoint
    let health: serde_json::Value = match client.get("/health").await {
        Ok(h) => h,
        Err(_) => {
            anyhow::bail!(
                "xpressclaw is not running on port {port}. Start it with `xpressclaw up`"
            );
        }
    };

    // Sanity check: make sure it's our server, not something else on this port
    if health.get("status").is_none() {
        anyhow::bail!("port {port} is in use by another application (not xpressclaw)");
    }

    Ok(client)
}
