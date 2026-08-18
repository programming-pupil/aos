use std::collections::BTreeMap;
use std::sync::OnceLock;

use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[must_use]
pub fn explicit_env_opt_in_enabled(name: &str) -> bool {
    std::env::var(name)
        .ok()
        .as_deref()
        .is_some_and(explicit_opt_in_value)
}

#[must_use]
pub fn explicit_opt_in_value(value: &str) -> bool {
    matches!(
        value.trim().to_ascii_lowercase().as_str(),
        "1" | "true" | "yes" | "on"
    )
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SensitiveDataCategory {
    ApiKey,
    BearerToken,
    Jwt,
    PrivateKey,
    CloudCredential,
    CredentialUrl,
    CredentialAssignment,
    Email,
    Phone,
    PaymentCard,
}

impl SensitiveDataCategory {
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::ApiKey => "api_key",
            Self::BearerToken => "bearer_token",
            Self::Jwt => "jwt",
            Self::PrivateKey => "private_key",
            Self::CloudCredential => "cloud_credential",
            Self::CredentialUrl => "credential_url",
            Self::CredentialAssignment => "credential_assignment",
            Self::Email => "email",
            Self::Phone => "phone",
            Self::PaymentCard => "payment_card",
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum DataProtectionMode {
    #[default]
    SecretsOnly,
    StrictPii,
}

/// Global deployment policy used at process-wide boundaries that do not carry
/// tenant policy yet. Secret protection is always enabled; strict PII
/// redaction requires an explicit opt-in so existing business workflows are
/// not silently changed during upgrades.
#[must_use]
pub fn configured_data_protection_mode() -> DataProtectionMode {
    if explicit_env_opt_in_enabled("AOS_DATA_PROTECTION_STRICT_PII") {
        DataProtectionMode::StrictPii
    } else {
        DataProtectionMode::SecretsOnly
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DataProtectionReport {
    pub redacted: bool,
    pub finding_count: usize,
    pub categories: BTreeMap<String, usize>,
}

impl DataProtectionReport {
    fn record(&mut self, category: SensitiveDataCategory, count: usize) {
        if count == 0 {
            return;
        }
        self.redacted = true;
        self.finding_count = self.finding_count.saturating_add(count);
        *self
            .categories
            .entry(category.label().to_string())
            .or_default() += count;
    }

    pub fn merge(&mut self, other: &Self) {
        self.redacted |= other.redacted;
        self.finding_count = self.finding_count.saturating_add(other.finding_count);
        for (category, count) in &other.categories {
            *self.categories.entry(category.clone()).or_default() += count;
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProtectedText {
    pub value: String,
    pub report: DataProtectionReport,
}

struct SensitivePattern {
    category: SensitiveDataCategory,
    regex: Regex,
    replacement: &'static str,
    pii: bool,
}

#[allow(clippy::too_many_lines)]
fn patterns() -> &'static [SensitivePattern] {
    static PATTERNS: OnceLock<Vec<SensitivePattern>> = OnceLock::new();
    PATTERNS.get_or_init(|| {
        vec![
            SensitivePattern {
                category: SensitiveDataCategory::PrivateKey,
                regex: Regex::new(
                    r"(?s)-----BEGIN (?:RSA |EC |OPENSSH |DSA )?PRIVATE KEY-----.*?-----END (?:RSA |EC |OPENSSH |DSA )?PRIVATE KEY-----",
                )
                .expect("valid private-key regex"),
                replacement: "[REDACTED_PRIVATE_KEY]",
                pii: false,
            },
            SensitivePattern {
                category: SensitiveDataCategory::BearerToken,
                regex: Regex::new(r"(?i)(authorization\s*:\s*bearer\s+)[^\s,;]+")
                    .expect("valid authorization bearer regex"),
                replacement: "$1[REDACTED]",
                pii: false,
            },
            SensitivePattern {
                category: SensitiveDataCategory::BearerToken,
                regex: Regex::new(r"(?i)\b(bearer\s+)[a-z0-9._~+/=-]{12,}")
                    .expect("valid bearer regex"),
                replacement: "$1[REDACTED]",
                pii: false,
            },
            SensitivePattern {
                category: SensitiveDataCategory::Jwt,
                regex: Regex::new(
                    r"\beyJ[a-zA-Z0-9_-]{8,}\.[a-zA-Z0-9_-]{8,}\.[a-zA-Z0-9_-]{8,}\b",
                )
                .expect("valid JWT regex"),
                replacement: "[REDACTED_JWT]",
                pii: false,
            },
            SensitivePattern {
                category: SensitiveDataCategory::CredentialUrl,
                regex: Regex::new(
                    r"(?i)\b(mysql|postgres(?:ql)?|trino|presto|mongodb(?:\+srv)?|redis|amqp|https?)://([^:/\s]+):([^@\s/]+)@",
                )
                .expect("valid credential URL regex"),
                replacement: "$1://$2:[REDACTED]@",
                pii: false,
            },
            SensitivePattern {
                category: SensitiveDataCategory::CredentialAssignment,
                regex: Regex::new(
                    r"(?i)([?&](?:api[_-]?key|apikey|token|access[_-]?token|auth|secret|password|passwd|signature)=)[^&#\s]+",
                )
                .expect("valid credential query regex"),
                replacement: "$1[REDACTED]",
                pii: false,
            },
            SensitivePattern {
                category: SensitiveDataCategory::CloudCredential,
                regex: Regex::new(r"\b(?:AKIA|ASIA)[A-Z0-9]{16}\b")
                    .expect("valid AWS access-key regex"),
                replacement: "[REDACTED_CLOUD_CREDENTIAL]",
                pii: false,
            },
            SensitivePattern {
                category: SensitiveDataCategory::ApiKey,
                regex: Regex::new(
                    r"(?i)\b(?:sk-[a-z0-9_-]{12,}|gh[pousr]_[a-z0-9]{20,}|glpat-[a-z0-9_-]{12,}|xox[baprs]-[a-z0-9-]{12,}|AIza[a-z0-9_-]{30,})\b",
                )
                .expect("valid API-key regex"),
                replacement: "[REDACTED_API_KEY]",
                pii: false,
            },
            SensitivePattern {
                category: SensitiveDataCategory::CredentialAssignment,
                regex: Regex::new(
                    r"(?i)\b(api[_-]?key|apikey|password|passwd|token|client[_-]?secret|access[_-]?token|auth[_-]?token|refresh[_-]?token|secret[_-]?key)\b(=)([^\s,;}]+)",
                )
                .expect("valid compact credential assignment regex"),
                replacement: "$1$2[REDACTED]",
                pii: false,
            },
            SensitivePattern {
                category: SensitiveDataCategory::CredentialAssignment,
                regex: Regex::new(
                    r"(?i)\b(api[_-]?key|apikey|password|passwd|token|client[_-]?secret|access[_-]?token|auth[_-]?token|refresh[_-]?token|secret[_-]?key)\b(\s*:\s*)([^\s,;}]+)",
                )
                .expect("valid unquoted credential field regex"),
                replacement: "$1$2[REDACTED]",
                pii: false,
            },
            SensitivePattern {
                category: SensitiveDataCategory::CredentialAssignment,
                regex: Regex::new(
                    r#"(?i)\b(api[_-]?key|apikey|password|passwd|token|client[_-]?secret|access[_-]?token|auth[_-]?token|refresh[_-]?token|secret[_-]?key)\b(\s*[:=]\s*)(\"[^\"]*\"|'[^']*')"#,
                )
                .expect("valid quoted credential assignment regex"),
                replacement: "$1$2[REDACTED]",
                pii: false,
            },
            SensitivePattern {
                category: SensitiveDataCategory::Email,
                regex: Regex::new(r"(?i)\b[a-z0-9.!#$%&'*+/=?^_`{|}~-]+@[a-z0-9-]+(?:\.[a-z0-9-]+)+\b")
                    .expect("valid email regex"),
                replacement: "[REDACTED_EMAIL]",
                pii: true,
            },
            SensitivePattern {
                category: SensitiveDataCategory::Phone,
                regex: Regex::new(r"(?:\+?86[- ]?)?1[3-9]\d{9}\b")
                    .expect("valid phone regex"),
                replacement: "[REDACTED_PHONE]",
                pii: true,
            },
            SensitivePattern {
                category: SensitiveDataCategory::PaymentCard,
                regex: Regex::new(r"\b(?:\d[ -]*?){13,19}\b")
                    .expect("valid payment-card regex"),
                replacement: "[REDACTED_PAYMENT_CARD]",
                pii: true,
            },
        ]
    })
}

#[must_use]
pub fn protect_sensitive_text(text: &str, mode: DataProtectionMode) -> ProtectedText {
    let mut value = text.to_string();
    let mut report = DataProtectionReport::default();
    for pattern in patterns() {
        if pattern.pii && mode != DataProtectionMode::StrictPii {
            continue;
        }
        let count = pattern.regex.find_iter(&value).count();
        if count > 0 {
            value = pattern
                .regex
                .replace_all(&value, pattern.replacement)
                .into_owned();
            report.record(pattern.category, count);
        }
    }
    ProtectedText { value, report }
}

#[must_use]
pub fn inspect_sensitive_text(text: &str, mode: DataProtectionMode) -> DataProtectionReport {
    protect_sensitive_text(text, mode).report
}

#[must_use]
#[allow(clippy::items_after_statements)]
pub fn protect_sensitive_json(
    value: &Value,
    mode: DataProtectionMode,
) -> (Value, DataProtectionReport) {
    crate::behavior_trace("SEC-002");
    fn protect(
        value: &Value,
        mode: DataProtectionMode,
        report: &mut DataProtectionReport,
    ) -> Value {
        match value {
            Value::String(text) => {
                let protected = protect_sensitive_text(text, mode);
                report.merge(&protected.report);
                Value::String(protected.value)
            }
            Value::Array(values) => Value::Array(
                values
                    .iter()
                    .map(|value| protect(value, mode, report))
                    .collect(),
            ),
            Value::Object(values) => Value::Object(
                values
                    .iter()
                    .map(|(key, value)| {
                        let protected_key = protect_sensitive_text(key, mode);
                        report.merge(&protected_key.report);
                        let protected_value = if sensitive_json_field_name(key)
                            && matches!(value, Value::String(_) | Value::Number(_))
                        {
                            report.record(SensitiveDataCategory::CredentialAssignment, 1);
                            Value::String("[REDACTED]".to_string())
                        } else {
                            protect(value, mode, report)
                        };
                        (protected_key.value, protected_value)
                    })
                    .collect(),
            ),
            _ => value.clone(),
        }
    }

    let mut report = DataProtectionReport::default();
    let protected = protect(value, mode, &mut report);
    (protected, report)
}

fn sensitive_json_field_name(key: &str) -> bool {
    let normalized = key
        .chars()
        .filter(char::is_ascii_alphanumeric)
        .collect::<String>()
        .to_ascii_lowercase();
    matches!(
        normalized.as_str(),
        "apikey"
            | "password"
            | "passwd"
            | "clientsecret"
            | "accesstoken"
            | "authtoken"
            | "refreshtoken"
            | "secretkey"
            | "authorization"
            | "cookie"
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    #[test]
    fn redacts_credentials_without_recording_plaintext_in_report() {
        let input = "Authorization: Bearer opaque-token-123456 mysql://reader:db-secret@example.test/aos api_key=sk-1234567890abcdef token=manifest-secret https://example.test/?token=query-secret-value";
        let protected = protect_sensitive_text(input, DataProtectionMode::SecretsOnly);
        assert!(!protected.value.contains("opaque-token-123456"));
        assert!(!protected.value.contains("db-secret"));
        assert!(!protected.value.contains("sk-1234567890abcdef"));
        assert!(!protected.value.contains("manifest-secret"));
        assert!(!protected.value.contains("query-secret-value"));
        let report = serde_json::to_string(&protected.report).unwrap();
        assert!(!report.contains("opaque-token-123456"));
        assert!(!report.contains("db-secret"));
        assert!(!report.contains("sk-1234567890abcdef"));
        assert!(!report.contains("manifest-secret"));
        assert!(!report.contains("query-secret-value"));
        assert!(protected.report.finding_count >= 5);
    }

    #[test]
    fn pii_is_only_redacted_in_strict_mode() {
        let input = "contact owner@example.test or 13800138000";
        let default = protect_sensitive_text(input, DataProtectionMode::SecretsOnly);
        assert_eq!(default.value, input);
        let strict = protect_sensitive_text(input, DataProtectionMode::StrictPii);
        assert!(!strict.value.contains("owner@example.test"));
        assert!(!strict.value.contains("13800138000"));
    }

    #[test]
    fn recursively_redacts_json_strings() {
        let input = serde_json::json!({
            "headers": {"Authorization": "opaque-token-123456"},
            "password": "database-secret",
            "items": ["password=hunter2-value"]
        });
        let (protected, report) = protect_sensitive_json(&input, DataProtectionMode::SecretsOnly);
        let serialized = protected.to_string();
        assert!(!serialized.contains("opaque-token-123456"));
        assert!(!serialized.contains("database-secret"));
        assert!(!serialized.contains("hunter2-value"));
        assert_eq!(report.finding_count, 3);
    }

    #[test]
    fn spaced_sql_identifier_comparison_is_not_corrupted() {
        let input = "SELECT password_hash FROM users WHERE password = password_hash";
        let protected = protect_sensitive_text(input, DataProtectionMode::SecretsOnly);
        assert_eq!(protected.value, input);
        assert!(!protected.report.redacted);
    }

    #[test]
    fn security_opt_in_requires_an_explicit_true_value() {
        for enabled in ["1", "true", "TRUE", " yes ", "on"] {
            assert!(explicit_opt_in_value(enabled));
        }
        for disabled in ["", "0", "false", "off", "enabled", "allow"] {
            assert!(!explicit_opt_in_value(disabled));
        }
    }

    #[test]
    fn redacts_yaml_and_additional_provider_key_shapes() {
        let input =
            "password: database-secret glpat-1234567890abcdef AIzaSyA1234567890abcdefghijklmnopq";
        let protected = protect_sensitive_text(input, DataProtectionMode::SecretsOnly);
        assert!(!protected.value.contains("database-secret"));
        assert!(!protected.value.contains("glpat-1234567890abcdef"));
        assert!(!protected
            .value
            .contains("AIzaSyA1234567890abcdefghijklmnopq"));
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(100))]

        #[test]
        fn redaction_is_idempotent(prefix in "[a-zA-Z0-9 ]{0,40}", suffix in "[a-zA-Z0-9 ]{0,40}") {
            let input = format!("{prefix} api_key=sk-1234567890abcdef {suffix}");
            let once = protect_sensitive_text(&input, DataProtectionMode::SecretsOnly);
            let twice = protect_sensitive_text(&once.value, DataProtectionMode::SecretsOnly);
            prop_assert_eq!(once.value, twice.value);
        }
    }
}
