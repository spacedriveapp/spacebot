//! Email messaging adapter using IMAP polling and SMTP delivery.

use crate::config::EmailConfig;
use crate::messaging::traits::{HistoryMessage, InboundStream, Messaging};
use crate::{InboundMessage, MessageContent, OutboundResponse};

use anyhow::Context as _;
use chrono::{TimeZone as _, Utc};
use lettre::message::header::ContentType;
use lettre::message::{Attachment as EmailAttachment, Mailbox, MultiPart, SinglePart};
use lettre::transport::smtp::authentication::Credentials;
use lettre::{Address, AsyncSmtpTransport, AsyncTransport, Message, Tokio1Executor};
use mailparse::{DispositionType, MailAddr, MailHeaderMap};
use regex::Regex;
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, OnceLock};
use std::time::Duration;
use tokio::sync::{RwLock, mpsc, watch};
use tokio::task::JoinHandle;

const EMAIL_MAX_RETRY_BACKOFF_SECS: u64 = 300;

type ImapSession = imap::Session<native_tls::TlsStream<std::net::TcpStream>>;

#[derive(Clone)]
struct EmailPollConfig {
    imap_host: String,
    imap_port: u16,
    imap_username: String,
    imap_password: String,
    imap_use_tls: bool,
    from_address: String,
    smtp_username: String,
    folders: Vec<String>,
    poll_interval: Duration,
    allowed_senders: Vec<String>,
    max_body_bytes: usize,
}

struct HistoryEntry {
    timestamp: chrono::DateTime<chrono::Utc>,
    message: HistoryMessage,
}

/// Email adapter state.
pub struct EmailAdapter {
    imap_host: String,
    imap_port: u16,
    imap_username: String,
    imap_password: String,
    imap_use_tls: bool,
    smtp_host: String,
    smtp_port: u16,
    smtp_username: String,
    smtp_use_starttls: bool,
    from_address: String,
    from_name: Option<String>,
    folders: Vec<String>,
    poll_interval: Duration,
    allowed_senders: Vec<String>,
    max_body_bytes: usize,
    max_attachment_bytes: usize,
    smtp_transport: AsyncSmtpTransport<Tokio1Executor>,
    shutdown_tx: Arc<RwLock<Option<watch::Sender<bool>>>>,
    poll_task: Arc<RwLock<Option<JoinHandle<()>>>>,
}

impl std::fmt::Debug for EmailAdapter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EmailAdapter")
            .field("imap_host", &self.imap_host)
            .field("imap_port", &self.imap_port)
            .field("imap_username", &"[REDACTED]")
            .field("imap_password", &"[REDACTED]")
            .field("imap_use_tls", &self.imap_use_tls)
            .field("smtp_host", &self.smtp_host)
            .field("smtp_port", &self.smtp_port)
            .field("smtp_username", &"[REDACTED]")
            .field("smtp_use_starttls", &self.smtp_use_starttls)
            .field("from_address", &"[REDACTED]")
            .field("from_name", &self.from_name)
            .field("folders", &self.folders)
            .field("poll_interval", &self.poll_interval)
            .field("allowed_senders", &"[REDACTED]")
            .field("max_body_bytes", &self.max_body_bytes)
            .field("max_attachment_bytes", &self.max_attachment_bytes)
            .finish()
    }
}

impl EmailAdapter {
    pub fn from_config(config: &EmailConfig) -> crate::Result<Self> {
        let folders = config
            .folders
            .iter()
            .map(|folder| folder.trim().to_string())
            .filter(|folder| !folder.is_empty())
            .collect::<Vec<_>>();

        let folders = if folders.is_empty() {
            vec!["INBOX".to_string()]
        } else {
            folders
        };

        let smtp_transport = build_smtp_transport(config)?;

        Ok(Self {
            imap_host: config.imap_host.clone(),
            imap_port: config.imap_port,
            imap_username: config.imap_username.clone(),
            imap_password: config.imap_password.clone(),
            imap_use_tls: config.imap_use_tls,
            smtp_host: config.smtp_host.clone(),
            smtp_port: config.smtp_port,
            smtp_username: config.smtp_username.clone(),
            smtp_use_starttls: config.smtp_use_starttls,
            from_address: config.from_address.clone(),
            from_name: config.from_name.clone(),
            folders,
            poll_interval: Duration::from_secs(config.poll_interval_secs.max(5)),
            allowed_senders: config.allowed_senders.clone(),
            max_body_bytes: config.max_body_bytes.max(1024),
            max_attachment_bytes: config.max_attachment_bytes.max(1024),
            smtp_transport,
            shutdown_tx: Arc::new(RwLock::new(None)),
            poll_task: Arc::new(RwLock::new(None)),
        })
    }

    fn poll_config(&self) -> EmailPollConfig {
        EmailPollConfig {
            imap_host: self.imap_host.clone(),
            imap_port: self.imap_port,
            imap_username: self.imap_username.clone(),
            imap_password: self.imap_password.clone(),
            imap_use_tls: self.imap_use_tls,
            from_address: self.from_address.clone(),
            smtp_username: self.smtp_username.clone(),
            folders: self.folders.clone(),
            poll_interval: self.poll_interval,
            allowed_senders: self.allowed_senders.clone(),
            max_body_bytes: self.max_body_bytes,
        }
    }

