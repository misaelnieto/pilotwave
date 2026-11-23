//! Rust side of the boundary contract (see ../../contracts/CONTRACT.md).
//!
//! These constants are the single source of truth for the worker. A change
//! here must be accompanied by a bump in CONTRACT.md and the Python
//! round-trip test.

// Several constants describe the full worker (claim loop, finalization,
// heartbeat) which is ported after this skeleton is green. Keep them visible
// now so the contract is executable documentation.
#![allow(dead_code)]

use serde_json::Value;

/// Table owned by `django-tasks-db==0.13.0` (AppConfig label, not module name).
pub const QUEUE_TABLE: &str = "django_tasks_database_dbtaskresult";
/// Table owned by Pilotwave.
pub const HEARTBEAT_TABLE: &str = "pilotwave_heartbeat";

pub const STATUS_READY: &str = "READY";
pub const STATUS_RUNNING: &str = "RUNNING";
pub const STATUS_SUCCESSFUL: &str = "SUCCESSFUL";
pub const STATUS_FAILED: &str = "FAILED";

/// `run_after` value meaning "not deferred" (contract §Queue row).
pub const RUN_AFTER_SENTINEL: &str = "9999-01-01T00:00:00Z";
pub const HEARTBEAT_KEY: &str = "pilotwave_worker";

pub const MAX_FILE_BYTES: u64 = 6 * 1024 * 1024;
pub const MAX_TOTAL_BYTES: u64 = 10 * 1024 * 1024;
pub const MAX_ATTACHMENTS: usize = 10;
pub const MAX_BODY_BYTES: usize = 1024 * 1024;
pub const MAX_ARGS_JSON_BYTES: usize = 2 * 1024 * 1024;

pub const FAILURE_HEADER: &str = "pilotwave_worker.header_violation";
pub const FAILURE_PATH_POLICY: &str = "pilotwave_worker.path_policy_violation";
pub const FAILURE_ATTACHMENT_MISSING: &str = "pilotwave_worker.attachment_missing";
pub const FAILURE_SMTP_PERMANENT: &str = "pilotwave_worker.smtp_permanent";
pub const FAILURE_SMTP_EXHAUSTED: &str = "pilotwave_worker.smtp_exhausted";

// Malformed payload errors share one code (`schema_error` kept for
// backwards compatibility with existing FAILED rows and monitors).
// No version gate: any `schema` key, in `kwargs` or at the root, is ignored.

/// Límite de `traceback` en bytes (contrato §Failure codes: redaccion +
/// truncado 4 KiB = 4096; decision owner Lote R).
/// Se trunca por borde de char para no partir UTF-8.
pub const TRACEBACK_MAX_BYTES: usize = 4096;

/// A validated email payload, ready for the sender.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EmailContent {
    pub to: String,
    pub subject: String,
    pub text_body: Option<String>,
    pub html_body: Option<String>,
    pub attachment_paths: Vec<String>,
    pub attachment_sizes: Vec<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContractError {
    pub code: &'static str,
    pub detail: String,
}

impl ContractError {
    fn schema(detail: impl Into<String>) -> Self {
        Self {
            code: "pilotwave_worker.schema_error",
            detail: detail.into(),
        }
    }
    fn header(detail: impl Into<String>) -> Self {
        Self {
            code: FAILURE_HEADER,
            detail: detail.into(),
        }
    }
    fn path(detail: impl Into<String>) -> Self {
        Self {
            code: FAILURE_PATH_POLICY,
            detail: detail.into(),
        }
    }
}

/// `to`: single addr-spec, printable ASCII, one `@`, no `<`/`>` or controls.
pub fn is_valid_to(value: &str) -> bool {
    let mut parts = value.split('@');
    match (parts.next(), parts.next(), parts.next()) {
        (Some(local), Some(domain), None) if !local.is_empty() && !domain.is_empty() => {}
        _ => return false,
    }
    value
        .chars()
        .all(|c| matches!(c, '!'..='9' | ';'..='~') && c != '<' && c != '>')
}

/// `subject`: ≤ 500 chars, no controls.
pub fn is_valid_subject(value: &str) -> bool {
    value.chars().count() <= 500
        && !value
            .chars()
            .any(|c| c == '\r' || c == '\n' || c < '\u{20}' || c == '\u{7f}')
}

/// Parse and validate a raw `args_kwargs` JSON string.
///
/// No version gate: a `schema` key is ignored wherever it appears
/// (kept working for old rows that carry one). Only real fields
/// (`to`/`subject`/bodies/attachments) are validated.
pub fn parse_args_kwargs(raw: &str) -> Result<EmailContent, ContractError> {
    if raw.len() > MAX_ARGS_JSON_BYTES {
        return Err(ContractError::schema("args_kwargs too large"));
    }
    let root: Value = serde_json::from_str(raw)
        .map_err(|error| ContractError::schema(format!("invalid JSON: {error}")))?;
    let kwargs = root
        .get("kwargs")
        .ok_or_else(|| ContractError::schema("missing kwargs"))?;

    let to = kwargs
        .get("to")
        .and_then(Value::as_str)
        .ok_or_else(|| ContractError::header("missing to"))?;
    if !is_valid_to(to) {
        return Err(ContractError::header(format!("invalid to: {to:?}")));
    }
    let subject = kwargs
        .get("subject")
        .and_then(Value::as_str)
        .ok_or_else(|| ContractError::header("missing subject"))?;
    if !is_valid_subject(subject) {
        return Err(ContractError::header("invalid subject"));
    }

    let text_body = string_field(kwargs, "text_body")?;
    let html_body = string_field(kwargs, "html_body")?;
    if text_body.is_none() && html_body.is_none() {
        return Err(ContractError::schema("missing body"));
    }
    if text_body.as_ref().is_some_and(|v| v.len() > MAX_BODY_BYTES)
        || html_body.as_ref().is_some_and(|v| v.len() > MAX_BODY_BYTES)
    {
        return Err(ContractError::schema("body too large"));
    }

    let (attachment_paths, attachment_sizes) = parse_attachments(kwargs)?;
    Ok(EmailContent {
        to: to.to_owned(),
        subject: subject.to_owned(),
        text_body,
        html_body,
        attachment_paths,
        attachment_sizes,
    })
}

