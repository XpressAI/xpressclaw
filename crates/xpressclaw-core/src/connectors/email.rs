use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use async_trait::async_trait;
use serde_json::{json, Value};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tracing::{debug, error, info, warn};

use crate::error::{Error, Result};

use super::traits::{ChannelConfig, Connector, ConnectorEvent, SinkMessage, ValidationResult};

/// Email connector using IMAP for polling and SMTP for sending.
///
/// **Source**: Polls IMAP folder for unseen messages.
/// **Sink**: Sends emails via SMTP using `lettre`.
pub struct EmailConnector {
    imap_host: Option<String>,
    imap_port: Option<u16>,
    smtp_host: Option<String>,
    smtp_port: Option<u16>,
    username: Option<String>,
    password: Option<String>,
    channels: Vec<ChannelConfig>,
    connector_id: String,
    shutdown: Arc<AtomicBool>,
    poll_handle: Option<JoinHandle<()>>,
}

impl Default for EmailConnector {
    fn default() -> Self {
        Self::new()
    }
}

impl EmailConnector {
    pub fn new() -> Self {
        Self {
            imap_host: None,
            imap_port: None,
            smtp_host: None,
            smtp_port: None,
            username: None,
            password: None,
            channels: Vec::new(),
            connector_id: String::new(),
            shutdown: Arc::new(AtomicBool::new(false)),
            poll_handle: None,
        }
    }
}

#[async_trait]
impl Connector for EmailConnector {
    fn connector_type(&self) -> &str {
        "email"
    }

    async fn validate_config(&self, config: &Value) -> ValidationResult {
        let imap_host = config
            .get("imap_host")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let smtp_host = config
            .get("smtp_host")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let username = config
            .get("username")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let password = config
            .get("password")
            .and_then(|v| v.as_str())
            .unwrap_or("");

        if imap_host.is_empty() {
            return ValidationResult {
                valid: false,
                error: Some("imap_host is required".to_string()),
            };
        }

        if smtp_host.is_empty() {
            return ValidationResult {
                valid: false,
                error: Some("smtp_host is required".to_string()),
            };
        }

        if username.is_empty() {
            return ValidationResult {
                valid: false,
                error: Some("username is required".to_string()),
            };
        }

        if password.is_empty() {
            return ValidationResult {
                valid: false,
                error: Some("password is required".to_string()),
            };
        }

        ValidationResult {
            valid: true,
            error: None,
        }
    }

    async fn start(
        &mut self,
        config: &Value,
        channels: &[ChannelConfig],
        event_tx: mpsc::Sender<ConnectorEvent>,
    ) -> Result<()> {
        let imap_host = config
            .get("imap_host")
            .and_then(|v| v.as_str())
            .ok_or_else(|| Error::Connector("imap_host is required".to_string()))?
            .to_string();
        let imap_port = config
            .get("imap_port")
            .and_then(|v| v.as_u64())
            .unwrap_or(993) as u16;
        let smtp_host = config
            .get("smtp_host")
            .and_then(|v| v.as_str())
            .ok_or_else(|| Error::Connector("smtp_host is required".to_string()))?
            .to_string();
        let smtp_port = config
            .get("smtp_port")
            .and_then(|v| v.as_u64())
            .unwrap_or(587) as u16;
        let username = config
            .get("username")
            .and_then(|v| v.as_str())
            .ok_or_else(|| Error::Connector("username is required".to_string()))?
            .to_string();
        let password = config
            .get("password")
            .and_then(|v| v.as_str())
            .ok_or_else(|| Error::Connector("password is required".to_string()))?
            .to_string();

        self.imap_host = Some(imap_host.clone());
        self.imap_port = Some(imap_port);
        self.smtp_host = Some(smtp_host.clone());
        self.smtp_port = Some(smtp_port);
        self.username = Some(username.clone());
        self.password = Some(password.clone());
        self.channels = channels.to_vec();
        self.shutdown.store(false, Ordering::SeqCst);

        let connector_id = channels.first().map(|ch| ch.id.clone()).unwrap_or_default();
        self.connector_id = connector_id.clone();

        let has_source = channels
            .iter()
            .any(|ch| ch.channel_type == "source" || ch.channel_type == "both");

        if has_source {
            let shutdown = self.shutdown.clone();
            let source_channels: Vec<ChannelConfig> = channels
                .iter()
                .filter(|ch| ch.channel_type == "source" || ch.channel_type == "both")
                .cloned()
                .collect();

            let ih = imap_host.clone();
            let ip = imap_port;
            let un = username.clone();
            let pw = password.clone();

            let handle = tokio::spawn(async move {
                poll_imap(ih, ip, un, pw, source_channels, event_tx, shutdown).await;
            });

            self.poll_handle = Some(handle);
        }

        info!(
            imap_host = imap_host.as_str(),
            smtp_host = smtp_host.as_str(),
            channels = channels.len(),
            "email connector started"
        );
        Ok(())
    }