    fn sender_mailbox(&self) -> crate::Result<Mailbox> {
        let from_address: Address = self
            .from_address
            .parse()
            .with_context(|| format!("invalid email from_address '{}'", self.from_address))?;
        Ok(Mailbox::new(self.from_name.clone(), from_address))
    }

    async fn send_email(
        &self,
        recipient: &str,
        subject: &str,
        body: String,
        in_reply_to: Option<String>,
        references: Vec<String>,
        attachment: Option<(String, Vec<u8>, String)>,
    ) -> crate::Result<()> {
        let recipient_mailbox = parse_mailbox(recipient)
            .with_context(|| format!("invalid recipient address '{recipient}'"))?;

        let mut builder = Message::builder()
            .from(self.sender_mailbox()?)
            .to(recipient_mailbox)
            .subject(subject.to_string());

        if let Some(in_reply_to) = in_reply_to {
            let in_reply_to = format_message_id_for_header(&in_reply_to);
            if !in_reply_to.is_empty() {
                builder = builder.in_reply_to(in_reply_to);
            }
        }

        for reference in references {
            let reference = format_message_id_for_header(&reference);
            if !reference.is_empty() {
                builder = builder.references(reference);
            }
        }

        let message = if let Some((filename, data, mime_type)) = attachment {
            if data.len() > self.max_attachment_bytes {
                return Err(anyhow::anyhow!(
                    "attachment '{filename}' exceeds max_attachment_bytes ({} > {})",
                    data.len(),
                    self.max_attachment_bytes
                )
                .into());
            }

            let content_type = ContentType::parse(&mime_type).unwrap_or(ContentType::TEXT_PLAIN);
            let attachment = EmailAttachment::new(filename).body(data, content_type);
            let multipart = MultiPart::mixed()
                .singlepart(SinglePart::plain(body))
                .singlepart(attachment);
            builder
                .multipart(multipart)
                .context("failed to build multipart email")?
        } else {
            builder.body(body).context("failed to build email body")?
        };

        self.smtp_transport
            .send(message)
            .await
            .context("failed to send email")?;

        Ok(())
    }
}

impl Messaging for EmailAdapter {
    fn name(&self) -> &str {
        "email"
    }

    async fn start(&self) -> crate::Result<InboundStream> {
        if self.poll_task.read().await.is_some() {
            return Err(anyhow::anyhow!("email adapter already started").into());
        }

        let (inbound_tx, inbound_rx) = mpsc::channel(256);
        let (shutdown_tx, mut shutdown_rx) = watch::channel(false);

        *self.shutdown_tx.write().await = Some(shutdown_tx);

        let poll_config = self.poll_config();

        let poll_task = tokio::spawn(async move {
            let mut retry_backoff = Duration::from_secs(5);

            loop {
                if *shutdown_rx.borrow() {
                    break;
                }

                let config = poll_config.clone();
                let poll_result =
                    tokio::task::spawn_blocking(move || poll_inbox_once(&config)).await;

                let mut had_error = false;

                match poll_result {
                    Ok(Ok(messages)) => {
                        retry_backoff = Duration::from_secs(5);
                        for message in messages {
                            if inbound_tx.send(message).await.is_err() {
                                tracing::warn!(
                                    "email inbound channel closed, stopping adapter loop"
                                );
                                return;
                            }
                        }
                    }
                    Ok(Err(error)) => {
                        had_error = true;
                        tracing::warn!(%error, "email poll cycle failed");
                    }
                    Err(error) => {
                        had_error = true;
                        tracing::warn!(%error, "email poll task panicked");
                    }
                }

                let sleep_duration = if had_error {
                    let current = retry_backoff;
                    retry_backoff =
                        (retry_backoff * 2).min(Duration::from_secs(EMAIL_MAX_RETRY_BACKOFF_SECS));
                    current
                } else {
                    poll_config.poll_interval
                };

                tokio::select! {
                    _ = shutdown_rx.changed() => {
                        if *shutdown_rx.borrow() {
                            break;
                        }
                    }
                    _ = tokio::time::sleep(sleep_duration) => {}
                }
            }

            tracing::info!("email adapter loop stopped");
        });

        *self.poll_task.write().await = Some(poll_task);

        let stream = tokio_stream::wrappers::ReceiverStream::new(inbound_rx);
        Ok(Box::pin(stream))
    }

