//! Email sending via SMTP.
//!
//! Uses the `lettre` crate to send transactional emails.
//! When SMTP is not configured, all operations are no-ops.

use tracing::{error, info, warn};

use crate::config::email::SmtpConfig;

#[derive(Debug, Clone)]
pub struct EmailDelivery {
    pub configured: bool,
    pub sent: bool,
    pub error: Option<String>,
}

/// Send an invite email to a newly invited user.
pub async fn send_invite_email(
    to_email: &str,
    invite_url: &str,
    tenant_name: &str,
) -> EmailDelivery {
    let Some(cfg) = SmtpConfig::from_env() else {
        warn!(
            "SMTP not configured — invite email not sent (URL: {})",
            invite_url
        );
        return EmailDelivery {
            configured: false,
            sent: false,
            error: None,
        };
    };

    let subject = format!("[AOS] You've been invited to join {tenant_name}");
    let html_body = format!(
        r#"<!DOCTYPE html>
<html>
<head><meta charset="utf-8"></head>
<body style="font-family: -apple-system, BlinkMacSystemFont, 'Segoe UI', sans-serif; \
            max-width: 560px; margin: 40px auto; padding: 0 20px; color: #1a1a1a;">
  <div style="text-align: center; margin-bottom: 32px;">
    <h1 style="font-size: 24px; font-weight: 700; color: #1a1a1a;">
      You've been invited to join <span style="color: #7c3aed;">{tenant_name}</span>
    </h1>
  </div>
  <p style="font-size: 15px; line-height: 1.6; color: #374151;">
    You've been invited to join <strong>{tenant_name}</strong> on the Agent OS platform.
    Click the button below to set your password and activate your account.
  </p>
  <div style="text-align: center; margin: 32px 0;">
    <a href="{invite_url}"
       style="display: inline-block; background: linear-gradient(135deg, #7c3aed, #a855f7); \
              color: #fff; text-decoration: none; font-weight: 600; font-size: 15px; \
              padding: 12px 32px; border-radius: 8px;">
      Set Password & Activate Account
    </a>
  </div>
  <p style="font-size: 13px; color: #9ca3af; line-height: 1.5;">
    If the button doesn't work, copy and paste this link into your browser:<br/>
    <a href="{invite_url}" style="color: #7c3aed; word-break: break-all;">{invite_url}</a>
  </p>
  <p style="font-size: 12px; color: #d1d5db; margin-top: 32px;">
    This invitation link expires in 7 days. If you didn't expect this email, you can safely ignore it.
  </p>
</body>
</html>"#,
    );

    match send_email(&cfg, to_email, &subject, &html_body).await {
        Ok(()) => EmailDelivery {
            configured: true,
            sent: true,
            error: None,
        },
        Err(error) => EmailDelivery {
            configured: true,
            sent: false,
            error: Some(error),
        },
    }
}

async fn send_email(
    cfg: &SmtpConfig,
    to: &str,
    subject: &str,
    html_body: &str,
) -> std::result::Result<(), String> {
    use lettre::transport::smtp::authentication::Credentials;
    use lettre::AsyncSmtpTransport;
    use lettre::AsyncTransport;
    use lettre::Tokio1Executor;

    // 465 is implicit TLS (SMTPS). 587 and most non-465 TLS ports use STARTTLS.
    // Plain TCP is kept only for private/self-hosted SMTP relays.
    let builder = if cfg.use_tls {
        let secure_builder = if cfg.port == 465 {
            AsyncSmtpTransport::<Tokio1Executor>::relay(&cfg.host)
        } else {
            AsyncSmtpTransport::<Tokio1Executor>::starttls_relay(&cfg.host)
        };
        secure_builder.map_err(|error| {
            format!(
                "failed to configure TLS for SMTP host {}:{}: {error}",
                cfg.host, cfg.port
            )
        })?
    } else {
        AsyncSmtpTransport::<Tokio1Executor>::builder_dangerous(&cfg.host)
    };
    let mut builder = builder.port(cfg.port);
    if !cfg.username.trim().is_empty() {
        builder = builder.credentials(Credentials::new(cfg.username.clone(), cfg.password.clone()));
    }
    let transport = builder.build();

    let from_addr = cfg
        .from
        .parse()
        .map_err(|error| format!("invalid SMTP_FROM address: {error}"))?;
    let to_addr = to
        .parse()
        .map_err(|error| format!("invalid invite recipient address: {error}"))?;

    let email = match lettre::Message::builder()
        .from(from_addr)
        .to(to_addr)
        .subject(subject)
        .header(lettre::message::header::ContentType::TEXT_HTML)
        .body(html_body.to_string())
    {
        Ok(e) => e,
        Err(e) => {
            error!("failed to build invite email: {}", e);
            return Err(format!("failed to build invite email: {e}"));
        }
    };

    match transport.send(email).await {
        Ok(_) => {
            info!("invite email sent");
            Ok(())
        }
        Err(e) => {
            let safe_error = runtime::protect_sensitive_text(
                &e.to_string(),
                runtime::configured_data_protection_mode(),
            )
            .value;
            error!(error = %safe_error, "failed to send invite email");
            Err(safe_error)
        }
    }
}
