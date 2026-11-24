//! SMTP sending with `lettre`.
//!
//! Build the transport from `Config`, send with the per-task timeout and
//! classify the error (transient vs permanent). Requeue with backoff lives
//! in `main.rs` + `retry` + `db`. Attachments arrive already validated from
//! `attachments` (on-disk policy); here they are only assembled.

use lettre::{
    AsyncSmtpTransport, AsyncTransport, Message, Tokio1Executor,
    message::{Attachment, Mailbox, MultiPart, SinglePart, header::ContentType},
    transport::smtp::authentication::Credentials,
};

use crate::attachments::AttachmentFile;
use crate::config::Config;
use crate::contract::EmailContent;

/// Already-classified send failure.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SendError {
    /// `4xx`, timeout, connection/TLS: retriable (requeued by the caller).
    Transient(String),
    /// `5xx`: final, no retry.
    Permanent(String),
    /// The `Message` could not be built (`payload` and `lettre` disagree on
    /// an edge): treated as an invalid header, no retry.
    Header(String),
}

impl SendError {
    pub fn code(&self) -> &'static str {
        match self {
            SendError::Transient(_) => "pilotwave_worker.smtp_transient",
            SendError::Permanent(_) => "pilotwave_worker.smtp_permanent",
            SendError::Header(_) => "pilotwave_worker.header_violation",
        }
    }

    pub fn detail(&self) -> &str {
        match self {
            SendError::Transient(detail)
            | SendError::Permanent(detail)
            | SendError::Header(detail) => detail,
        }
    }
}

/// Build the `Message`. Without attachments: the plain single body; with
/// attachments: `mixed(base, att1, ...)` where `base` is the alternative/plain/
/// html part. Build errors are header defects (not network errors).
pub fn build_message(
    from: &Mailbox,
    content: &EmailContent,
    attachments: &[AttachmentFile],
) -> Result<Message, String> {
    let to: Mailbox = content
        .to
        .parse()
        .map_err(|err| format!("invalid to mailbox: {err}"))?;
    let builder = Message::builder()
        .from(from.clone())
        .to(to)
        .subject(&content.subject);
    if attachments.is_empty() {
        return match (&content.text_body, &content.html_body) {
            (Some(text), Some(html)) => builder.multipart(MultiPart::alternative_plain_html(
                text.clone(),
                html.clone(),
            )),
            (Some(text), None) => builder.singlepart(SinglePart::plain(text.clone())),
            (None, Some(html)) => builder.singlepart(SinglePart::html(html.clone())),
            (None, None) => {
                return Err(
                    "missing body (unreachable: contract::parse_args_kwargs rejects it)".to_owned(),
                );
            }
        }
        .map_err(|err| format!("message build failed: {err}"));
    }

    let mut mixed = match (&content.text_body, &content.html_body) {
        (Some(text), Some(html)) => MultiPart::mixed().multipart(
            MultiPart::alternative_plain_html(text.clone(), html.clone()),
        ),
        (Some(text), None) => MultiPart::mixed().singlepart(SinglePart::plain(text.clone())),
        (None, Some(html)) => MultiPart::mixed().singlepart(SinglePart::html(html.clone())),
        (None, None) => {
            return Err(
                "missing body (unreachable: contract::parse_args_kwargs rejects it)".to_owned(),
            );
        }
    };
    for att in attachments {
        let content_type = ContentType::parse("application/octet-stream")
            .map_err(|err| format!("attachment content type: {err}"))?;
        mixed = mixed.singlepart(
            Attachment::new(att.filename.clone()).body(att.bytes.clone(), content_type),
        );
    }
    builder
        .multipart(mixed)
        .map_err(|err| format!("message build failed: {err}"))
}