    async fn respond(
        &self,
        message: &InboundMessage,
        response: OutboundResponse,
    ) -> crate::Result<()> {
        let mut context = reply_context_from_message(message)?;

        match response {
            OutboundResponse::Text(text) => {
                self.send_email(
                    &context.recipient,
                    &context.subject,
                    text,
                    context.in_reply_to,
                    context.references,
                    None,
                )
                .await?;
            }
            OutboundResponse::RichMessage { text, .. } => {
                self.send_email(
                    &context.recipient,
                    &context.subject,
                    text,
                    context.in_reply_to,
                    context.references,
                    None,
                )
                .await?;
            }
            OutboundResponse::ThreadReply { thread_name, text } => {
                if !thread_name.trim().is_empty() {
                    context.subject = normalize_reply_subject(&thread_name);
                }
                self.send_email(
                    &context.recipient,
                    &context.subject,
                    text,
                    context.in_reply_to,
                    context.references,
                    None,
                )
                .await?;
            }
            OutboundResponse::File {
                filename,
                data,
                mime_type,
                caption,
            } => {
                let mut body = caption.unwrap_or_else(|| format!("Attached file: {filename}"));
                if body.trim().is_empty() {
                    body = format!("Attached file: {filename}");
                }

                self.send_email(
                    &context.recipient,
                    &context.subject,
                    body,
                    context.in_reply_to,
                    context.references,
                    Some((filename, data, mime_type)),
                )
                .await?;
            }
            OutboundResponse::Reaction(_)
            | OutboundResponse::RemoveReaction(_)
            | OutboundResponse::Status(_) => {}
            OutboundResponse::Ephemeral { text, .. } => {
                self.send_email(
                    &context.recipient,
                    &context.subject,
                    text,
                    context.in_reply_to,
                    context.references,
                    None,
                )
                .await?;
            }
            OutboundResponse::ScheduledMessage { text, post_at } => {
                tracing::warn!(
                    post_at,
                    recipient = %context.recipient,
                    subject = %context.subject,
                    "email adapter does not support scheduled delivery; sending immediately"
                );
                self.send_email(
                    &context.recipient,
                    &context.subject,
                    text,
                    context.in_reply_to,
                    context.references,
                    None,
                )
                .await?;
            }
            OutboundResponse::StreamStart
            | OutboundResponse::StreamChunk(_)
            | OutboundResponse::StreamEnd => {}
        }

        Ok(())
    }

    async fn broadcast(&self, target: &str, response: OutboundResponse) -> crate::Result<()> {
        let recipient = normalize_email_target(target)
            .ok_or_else(|| anyhow::anyhow!("invalid email target '{target}'"))?;

        match response {
            OutboundResponse::Text(text) => {
                self.send_email(&recipient, "Spacebot message", text, None, Vec::new(), None)
                    .await?;
            }
            OutboundResponse::RichMessage { text, .. } => {
                self.send_email(&recipient, "Spacebot message", text, None, Vec::new(), None)
                    .await?;
            }
            OutboundResponse::File {
                filename,
                data,
                mime_type,
                caption,
            } => {
                let body = caption.unwrap_or_else(|| format!("Attached file: {filename}"));
                self.send_email(
                    &recipient,
                    "Spacebot message",
                    body,
                    None,
                    Vec::new(),
                    Some((filename, data, mime_type)),
                )
                .await?;
            }
            OutboundResponse::ThreadReply { text, .. }
            | OutboundResponse::Ephemeral { text, .. } => {
                self.send_email(&recipient, "Spacebot message", text, None, Vec::new(), None)
                    .await?;
            }
            OutboundResponse::ScheduledMessage { text, post_at } => {
                tracing::warn!(
                    post_at,
                    recipient = %recipient,
                    "email adapter does not support scheduled delivery; sending immediately"
                );
                self.send_email(&recipient, "Spacebot message", text, None, Vec::new(), None)
                    .await?;
            }
            OutboundResponse::Reaction(_)
            | OutboundResponse::RemoveReaction(_)
            | OutboundResponse::Status(_)
            | OutboundResponse::StreamStart
            | OutboundResponse::StreamChunk(_)
            | OutboundResponse::StreamEnd => {}
        }

        Ok(())
    }

    async fn fetch_history(
        &self,
        message: &InboundMessage,
        limit: usize,
    ) -> crate::Result<Vec<HistoryMessage>> {
        if limit == 0 {
            return Ok(Vec::new());
        }

        let references = message
            .metadata
            .get("email_references")
            .and_then(json_value_to_string)
            .map(|value| extract_message_ids(&value))
            .unwrap_or_default();

        let in_reply_to = message
            .metadata
            .get("email_in_reply_to")
            .and_then(json_value_to_string)
            .and_then(|value| extract_message_ids(&value).into_iter().next());

        let mut message_ids = references;
        if let Some(in_reply_to) = in_reply_to
            && !message_ids.contains(&in_reply_to)
        {
            message_ids.push(in_reply_to);
        }

        let current_message_id = message
            .metadata
            .get("email_message_id")
            .and_then(json_value_to_string)
            .map(|value| normalize_message_id(&value));

        message_ids.retain(|message_id| {
            current_message_id
                .as_ref()
                .is_none_or(|current| current != message_id)
        });

        if message_ids.is_empty() {
            return Ok(Vec::new());
        }

        let poll_config = self.poll_config();

        let history = tokio::task::spawn_blocking(move || {
            fetch_history_from_imap(&poll_config, message_ids, limit)
        })
        .await
        .context("email history task failed")??;

        Ok(history)
    }

