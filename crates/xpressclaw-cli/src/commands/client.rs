//! HTTP client for talking to a running xpressclaw server.

use anyhow::{Context, Result};
use reqwest::Client;
use serde::de::DeserializeOwned;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

/// A thin wrapper around reqwest that hits the local xpressclaw API.
pub struct ApiClient {
    client: Client,
    base_url: String,
}

impl ApiClient {
    pub fn for_bind(bind: IpAddr, port: u16) -> Self {
        Self {
            client: Client::builder()
                // Instance lifecycle traffic is always directed at the local
                // machine. Never hand even public bootstrap responses to an
                // inherited corporate or user-configured proxy.
                .no_proxy()
                .connect_timeout(std::time::Duration::from_secs(3))
                .timeout(std::time::Duration::from_secs(10))
                .build()
                .expect("failed to build HTTP client"),
            base_url: format!("{}/api", local_instance_url(bind, port)),
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
}

/// Connect to the local endpoint represented by a configured listener.
/// Wildcard listeners are reached through the corresponding loopback family;
/// an explicitly bound interface is reached through that exact address.
pub async fn connect_to(bind: IpAddr, port: u16) -> Result<ApiClient> {
    let client = ApiClient::for_bind(bind, port);

    // Verify it's actually an xpressclaw server by hitting the health endpoint
    let health: serde_json::Value = match client.get("/health").await {
        Ok(h) => h,
        Err(_) => {
            anyhow::bail!(
                "xpressclaw is not running at {}. Start it with `xpressclaw up`",
                local_instance_url(bind, port)
            );
        }
    };

    // Sanity check: make sure it's our server, not something else on this port
    if health.get("status").is_none() {
        anyhow::bail!("port {port} is in use by another application (not xpressclaw)");
    }

    Ok(client)
}

fn local_instance_url(bind: IpAddr, port: u16) -> String {
    let address = match bind {
        IpAddr::V4(address) if address.is_unspecified() => IpAddr::V4(Ipv4Addr::LOCALHOST),
        IpAddr::V6(address) if address.is_unspecified() => IpAddr::V6(Ipv6Addr::LOCALHOST),
        address => address,
    };
    match address {
        IpAddr::V4(address) => format!("http://{address}:{port}"),
        IpAddr::V6(address) => format!("http://[{address}]:{port}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn configured_listener_addresses_map_to_reachable_local_urls() {
        assert_eq!(
            local_instance_url("0.0.0.0".parse().unwrap(), 9000),
            "http://127.0.0.1:9000"
        );
        assert_eq!(
            local_instance_url("::".parse().unwrap(), 9001),
            "http://[::1]:9001"
        );
        assert_eq!(
            local_instance_url("100.64.0.7".parse().unwrap(), 9002),
            "http://100.64.0.7:9002"
        );
    }
}