    async fn stop(&mut self) -> Result<()> {
        info!("email connector stopping");
        self.shutdown.store(true, Ordering::SeqCst);

        if let Some(handle) = self.poll_handle.take() {
            handle.abort();
            let _ = handle.await;
        }

        self.imap_host = None;
        self.imap_port = None;
        self.smtp_host = None;
        self.smtp_port = None;
        self.username = None;
        self.password = None;
        self.channels.clear();
        Ok(())
    }

    async fn send(&self, message: &SinkMessage) -> Result<()> {
        let smtp_host = self
            .smtp_host
            .as_deref()
            .ok_or_else(|| Error::Connector("email connector not started".to_string()))?;
        let smtp_port = self
            .smtp_port
            .ok_or_else(|| Error::Connector("email connector not started".to_string()))?;
        let username = self
            .username
            .as_deref()
            .ok_or_else(|| Error::Connector("email connector not started".to_string()))?;
        let password = self
            .password
            .as_deref()
            .ok_or_else(|| Error::Connector("email connector not started".to_string()))?;

        let channel = self.channels.iter().find(|ch| ch.id == message.channel_id);

        let to_addr = channel
            .and_then(|ch| ch.config.get("to"))
            .and_then(|v| v.as_str())
            .or_else(|| message.context.get("to").and_then(|v| v.as_str()))
            .ok_or_else(|| {
                Error::Connector(format!(
                    "no 'to' address configured for email channel {}",
                    message.channel_id
                ))
            })?;

        let subject = message
            .context
            .get("subject")
            .and_then(|v| v.as_str())
            .unwrap_or("Message from xpressclaw");

        let body_text = render_template(&message.template, &message.context);

        debug!(to = to_addr, subject = subject, "sending email");

        use lettre::message::header::ContentType;
        use lettre::transport::smtp::authentication::Credentials;
        use lettre::{AsyncSmtpTransport, AsyncTransport, Message, Tokio1Executor};

        let email = Message::builder()
            .from(
                username
                    .parse()
                    .map_err(|e| Error::Connector(format!("invalid from address: {e}")))?,
            )
            .to(to_addr
                .parse()
                .map_err(|e| Error::Connector(format!("invalid to address: {e}")))?)
            .subject(subject)
            .header(ContentType::TEXT_PLAIN)
            .body(body_text)
            .map_err(|e| Error::Connector(format!("failed to build email: {e}")))?;

        let creds = Credentials::new(username.to_string(), password.to_string());

        let mailer = AsyncSmtpTransport::<Tokio1Executor>::starttls_relay(smtp_host)
            .map_err(|e| Error::Connector(format!("failed to create SMTP transport: {e}")))?
            .port(smtp_port)
            .credentials(creds)
            .build();

        mailer
            .send(email)
            .await
            .map_err(|e| Error::Connector(format!("SMTP send failed: {e}")))?;

        info!(
            to = to_addr,
            subject = subject,
            channel_id = message.channel_id.as_str(),
            "email sent"
        );
        Ok(())
    }

    async fn health(&self) -> bool {
        // A basic health check: verify we have credentials configured
        self.imap_host.is_some()
            && self.smtp_host.is_some()
            && self.username.is_some()
            && self.password.is_some()
    }
}

/// Polling loop for IMAP unseen messages.
async fn poll_imap(
    imap_host: String,
    imap_port: u16,
    username: String,
    password: String,
    source_channels: Vec<ChannelConfig>,
    event_tx: mpsc::Sender<ConnectorEvent>,
    shutdown: Arc<AtomicBool>,
) {
    info!("email IMAP poll loop started");

    loop {
        if shutdown.load(Ordering::Relaxed) {
            info!("email IMAP poll loop shutting down");
            break;
        }

        for channel in &source_channels {
            let folder = channel
                .config
                .get("folder")
                .and_then(|v| v.as_str())
                .unwrap_or("INBOX");

            match fetch_unseen_emails(&imap_host, imap_port, &username, &password, folder).await {
                Ok(emails) => {
                    for email_data in emails {
                        let event = ConnectorEvent {
                            connector_id: channel.id.clone(),
                            channel_id: channel.id.clone(),
                            event_type: "email_received".to_string(),
                            payload: email_data,
                        };

                        if let Err(e) = event_tx.send(event).await {
                            error!(error = %e, "failed to send email event");
                            break;
                        }
                    }
                }
                Err(e) => {
                    warn!(
                        error = %e,
                        folder = folder,
                        "IMAP fetch failed"
                    );
                }
            }
        }

        // Poll every 30 seconds
        tokio::time::sleep(std::time::Duration::from_secs(30)).await;
    }
}