    async fn health_check(&self) -> crate::Result<()> {
        let poll_config = self.poll_config();
        tokio::task::spawn_blocking(move || {
            let mut session = open_imap_session(&poll_config)?;
            let folder = poll_config
                .folders
                .first()
                .cloned()
                .unwrap_or_else(|| "INBOX".to_string());
            session
                .select(&folder)
                .with_context(|| format!("failed to select IMAP folder '{folder}'"))?;
            session.logout().ok();
            anyhow::Ok(())
        })
        .await
        .context("email IMAP health check task failed")??;

        let smtp_ok = self
            .smtp_transport
            .test_connection()
            .await
            .context("SMTP health check failed")?;
        if !smtp_ok {
            return Err(anyhow::anyhow!("SMTP server rejected test connection").into());
        }

        Ok(())
    }

    async fn shutdown(&self) -> crate::Result<()> {
        if let Some(shutdown_tx) = self.shutdown_tx.write().await.take() {
            shutdown_tx.send(true).ok();
        }

        if let Some(poll_task) = self.poll_task.write().await.take()
            && let Err(error) = poll_task.await
        {
            tracing::warn!(%error, "email poll task join failed during shutdown");
        }

        self.smtp_transport.shutdown().await;

        tracing::info!("email adapter shut down");
        Ok(())
    }
}

fn build_smtp_transport(config: &EmailConfig) -> crate::Result<AsyncSmtpTransport<Tokio1Executor>> {
    let builder = if config.smtp_use_starttls {
        AsyncSmtpTransport::<Tokio1Executor>::starttls_relay(&config.smtp_host)
            .with_context(|| format!("invalid SMTP host '{}'", config.smtp_host))?
    } else {
        AsyncSmtpTransport::<Tokio1Executor>::relay(&config.smtp_host)
            .with_context(|| format!("invalid SMTP host '{}'", config.smtp_host))?
    };

    Ok(builder
        .port(config.smtp_port)
        .credentials(Credentials::new(
            config.smtp_username.clone(),
            config.smtp_password.clone(),
        ))
        .build())
}

fn poll_inbox_once(config: &EmailPollConfig) -> anyhow::Result<Vec<InboundMessage>> {
    let mut session = open_imap_session(config)?;
    let mut inbound_messages = Vec::new();

    for folder in &config.folders {
        if let Err(error) = session.select(folder) {
            tracing::warn!(folder, %error, "failed to select IMAP folder");
            continue;
        }

        let message_uids = session
            .uid_search("UNSEEN")
            .with_context(|| format!("failed to search unseen messages in folder '{folder}'"))?;

        for uid in message_uids {
            let uid_sequence = uid.to_string();

            let fetches = match session.uid_fetch(&uid_sequence, "(UID RFC822)") {
                Ok(fetches) => fetches,
                Err(error) => {
                    tracing::warn!(folder, uid, %error, "failed to fetch unseen email");
                    continue;
                }
            };

            let mut should_mark_seen = !fetches.is_empty();

            for fetch in &fetches {
                let current_uid = fetch.uid.unwrap_or(uid);
                let Some(raw_email) = fetch.body() else {
                    should_mark_seen = false;
                    tracing::warn!(
                        folder,
                        uid = current_uid,
                        "email fetch body missing; leaving message unseen for retry"
                    );
                    continue;
                };

                match parse_inbound_email(raw_email, folder, current_uid, config) {
                    Ok(Some(inbound_message)) => inbound_messages.push(inbound_message),
                    Ok(None) => {}
                    Err(error) => {
                        should_mark_seen = false;
                        tracing::warn!(folder, uid = current_uid, %error, "failed to parse inbound email");
                    }
                }
            }

            if should_mark_seen {
                if let Err(error) = session.uid_store(&uid_sequence, "+FLAGS (\\Seen)") {
                    tracing::warn!(folder, uid, %error, "failed to mark email as seen");
                }
            } else {
                tracing::debug!(folder, uid, "leaving email unseen for retry");
            }
        }
    }

    session.logout().ok();

    Ok(inbound_messages)
}

fn open_imap_session(config: &EmailPollConfig) -> anyhow::Result<ImapSession> {
    let tls = native_tls::TlsConnector::builder()
        .build()
        .context("failed to build TLS connector for IMAP")?;

    let client = if config.imap_use_tls {
        imap::connect(
            (config.imap_host.as_str(), config.imap_port),
            config.imap_host.as_str(),
            &tls,
        )
        .with_context(|| {
            format!(
                "failed to connect to IMAP server '{}:{}'",
                config.imap_host, config.imap_port
            )
        })?
    } else {
        imap::connect_starttls(
            (config.imap_host.as_str(), config.imap_port),
            config.imap_host.as_str(),
            &tls,
        )
        .with_context(|| {
            format!(
                "failed to connect to IMAP server '{}:{}' with STARTTLS",
                config.imap_host, config.imap_port
            )
        })?
    };

    let session = client
        .login(config.imap_username.as_str(), config.imap_password.as_str())
        .map_err(|error| anyhow::anyhow!(error.0))
        .context("failed to authenticate to IMAP server")?;

    Ok(session)
}