fn string_field(kwargs: &Value, key: &str) -> Result<Option<String>, ContractError> {
    match kwargs.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(value)) => Ok(Some(value.clone())),
        Some(_) => Err(ContractError::schema(format!("{key} must be a string"))),
    }
}

fn parse_attachments(kwargs: &Value) -> Result<(Vec<String>, Vec<u64>), ContractError> {
    let (Some(paths), Some(sizes)) = (
        kwargs.get("attachment_paths"),
        kwargs.get("attachment_sizes"),
    ) else {
        return match (
            kwargs.get("attachment_paths"),
            kwargs.get("attachment_sizes"),
        ) {
            (None, None) => Ok((Vec::new(), Vec::new())),
            _ => Err(ContractError::schema(
                "attachment paths/sizes must be paired",
            )),
        };
    };
    let (Some(paths), Some(sizes)) = (paths.as_array(), sizes.as_array()) else {
        return Err(ContractError::schema("attachments must be arrays"));
    };
    if paths.len() != sizes.len() {
        return Err(ContractError::schema("attachment length mismatch"));
    }
    if paths.len() > MAX_ATTACHMENTS {
        return Err(ContractError::path("too many attachments"));
    }

    let mut out_paths = Vec::with_capacity(paths.len());
    let mut out_sizes = Vec::with_capacity(sizes.len());
    let mut total = 0_u64;
    for (index, (path, size)) in paths.iter().zip(sizes).enumerate() {
        let path = path
            .as_str()
            .filter(|value| !value.is_empty() && !value.contains(['\r', '\n', '\0']))
            .ok_or_else(|| ContractError::schema(format!("invalid path at {index}")))?;
        let size = size
            .as_u64()
            .ok_or_else(|| ContractError::schema(format!("invalid size at {index}")))?;
        if size > MAX_FILE_BYTES {
            return Err(ContractError::path("file exceeds per-file cap"));
        }
        total = total.saturating_add(size);
        if total > MAX_TOTAL_BYTES {
            return Err(ContractError::path("attachments exceed total cap"));
        }
        out_paths.push(path.to_owned());
        out_sizes.push(size);
    }
    Ok((out_paths, out_sizes))
}

#[cfg(test)]
mod tests {
    use super::*;

    const EXAMPLE: &str = include_str!("../../contracts/payloads/email-v1.json");
    const EXAMPLE_ATTACHMENTS: &str =
        include_str!("../../contracts/payloads/email-v1-attachments.json");

    #[test]
    fn contract_examples_parse() {
        let content = parse_args_kwargs(EXAMPLE).expect("email-v1.json must be valid");
        assert_eq!(content.to, "team@example.com");
        assert!(content.text_body.is_some() && content.html_body.is_some());

        let content = parse_args_kwargs(EXAMPLE_ATTACHMENTS).expect("attachments example is valid");
        assert_eq!(content.attachment_sizes, vec![123456, 65432]);
    }

    #[test]
    fn schema_key_is_ignored() {
        // No version gate: rows with, without, or with bogus `schema`
        // (kwargs or root) all parse as long as real fields are valid.
        // Old rows carrying `"schema": 1` keep draining.
        for raw in [
            r#"{"args":[],"kwargs":{"to":"a@b.com","subject":"s","text_body":"x"}}"#,
            r#"{"args":[],"kwargs":{"schema":1,"to":"a@b.com","subject":"s","text_body":"x"}}"#,
            r#"{"schema":1,"args":[],"kwargs":{"to":"a@b.com","subject":"s","text_body":"x"}}"#,
            r#"{"schema":999,"args":[],"kwargs":{"schema":2,"to":"a@b.com","subject":"s","text_body":"x"}}"#,
        ] {
            assert!(parse_args_kwargs(raw).is_ok(), "{raw}");
        }
    }

    #[test]
    fn rejects_bad_headers() {
        let raw = r#"{"args":[],"kwargs":{"to":"a b@c.com","subject":"s","text_body":"x"}}"#;
        assert_eq!(
            parse_args_kwargs(raw).unwrap_err().code,
            "pilotwave_worker.header_violation"
        );
    }

    #[test]
    fn constants_match_contract_sql() {
        assert_eq!(QUEUE_TABLE, "django_tasks_database_dbtaskresult");
        assert_eq!(HEARTBEAT_TABLE, "pilotwave_heartbeat");
        assert_eq!(STATUS_SUCCESSFUL, "SUCCESSFUL");
        assert_eq!(RUN_AFTER_SENTINEL, "9999-01-01T00:00:00Z");
    }

    #[test]
    fn header_helpers_match_python() {
        assert!(is_valid_to("a=b@example.com"));
        assert!(!is_valid_to("a<b@example.com"));
        assert!(!is_valid_to("a b@example.com"));
        assert!(is_valid_subject("Hola"));
        assert!(!is_valid_subject("con\nsalto"));
    }
}