/// Connect to IMAP, search for unseen messages, fetch them, and mark as seen.
async fn fetch_unseen_emails(
    host: &str,
    port: u16,
    username: &str,
    password: &str,
    folder: &str,
) -> std::result::Result<Vec<Value>, String> {
    let tls = async_native_tls::TlsConnector::new();

    // Use async-std's TcpStream because async-imap and async-native-tls
    // require futures::AsyncRead/AsyncWrite, not tokio's versions.
    let tcp = async_std::net::TcpStream::connect((host, port))
        .await
        .map_err(|e| format!("TCP connect to {}:{} failed: {}", host, port, e))?;

    let tls_stream = tls
        .connect(host, tcp)
        .await
        .map_err(|e| format!("TLS handshake failed: {}", e))?;

    let client = async_imap::Client::new(tls_stream);

    let mut session = client
        .login(username, password)
        .await
        .map_err(|(e, _)| format!("IMAP login failed: {}", e))?;

    session
        .select(folder)
        .await
        .map_err(|e| format!("IMAP SELECT {} failed: {}", folder, e))?;

    // Search for unseen messages
    let search_result = session
        .search("UNSEEN")
        .await
        .map_err(|e| format!("IMAP SEARCH UNSEEN failed: {}", e))?;

    let mut emails = Vec::new();

    if search_result.is_empty() {
        let _ = session.logout().await;
        return Ok(emails);
    }

    // Build a sequence set from the UIDs
    let uids: Vec<String> = search_result.iter().map(|u| u.to_string()).collect();
    let uid_set = uids.join(",");

    // Fetch message headers and body
    use futures_util::StreamExt;
    let mut messages = session
        .fetch(&uid_set, "(FLAGS ENVELOPE BODY[TEXT])")
        .await
        .map_err(|e| format!("IMAP FETCH failed: {}", e))?;

    while let Some(msg_result) = messages.next().await {
        let msg = match msg_result {
            Ok(m) => m,
            Err(e) => {
                warn!(error = %e, "failed to parse IMAP message");
                continue;
            }
        };

        let envelope = msg.envelope();
        let (from, subject, date) = if let Some(env) = envelope {
            let from = env
                .from
                .as_ref()
                .and_then(|addrs| addrs.first())
                .map(|addr| {
                    let mailbox = addr
                        .mailbox
                        .as_ref()
                        .map(|m| String::from_utf8_lossy(m).to_string())
                        .unwrap_or_default();
                    let host = addr
                        .host
                        .as_ref()
                        .map(|h| String::from_utf8_lossy(h).to_string())
                        .unwrap_or_default();
                    if host.is_empty() {
                        mailbox
                    } else {
                        format!("{}@{}", mailbox, host)
                    }
                })
                .unwrap_or_default();

            let subject = env
                .subject
                .as_ref()
                .map(|s| String::from_utf8_lossy(s).to_string())
                .unwrap_or_default();

            let date = env
                .date
                .as_ref()
                .map(|d| String::from_utf8_lossy(d).to_string())
                .unwrap_or_default();

            (from, subject, date)
        } else {
            (String::new(), String::new(), String::new())
        };

        let body = msg
            .text()
            .map(|b| String::from_utf8_lossy(b).to_string())
            .unwrap_or_default();

        let payload = json!({
            "from": from,
            "subject": subject,
            "body": body,
            "date": date,
        });

        emails.push(payload);

        debug!(
            from = from.as_str(),
            subject = subject.as_str(),
            "fetched unseen email"
        );
    }
    // Drop the stream before issuing more commands
    drop(messages);

    // Mark fetched messages as seen
    if !uid_set.is_empty() {
        let _ = session
            .store(&uid_set, "+FLAGS (\\Seen)")
            .await
            .map_err(|e| warn!(error = %e, "failed to mark messages as seen"));
    }

    let _ = session.logout().await;

    Ok(emails)
}

/// Simple template renderer: replaces `{{key}}` with values from context.
fn render_template(template: &str, context: &Value) -> String {
    let mut result = template.to_string();
    if let Some(obj) = context.as_object() {
        for (key, value) in obj {
            let placeholder = format!("{{{{{}}}}}", key);
            let replacement = match value {
                Value::String(s) => s.clone(),
                other => other.to_string(),
            };
            result = result.replace(&placeholder, &replacement);
        }
    }
    result
}