fn parse_inbound_email(
    raw_email: &[u8],
    folder: &str,
    uid: u32,
    config: &EmailPollConfig,
) -> anyhow::Result<Option<InboundMessage>> {
    let parsed = mailparse::parse_mail(raw_email).context("failed to parse MIME email")?;
    let headers = parsed.headers.as_slice();

    if is_auto_generated_email(headers) {
        return Ok(None);
    }

    let from_header = headers.get_first_value("From").unwrap_or_default();
    let Some((sender_email, sender_name)) = parse_primary_mailbox(&from_header) else {
        return Ok(None);
    };

    if is_own_sender(&sender_email, config) {
        return Ok(None);
    }

    if !is_allowed_sender(&sender_email, &config.allowed_senders) {
        return Ok(None);
    }

    let reply_to_email = headers
        .get_first_value("Reply-To")
        .and_then(|value| parse_primary_mailbox(&value).map(|(address, _)| address))
        .unwrap_or_else(|| sender_email.clone());

    let to_header = headers.get_first_value("To");
    let subject = headers
        .get_first_value("Subject")
        .unwrap_or_else(|| "(no subject)".to_string());

    let message_id = headers
        .get_first_value("Message-ID")
        .map(|value| normalize_message_id(&value))
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| format!("generated-{}-{}", uid, uuid::Uuid::new_v4()));

    let in_reply_to = headers
        .get_first_value("In-Reply-To")
        .and_then(|value| extract_message_ids(&value).into_iter().next());

    let references = headers
        .get_first_value("References")
        .map(|value| extract_message_ids(&value))
        .unwrap_or_default();

    let thread_key = derive_thread_key(
        &references,
        in_reply_to.as_deref(),
        Some(message_id.as_str()),
        &subject,
        &sender_email,
    );

    let account_key = sanitize_account_key(&config.from_address);
    let conversation_id = format!("email:{account_key}:{thread_key}");

    let (mut body_text, attachment_names) =
        extract_text_and_attachments(&parsed, config.max_body_bytes);
    if !attachment_names.is_empty() {
        body_text.push_str("\n\nAttachments: ");
        body_text.push_str(&attachment_names.join(", "));
    }

    let timestamp = headers
        .get_first_value("Date")
        .and_then(|value| mailparse::dateparse(&value).ok())
        .and_then(|timestamp| Utc.timestamp_opt(timestamp, 0).single())
        .unwrap_or_else(Utc::now);

    let mut metadata = HashMap::new();
    metadata.insert(
        "email_from".into(),
        serde_json::Value::String(sender_email.clone()),
    );
    metadata.insert(
        "email_reply_to".into(),
        serde_json::Value::String(reply_to_email),
    );
    if let Some(to_header) = to_header {
        metadata.insert("email_to".into(), serde_json::Value::String(to_header));
    }
    metadata.insert(
        "email_subject".into(),
        serde_json::Value::String(subject.clone()),
    );
    metadata.insert(
        "email_message_id".into(),
        serde_json::Value::String(message_id.clone()),
    );
    if let Some(in_reply_to) = in_reply_to {
        metadata.insert(
            "email_in_reply_to".into(),
            serde_json::Value::String(in_reply_to),
        );
    }
    if !references.is_empty() {
        metadata.insert(
            "email_references".into(),
            serde_json::Value::String(references.join(" ")),
        );
    }
    metadata.insert(
        "email_folder".into(),
        serde_json::Value::String(folder.to_string()),
    );
    metadata.insert(
        "email_uid".into(),
        serde_json::Value::Number(serde_json::Number::from(uid)),
    );
    metadata.insert(
        "email_thread_key".into(),
        serde_json::Value::String(thread_key),
    );
    metadata.insert(
        "sender_display_name".into(),
        serde_json::Value::String(sender_name.clone().unwrap_or_else(|| sender_email.clone())),
    );

    let formatted_author = sender_name.map_or_else(
        || sender_email.clone(),
        |name| format!("{name} <{sender_email}>"),
    );

    Ok(Some(InboundMessage {
        id: message_id,
        source: "email".into(),
        conversation_id,
        sender_id: sender_email,
        agent_id: None,
        content: MessageContent::Text(body_text),
        timestamp,
        metadata,
        formatted_author: Some(formatted_author),
    }))
}

