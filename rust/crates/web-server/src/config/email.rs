//! SMTP / email configuration.
//!
//! Loads SMTP settings from environment variables. Gracefully degrades when not
//! configured (invite emails fall back to showing the URL on the UI).

use serde::Deserialize;

#[derive(Debug, Clone, Deserialize)]
pub struct SmtpConfig {
    pub host: String,
    pub port: u16,
    /// SMTP connection security mode. When true, uses TLS (SMTPS).
    /// When false, uses plain TCP (not recommended for production).
    pub use_tls: bool,
    pub username: String,
    pub password: String,
    pub from: String,
}

impl SmtpConfig {
    /// Load from environment variables.
    /// Returns `None` if SMTP is not configured.
    pub fn from_env() -> Option<Self> {
        let host = std::env::var("SMTP_HOST").ok()?;
        let port: u16 = std::env::var("SMTP_PORT").ok()?.parse().ok()?;
        let use_tls = std::env::var("SMTP_USE_TLS")
            .map(|v| v == "1" || v.to_lowercase() == "true")
            .unwrap_or(true);
        let username = std::env::var("SMTP_USERNAME").ok()?;
        let password = std::env::var("SMTP_PASSWORD").ok()?;
        let from = std::env::var("SMTP_FROM").unwrap_or_else(|_| format!("AOS <noreply@{host}>"));

        Some(Self {
            host,
            port,
            use_tls,
            username,
            password,
            from,
        })
    }
}
