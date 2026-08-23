//! Glue: validate an `email_send` request, resolve the SMTP credential
//! vault-first, build a `lettre` message, and send it. A
//! `EMAIL_PLUGIN_SMTP_STUB=true` process env switches the send into stub mode
//! (no network), which keeps the fake-kernel tests offline and lets an
//! operator smoke-test the plugin without an SMTP account. The resolved
//! password is only ever handed to `lettre`'s `Credentials`; it never appears
//! in an error string or log line.

use vynkor_sdk::VynkorClient;

use crate::request::{self, EmailSendParams};

/// When set to the exact string `true`, `handle_email_send` skips the real
/// SMTP send and returns a successful, clearly-marked stub response.
const SMTP_STUB_ENV: &str = "EMAIL_PLUGIN_SMTP_STUB";

fn unix_millis() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

fn smtp_stub_enabled() -> bool {
    std::env::var(SMTP_STUB_ENV).as_deref() == Ok("true")
}

/// Build the `lettre` message from parsed params. `lettre`'s `Address` parser
/// re-validates the addresses (stricter than `request::is_valid_email`), so a
/// caller-provided address that slips past the parse-time check still fails
/// cleanly here without leaking anything.
fn build_message(params: &EmailSendParams) -> Result<lettre::Message, String> {
    use lettre::message::header::ContentType;
    use lettre::message::Mailbox;
    use lettre::Address;

    let from: Address = params
        .from
        .parse()
        .map_err(|e| format!("invalid from address: {e}"))?;
    let to: Address = params
        .to
        .parse()
        .map_err(|e| format!("invalid recipient address: {e}"))?;

    let content_type = if params.is_html {
        ContentType::TEXT_HTML
    } else {
        ContentType::TEXT_PLAIN
    };

    lettre::Message::builder()
        .from(Mailbox::new(None, from))
        .to(Mailbox::new(None, to))
        .subject(params.subject.clone())
        .header(content_type)
        .body(params.body.clone())
        .map_err(|e| format!("failed to build email message: {e}"))
}

/// Handle one `email_send` action end to end. Returns the JSON to place in
/// `ActionResponse.data_json` on success, or a human-readable error (never
/// containing the resolved password) on failure.
pub async fn handle_email_send(
    client: &mut VynkorClient,
    params_json: &[u8],
) -> Result<Vec<u8>, String> {
    let params = request::parse_request(params_json)?;

    let allowed_cred_envs = request::parse_allowed_cred_envs(
        &std::env::var(request::ALLOWED_CRED_ENVS_ENV).unwrap_or_default(),
    );
    if !request::is_allowed_cred_env(&params.credentials_env, &allowed_cred_envs) {
        return Err(format!(
            "credentials_env '{}' is not in the operator's {} allowlist",
            params.credentials_env,
            request::ALLOWED_CRED_ENVS_ENV
        ));
    }

    // Vault-first resolution. The value is bound here and only ever moved
    // into `Credentials` below — never formatted into an error or log.
    let password = crate::key_resolve::resolve_secret(client, &params.credentials_env).await?;

    if smtp_stub_enabled() {
        let message_id = format!("stub-{}", unix_millis());
        return serde_json::to_vec(&serde_json::json!({
            "message_id": message_id,
            "to": params.to,
            "subject": params.subject,
            "stubbed": true,
            "smtp_host": params.smtp_host,
            "smtp_port": params.smtp_port,
        }))
        .map_err(|e| format!("failed to encode stub response: {e}"));
    }

    use lettre::transport::smtp::authentication::Credentials;
    use lettre::{AsyncSmtpTransport, AsyncTransport, Tokio1Executor};

    let email = build_message(&params)?;
    // Correlation id for the caller. Deliberately our own, not lettre's
    // generated Message-Id header (lettre 0.11 has no getter for it); it is
    // returned verbatim so the caller can correlate the reply with this send.
    let message_id = format!("email-{}", unix_millis());

    let creds = Credentials::new(params.smtp_user.clone(), password);

    let mailer = AsyncSmtpTransport::<Tokio1Executor>::relay(&params.smtp_host)
        .map_err(|e| {
            format!(
                "failed to configure SMTP relay for host '{}': {e}",
                params.smtp_host
            )
        })?
        .port(params.smtp_port)
        .credentials(creds)
        .timeout(Some(std::time::Duration::from_millis(params.timeout_ms)))
        .build();

    mailer
        .send(email)
        .await
        .map_err(|e| format!("SMTP send failed: {e}"))?;

    serde_json::to_vec(&serde_json::json!({
        "message_id": message_id,
        "to": params.to,
        "subject": params.subject,
        "stubbed": false,
    }))
    .map_err(|e| format!("failed to encode response: {e}"))
}