fn reply_context_from_message(message: &InboundMessage) -> anyhow::Result<EmailReplyContext> {
    let recipient = message
        .metadata
        .get("email_reply_to")
        .and_then(json_value_to_string)
        .or_else(|| {
            message
                .metadata
                .get("email_from")
                .and_then(json_value_to_string)
        })
        .context("missing recipient metadata for email reply")?;

    let subject = message
        .metadata
        .get("email_subject")
        .and_then(json_value_to_string)
        .map(|value| normalize_reply_subject(&value))
        .unwrap_or_else(|| "Re: Spacebot reply".to_string());

    let in_reply_to = message
        .metadata
        .get("email_message_id")
        .and_then(json_value_to_string)
        .map(|value| normalize_message_id(&value))
        .filter(|value| !value.is_empty());

    let mut references = message
        .metadata
        .get("email_references")
        .and_then(json_value_to_string)
        .map(|value| extract_message_ids(&value))
        .unwrap_or_default();

    if let Some(in_reply_to) = &in_reply_to
        && !references.contains(in_reply_to)
    {
        references.push(in_reply_to.clone());
    }

    Ok(EmailReplyContext {
        recipient,
        subject,
        in_reply_to,
        references,
    })
}

fn fetch_history_from_imap(
    config: &EmailPollConfig,
    message_ids: Vec<String>,
    limit: usize,
) -> anyhow::Result<Vec<HistoryMessage>> {
    let mut session = open_imap_session(config)?;
    let mut seen_message_ids = HashSet::new();
    let mut entries = Vec::new();

    for folder in &config.folders {
        if entries.len() >= limit {
            break;
        }

        if let Err(error) = session.select(folder) {
            tracing::warn!(folder, %error, "failed to select IMAP folder for history backfill");
            continue;
        }

        for message_id in &message_ids {
            if entries.len() >= limit {
                break;
            }

            let Some(criterion) = build_message_id_search_criterion(message_id) else {
                tracing::debug!(
                    message_id,
                    "skipping unsafe message id for IMAP history search"
                );
                continue;
            };

            let uids = match session.uid_search(&criterion) {
                Ok(uids) => uids,
                Err(error) => {
                    tracing::warn!(folder, message_id, %error, "failed IMAP history search");
                    continue;
                }
            };

            for uid in uids {
                if entries.len() >= limit {
                    break;
                }

                let fetches = match session.uid_fetch(uid.to_string(), "(UID RFC822)") {
                    Ok(fetches) => fetches,
                    Err(error) => {
                        tracing::warn!(folder, uid, %error, "failed IMAP history fetch");
                        continue;
                    }
                };

                for fetch in &fetches {
                    let Some(raw_email) = fetch.body() else {
                        continue;
                    };

                    let parsed = match mailparse::parse_mail(raw_email) {
                        Ok(parsed) => parsed,
                        Err(error) => {
                            tracing::warn!(folder, uid, %error, "failed to parse history email MIME");
                            continue;
                        }
                    };

                    let headers = parsed.headers.as_slice();
                    let normalized_message_id = headers
                        .get_first_value("Message-ID")
                        .map(|value| normalize_message_id(&value))
                        .filter(|value| !value.is_empty());

                    let Some(normalized_message_id) = normalized_message_id else {
                        continue;
                    };

                    if !seen_message_ids.insert(normalized_message_id) {
                        continue;
                    }

                    let from_header = headers.get_first_value("From").unwrap_or_default();
                    let (sender_email, sender_name) = parse_primary_mailbox(&from_header)
                        .unwrap_or_else(|| (from_header.clone(), None));

                    let is_bot = sender_email.eq_ignore_ascii_case(&config.from_address)
                        || sender_email.eq_ignore_ascii_case(&config.smtp_username);

                    let author = sender_name.unwrap_or(sender_email);
                    let (body, _) = extract_text_and_attachments(&parsed, config.max_body_bytes);

                    let timestamp = headers
                        .get_first_value("Date")
                        .and_then(|value| mailparse::dateparse(&value).ok())
                        .and_then(|timestamp| Utc.timestamp_opt(timestamp, 0).single())
                        .unwrap_or_else(Utc::now);

                    entries.push(HistoryEntry {
                        timestamp,
                        message: HistoryMessage {
                            author,
                            content: body,
                            is_bot,
                        },
                    });
                }
            }
        }
    }

    session.logout().ok();

    entries.sort_by_key(|entry| entry.timestamp);
    entries.truncate(limit);

    Ok(entries.into_iter().map(|entry| entry.message).collect())
}

