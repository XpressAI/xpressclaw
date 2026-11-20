//! Direct TCP/UDP permissions. Web requests retain their separate proxy policy.
use std::net::IpAddr;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NetworkProtocol {
    #[default]
    Tcp,
    Udp,
}

impl NetworkProtocol {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Tcp => "tcp",
            Self::Udp => "udp",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct NetworkRule {
    pub name: String,
    pub enabled: bool,
    pub protocol: NetworkProtocol,
    pub destination: String,
    /// Comma-separated ports or inclusive ranges, e.g. `22,2222,5000-5010`.
    pub ports: String,
}

impl Default for NetworkRule {
    fn default() -> Self {
        Self {
            name: "SSH".into(),
            enabled: true,
            protocol: NetworkProtocol::Tcp,
            destination: "*".into(),
            ports: "22".into(),
        }
    }
}

pub fn port_ranges(ports: &str) -> Result<Vec<(u16, u16)>, String> {
    let parse = |value: &str| -> Result<u16, String> {
        let value = value.trim();
        if value.is_empty() || !value.bytes().all(|b| b.is_ascii_digit()) {
            return Err("Use ports or ranges such as 22,2222,5000-5010".into());
        }
        value
            .parse::<u16>()
            .ok()
            .filter(|port| *port != 0)
            .ok_or_else(|| "Ports must be between 1 and 65535".into())
    };
    let mut ranges = Vec::new();
    for entry in ports.split(',') {
        let (start, end) = match entry.split_once('-') {
            Some((start, end)) => (parse(start)?, parse(end)?),
            None => {
                let port = parse(entry)?;
                (port, port)
            }
        };
        if start > end {
            return Err("Port range start must not exceed its end".into());
        }
        ranges.push((start, end));
    }
    if ranges.len() > 16 {
        return Err("Use at most 16 ports or ranges per rule".into());
    }
    Ok(ranges)
}

fn validate_destination(destination: &str) -> Result<(), String> {
    if destination == "*" || destination.parse::<IpAddr>().is_ok() {
        return Ok(());
    }
    if destination.split('.').count() == 4
        && destination.bytes().all(|b| b.is_ascii_digit() || b == b'.')
    {
        return Err("Use a valid IPv4 address".into());
    }
    if let Some((address, prefix)) = destination.split_once('/') {
        let valid = prefix.bytes().all(|b| b.is_ascii_digit())
            && address
                .parse::<IpAddr>()
                .ok()
                .zip(prefix.parse::<u8>().ok())
                .is_some_and(|(address, prefix)| {
                    prefix <= if address.is_ipv4() { 32 } else { 128 }
                });
        if valid {
            return Ok(());
        }
        return Err("Use a valid IP address and CIDR prefix".into());
    }
    if destination.starts_with("*.") {
        return Err(
            "Network destinations use an exact hostname, IP, CIDR, or * for any destination".into(),
        );
    }
    super::validate_domains(&[destination.to_string()])
}

pub fn validate_rules(rules: &[NetworkRule]) -> Result<(), String> {
    if rules.len() > 64 {
        return Err("At most 64 direct network rules are supported".into());
    }
    for (index, rule) in rules.iter().enumerate() {
        let prefix = format!("Network rule {}", index + 1);
        if rule.name.len() > 128 {
            return Err(format!("{prefix}: name is too long"));
        }
        validate_destination(&rule.destination)
            .map_err(|message| format!("{prefix}: {message}"))?;
        port_ranges(&rule.ports).map_err(|message| format!("{prefix}: {message}"))?;
    }
    Ok(())
}

/// Only validated atoms reach the guard. Names and free-form text never become
/// shell commands. Each line is a protocol, destination, and one port/range.
pub fn render_rules(rules: &[NetworkRule]) -> Result<String, String> {
    validate_rules(rules)?;
    let mut output = String::new();
    for rule in rules.iter().filter(|rule| rule.enabled) {
        for (start, end) in port_ranges(&rule.ports)? {
            let ports = if start == end {
                start.to_string()
            } else {
                format!("{start}:{end}")
            };
            output.push_str(&format!(
                "{}\t{}\t{ports}\n",
                rule.protocol.as_str(),
                rule.destination.to_ascii_lowercase()
            ));
        }
    }
    Ok(output)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_restore_ssh_and_serialize_custom_services() {
        assert_eq!(
            render_rules(&[NetworkRule::default()]).unwrap(),
            "tcp\t*\t22\n"
        );
        let mut rule = NetworkRule {
            protocol: NetworkProtocol::Udp,
            destination: "10.0.0.0/8".into(),
            ports: "53, 5000-5010".into(),
            ..Default::default()
        };
        assert_eq!(
            render_rules(&[rule.clone()]).unwrap(),
            "udp\t10.0.0.0/8\t53\nudp\t10.0.0.0/8\t5000:5010\n"
        );
        rule.enabled = false;
        assert!(render_rules(&[rule]).unwrap().is_empty());
    }

    #[test]
    fn reject_bad_ports_destinations_and_shell_input() {
        for ports in ["", "0", "65536", "22;id", "20-10", "+22", "22,"] {
            assert!(port_ranges(ports).is_err(), "{ports}");
        }
        for destination in [
            "",
            "*.example.com",
            "https://example.com",
            "host;id",
            "$(id)",
            "-j ACCEPT",
            "10.0.0.0/33",
            "::/129",
        ] {
            assert!(validate_destination(destination).is_err(), "{destination}");
        }
        for destination in [
            "*",
            "db.example.com",
            "192.0.2.1",
            "192.0.2.0/24",
            "2001:db8::1",
            "2001:db8::/32",
        ] {
            validate_destination(destination).unwrap();
        }
    }
}