fn transport(config: &Config) -> Result<AsyncSmtpTransport<Tokio1Executor>, SendError> {
    // No TLS only with explicit opt-in, validated in `Config::from_env`
    // (`PILOTWAVE_ALLOW_PLAINTEXT_SMTP=true`, dev-only): this
    // `builder_dangerous` is never silent.
    let builder = if config.smtp_use_tls {
        AsyncSmtpTransport::<Tokio1Executor>::starttls_relay(&config.smtp_host)
            .map_err(|err| SendError::Transient(format!("smtp relay: {err}")))?
    } else {
        AsyncSmtpTransport::<Tokio1Executor>::builder_dangerous(&config.smtp_host)
    };
    Ok(builder
        .port(config.smtp_port)
        .credentials(Credentials::new(
            config.smtp_user.clone(),
            config.smtp_password.clone(),
        ))
        .build())
}

/// Send already-validated content with its attachments. The global timeout is
/// `task_timeout`; it also covers assembly (files are already held in
/// policy-bounded memory).
pub async fn send(
    config: &Config,
    content: &EmailContent,
    attachments: &[AttachmentFile],
) -> Result<(), SendError> {
    let mailer = transport(config)?;
    let message = build_message(&config.from, content, attachments).map_err(SendError::Header)?;
    match tokio::time::timeout(config.task_timeout, mailer.send(message)).await {
        Ok(Ok(_)) => Ok(()),
        Ok(Err(err)) if err.is_permanent() => Err(SendError::Permanent(err.to_string())),
        Ok(Err(err)) => Err(SendError::Transient(err.to_string())),
        Err(_) => Err(SendError::Transient(format!(
            "send timeout after {}s",
            config.task_timeout.as_secs()
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mailbox(s: &str) -> Mailbox {
        s.parse().unwrap()
    }

    fn content(text: Option<&str>, html: Option<&str>) -> EmailContent {
        EmailContent {
            to: "team@example.com".to_owned(),
            subject: "Hola".to_owned(),
            text_body: text.map(str::to_owned),
            html_body: html.map(str::to_owned),
            attachment_paths: Vec::new(),
            attachment_sizes: Vec::new(),
        }
    }

    fn attachment(name: &str, bytes: &[u8]) -> AttachmentFile {
        AttachmentFile {
            filename: name.to_owned(),
            bytes: bytes.to_vec(),
        }
    }

    fn formatted(msg: &Message) -> String {
        String::from_utf8(msg.formatted()).unwrap()
    }

    #[test]
    fn bodies_text_html_y_ambos_construyen_su_multipart() {
        let text_only = build_message(
            &mailbox("CRM <crm@example.com>"),
            &content(Some("hola"), None),
            &[],
        )
        .unwrap();
        let raw = formatted(&text_only);
        assert!(raw.contains("Content-Type: text/plain"));
        assert!(raw.contains("To: team@example.com"));
        assert!(raw.contains("Subject: Hola"));

        let html_only = build_message(
            &mailbox("CRM <crm@example.com>"),
            &content(None, Some("<p>x</p>")),
            &[],
        )
        .unwrap();
        assert!(formatted(&html_only).contains("Content-Type: text/html"));

        let both = build_message(
            &mailbox("CRM <crm@example.com>"),
            &content(Some("hola"), Some("<p>hola</p>")),
            &[],
        )
        .unwrap();
        assert!(formatted(&both).contains("multipart/alternative"));
    }

    #[test]
    fn with_attachments_builds_mixed_with_filename() {
        let msg = build_message(
            &mailbox("CRM <crm@example.com>"),
            &content(Some("hola"), None),
            &[attachment("a.pdf", b"datos")],
        )
        .unwrap();
        let raw = formatted(&msg);
        assert!(raw.contains("multipart/mixed"));
        assert!(raw.contains("a.pdf"));
    }

    #[test]
    fn send_error_codes_match_plan() {
        assert_eq!(
            SendError::Transient("x".into()).code(),
            "pilotwave_worker.smtp_transient"
        );
        assert_eq!(
            SendError::Permanent("x".into()).code(),
            "pilotwave_worker.smtp_permanent"
        );
        assert_eq!(
            SendError::Header("x".into()).code(),
            "pilotwave_worker.header_violation"
        );
    }
}