fn is_auto_generated_email(headers: &[mailparse::MailHeader<'_>]) -> bool {
    let auto_submitted = headers
        .get_first_value("Auto-Submitted")
        .map(|value| value.trim().to_ascii_lowercase())
        .unwrap_or_default();
    if !auto_submitted.is_empty() && auto_submitted != "no" {
        return true;
    }

    let precedence = headers
        .get_first_value("Precedence")
        .map(|value| value.trim().to_ascii_lowercase())
        .unwrap_or_default();
    if matches!(precedence.as_str(), "bulk" | "junk" | "list" | "auto_reply") {
        return true;
    }

    headers.get_first_value("X-Autoreply").is_some()
        || headers.get_first_value("X-Autorespond").is_some()
}

fn is_own_sender(sender_email: &str, config: &EmailPollConfig) -> bool {
    sender_email.eq_ignore_ascii_case(&config.from_address)
        || sender_email.eq_ignore_ascii_case(&config.imap_username)
        || sender_email.eq_ignore_ascii_case(&config.smtp_username)
}

fn is_allowed_sender(sender_email: &str, allowed_senders: &[String]) -> bool {
    if allowed_senders.is_empty() {
        return true;
    }

    let sender_email = sender_email.trim().to_ascii_lowercase();

    allowed_senders.iter().any(|rule| {
        let rule = rule.trim().to_ascii_lowercase();
        if rule.is_empty() {
            return false;
        }

        if rule.starts_with('@') {
            return sender_email.ends_with(&rule);
        }

        if rule.contains('@') {
            return sender_email == rule;
        }

        sender_email.ends_with(&format!("@{rule}"))
    })
}

fn parse_primary_mailbox(value: &str) -> Option<(String, Option<String>)> {
    let addresses = mailparse::addrparse(value).ok()?.into_inner();
    for address in addresses {
        match address {
            MailAddr::Single(single) => {
                return Some((single.addr, single.display_name));
            }
            MailAddr::Group(group) => {
                if let Some(single) = group.addrs.into_iter().next() {
                    return Some((single.addr, single.display_name));
                }
            }
        }
    }
    None
}

fn parse_mailbox(value: &str) -> anyhow::Result<Mailbox> {
    if let Ok(mailbox) = value.parse::<Mailbox>() {
        return Ok(mailbox);
    }

    let (address, display_name) = parse_primary_mailbox(value)
        .with_context(|| format!("failed to parse email address '{value}'"))?;
    let address: Address = address
        .parse()
        .with_context(|| format!("invalid email address '{address}'"))?;
    Ok(Mailbox::new(display_name, address))
}

fn extract_text_and_attachments(
    parsed: &mailparse::ParsedMail<'_>,
    max_body_bytes: usize,
) -> (String, Vec<String>) {
    let mut plain_text_parts = Vec::new();
    let mut html_parts = Vec::new();
    let mut attachment_names = Vec::new();

    collect_parts(
        parsed,
        &mut plain_text_parts,
        &mut html_parts,
        &mut attachment_names,
    );

    let mut body_text = if !plain_text_parts.is_empty() {
        plain_text_parts.join("\n\n")
    } else if !html_parts.is_empty() {
        html_to_text(&html_parts.join("\n\n"))
    } else {
        parsed.get_body().unwrap_or_default()
    };

    body_text = body_text.replace("\r\n", "\n").trim().to_string();
    if body_text.is_empty() {
        body_text = "(No message body)".to_string();
    }

    if body_text.len() > max_body_bytes {
        body_text = format!(
            "{}\n\n[Message truncated due to size limit]",
            truncate_to_bytes(&body_text, max_body_bytes)
        );
    }

    attachment_names.sort();
    attachment_names.dedup();

    (body_text, attachment_names)
}

fn collect_parts(
    part: &mailparse::ParsedMail<'_>,
    plain_text_parts: &mut Vec<String>,
    html_parts: &mut Vec<String>,
    attachment_names: &mut Vec<String>,
) {
    if part.subparts.is_empty() {
        let disposition = part.get_content_disposition();
        let filename = disposition
            .params
            .get("filename")
            .cloned()
            .or_else(|| part.ctype.params.get("name").cloned());
        let is_attachment =
            matches!(disposition.disposition, DispositionType::Attachment) || filename.is_some();

        if let Some(filename) = filename {
            attachment_names.push(filename);
        }

        if is_attachment {
            return;
        }

        let mime_type = part.ctype.mimetype.to_ascii_lowercase();
        if mime_type.starts_with("text/plain") {
            if let Ok(body) = part.get_body()
                && !body.trim().is_empty()
            {
                plain_text_parts.push(body);
            }
        } else if mime_type.starts_with("text/html")
            && let Ok(body) = part.get_body()
            && !body.trim().is_empty()
        {
            html_parts.push(body);
        }
        return;
    }

    for subpart in &part.subparts {
        collect_parts(subpart, plain_text_parts, html_parts, attachment_names);
    }
}

fn html_to_text(html: &str) -> String {
    let without_tags = html_tag_regex().replace_all(html, " ");
    let decoded = without_tags
        .replace("&nbsp;", " ")
        .replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'");

    decoded.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn html_tag_regex() -> &'static Regex {
    static REGEX: OnceLock<Regex> = OnceLock::new();
    REGEX.get_or_init(|| Regex::new(r"(?is)<[^>]+>").expect("valid HTML tag regex"))
}

fn normalize_reply_subject(subject: &str) -> String {
    let subject = subject.trim();
    if subject.is_empty() {
        return "Re: Spacebot reply".to_string();
    }

    if subject.to_ascii_lowercase().starts_with("re:") {
        subject.to_string()
    } else {
        format!("Re: {subject}")
    }
}

fn extract_message_ids(value: &str) -> Vec<String> {
    mailparse::msgidparse(value)
        .map(|ids| {
            ids.iter()
                .map(|id| normalize_message_id(id.as_str()))
                .filter(|id| !id.is_empty())
                .collect::<Vec<String>>()
        })
        .unwrap_or_default()
}

fn normalize_message_id(value: &str) -> String {
    value
        .trim()
        .trim_start_matches('<')
        .trim_end_matches('>')
        .trim()
        .to_string()
}

fn format_message_id_for_header(message_id: &str) -> String {
    let message_id = normalize_message_id(message_id);
    if message_id.is_empty() {
        String::new()
    } else {
        format!("<{message_id}>")
    }
}

fn build_message_id_search_criterion(message_id: &str) -> Option<String> {
    let search_id = format_message_id_for_header(message_id);
    if search_id.is_empty()
        || search_id
            .chars()
            .any(|character| character == '\r' || character == '\n')
    {
        return None;
    }

    let escaped = search_id.replace('\\', "\\\\").replace('"', "\\\"");
    Some(format!("HEADER Message-ID \"{escaped}\""))
}

fn derive_thread_key(
    references: &[String],
    in_reply_to: Option<&str>,
    message_id: Option<&str>,
    subject: &str,
    sender_email: &str,
) -> String {
    let seed = references
        .first()
        .cloned()
        .or_else(|| in_reply_to.map(normalize_message_id))
        .or_else(|| message_id.map(normalize_message_id))
        .unwrap_or_else(|| {
            format!(
                "{}:{}",
                subject.trim().to_ascii_lowercase(),
                sender_email.trim().to_ascii_lowercase()
            )
        });

    let mut hasher = Sha256::new();
    hasher.update(seed.as_bytes());
    let digest = hasher.finalize();
    hex::encode(digest)[..24].to_string()
}

fn sanitize_account_key(value: &str) -> String {
    let mut result = String::new();
    for character in value.trim().to_ascii_lowercase().chars() {
        if character.is_ascii_alphanumeric() {
            result.push(character);
        } else {
            result.push('_');
        }
    }

    let result = result.trim_matches('_').to_string();
    if result.is_empty() {
        "default".to_string()
    } else {
        result
    }
}

fn normalize_email_target(value: &str) -> Option<String> {
    let value = value.trim();
    if value.is_empty() {
        return None;
    }

    if let Some((address, _)) = parse_primary_mailbox(value) {
        return Some(address);
    }

    let value = value.strip_prefix("email:").unwrap_or(value).trim();
    if value.contains('@') && !value.contains(char::is_whitespace) {
        Some(value.to_string())
    } else {
        None
    }
}

fn truncate_to_bytes(value: &str, max_bytes: usize) -> String {
    if value.len() <= max_bytes {
        return value.to_string();
    }

    let mut cutoff = max_bytes;
    while cutoff > 0 && !value.is_char_boundary(cutoff) {
        cutoff -= 1;
    }

    let mut truncated = value[..cutoff].to_string();
    truncated.push_str("...");
    truncated
}

fn json_value_to_string(value: &serde_json::Value) -> Option<String> {
    if let Some(text) = value.as_str() {
        return Some(text.to_string());
    }
    if let Some(number) = value.as_i64() {
        return Some(number.to_string());
    }
    if let Some(number) = value.as_u64() {
        return Some(number.to_string());
    }
    None
}

struct EmailReplyContext {
    recipient: String,
    subject: String,
    in_reply_to: Option<String>,
    references: Vec<String>,
}

#[cfg(test)]
mod tests {
    use super::{
        derive_thread_key, extract_message_ids, normalize_email_target, normalize_reply_subject,
        parse_primary_mailbox,
    };

    #[test]
    fn parse_primary_mailbox_parses_display_name() {
        let parsed = parse_primary_mailbox("Alice Example <alice@example.com>");
        assert_eq!(
            parsed,
            Some((
                "alice@example.com".to_string(),
                Some("Alice Example".to_string())
            ))
        );
    }

    #[test]
    fn extract_message_ids_strips_angle_brackets() {
        let ids = extract_message_ids("<root@example.com> <child@example.com>");
        assert_eq!(ids, vec!["root@example.com", "child@example.com"]);
    }

    #[test]
    fn normalize_email_target_accepts_prefixed_target() {
        assert_eq!(
            normalize_email_target("email:alice@example.com"),
            Some("alice@example.com".to_string())
        );
    }

    #[test]
    fn normalize_reply_subject_preserves_existing_prefix() {
        assert_eq!(
            normalize_reply_subject("Re: Existing thread"),
            "Re: Existing thread"
        );
        assert_eq!(
            normalize_reply_subject("Existing thread"),
            "Re: Existing thread"
        );
    }

    #[test]
    fn derive_thread_key_prefers_root_reference() {
        let from_references = derive_thread_key(
            &[
                "root@example.com".to_string(),
                "child@example.com".to_string(),
            ],
            Some("reply@example.com"),
            Some("current@example.com"),
            "Subject",
            "sender@example.com",
        );
        let from_root_only = derive_thread_key(
            &["root@example.com".to_string()],
            None,
            None,
            "Different subject",
            "other@example.com",
        );

        assert_eq!(from_references, from_root_only);
    }
}
