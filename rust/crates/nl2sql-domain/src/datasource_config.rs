//! Datasource connection configuration helpers.
//!
//! This module owns pure config DTOs and string/preview helpers shared by the
//! web API, schema discovery, query execution and cancellation paths. It does
//! not encrypt, query databases, or know about HTTP handlers.

use serde::Deserialize;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NormalizedHostInput {
    pub host: String,
    pub port: Option<u16>,
    pub secure: Option<bool>,
}

#[derive(Debug, Deserialize)]
pub struct SqlConfig {
    pub host: String,
    pub port: u16,
    pub database: String,
    pub username: String,
    pub password: String,
    #[allow(dead_code)]
    pub ssl: Option<bool>,
    #[allow(dead_code)]
    pub extra_params: Option<serde_json::Value>,
}

#[derive(Debug, Deserialize)]
pub struct ClickHouseConfig {
    pub host: String,
    pub port: u16,
    pub database: String,
    pub username: String,
    pub password: String,
}

#[derive(Debug, Deserialize)]
pub struct TrinoConfig {
    pub host: String,
    pub port: u16,
    pub catalog: String,
    #[serde(default)]
    pub schema: String,
    #[serde(default)]
    pub schemas: Vec<String>,
    pub username: String,
    #[serde(default)]
    pub password: String,
    #[serde(default)]
    pub ssl: Option<bool>,
    #[serde(default)]
    pub basic_auth: Option<bool>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct MongoConfig {
    #[serde(default, alias = "connection_string")]
    pub uri: String,
    #[serde(default)]
    pub host: String,
    #[serde(default = "default_mongodb_port")]
    pub port: u16,
    pub database: String,
    #[serde(default)]
    pub username: String,
    #[serde(default)]
    pub password: String,
    #[serde(default)]
    pub auth_source: Option<String>,
    #[serde(default)]
    pub tls: Option<bool>,
}

const fn default_mongodb_port() -> u16 {
    27017
}

impl TrinoConfig {
    #[must_use]
    pub fn effective_schemas(&self) -> Vec<String> {
        normalize_trino_schemas(&self.schema, self.schemas.iter().map(String::as_str))
    }
}

#[must_use]
pub fn normalize_trino_schemas<'a>(
    primary_schema: &str,
    schemas: impl IntoIterator<Item = &'a str>,
) -> Vec<String> {
    let mut out = Vec::new();
    for schema in schemas {
        let trimmed = schema.trim();
        if !trimmed.is_empty() && !out.iter().any(|s| s == trimmed) {
            out.push(trimmed.to_string());
        }
    }
    let primary = primary_schema.trim();
    if !primary.is_empty() {
        out.retain(|schema| schema != primary);
        out.insert(0, primary.to_string());
    }
    if out.is_empty() {
        out.push("default".to_string());
    }
    out
}

pub fn normalize_host_input(input: &str) -> NormalizedHostInput {
    let trimmed = input.trim();
    let lower = trimmed.to_ascii_lowercase();
    let (rest, secure) = if lower.starts_with("https://") {
        (&trimmed[8..], Some(true))
    } else if lower.starts_with("http://") {
        (&trimmed[7..], Some(false))
    } else {
        (trimmed, None)
    };
    let authority = rest
        .trim_start_matches('/')
        .split('/')
        .next()
        .unwrap_or(rest)
        .trim();
    let (host, port) = if authority.starts_with('[') {
        match authority.find(']') {
            Some(end) => {
                let host = authority[..=end].to_string();
                let port = authority[end + 1..]
                    .strip_prefix(':')
                    .and_then(|p| p.parse::<u16>().ok());
                (host, port)
            }
            None => (authority.to_string(), None),
        }
    } else {
        match authority.rsplit_once(':') {
            Some((host, port)) if port.chars().all(|c| c.is_ascii_digit()) => {
                (host.to_string(), port.parse::<u16>().ok())
            }
            _ => (authority.to_string(), None),
        }
    };
    NormalizedHostInput { host, port, secure }
}

pub fn redact_sensitive_config(config: &serde_json::Value) -> serde_json::Value {
    let sensitive_keys = [
        "password",
        "auth_token",
        "secret",
        "api_key",
        "token",
        "private_key",
        "uri",
        "connection_string",
    ];
    match config {
        serde_json::Value::Object(map) => {
            let mut redacted = serde_json::Map::new();
            for (k, v) in map {
                if sensitive_keys.contains(&k.as_str()) {
                    redacted.insert(
                        k.clone(),
                        serde_json::Value::String("[REDACTED]".to_string()),
                    );
                } else if k == "_encrypted" || k == "nonce" || k == "data" {
                    redacted.insert(
                        k.clone(),
                        serde_json::Value::String("[ENCRYPTED]".to_string()),
                    );
                } else {
                    redacted.insert(k.clone(), redact_sensitive_config(v));
                }
            }
            serde_json::Value::Object(redacted)
        }
        serde_json::Value::Array(arr) => {
            serde_json::Value::Array(arr.iter().map(redact_sensitive_config).collect())
        }
        _ => config.clone(),
    }
}

pub fn build_mysql_url_parts(
    username: &str,
    password: &str,
    host: &str,
    port: u16,
    database: &str,
) -> String {
    let user_enc = urlencoding::encode(username);
    let pass_enc = urlencoding::encode(password);
    let db_enc = urlencoding::encode(database);
    format!("mysql://{user_enc}:{pass_enc}@{host}:{port}/{db_enc}")
}

pub fn build_postgres_url_parts(
    username: &str,
    password: &str,
    host: &str,
    port: u16,
    database: &str,
) -> String {
    let user_enc = urlencoding::encode(username);
    let pass_enc = urlencoding::encode(password);
    let db_enc = urlencoding::encode(database);
    format!("postgres://{user_enc}:{pass_enc}@{host}:{port}/{db_enc}")
}

pub fn build_mysql_url(config: &SqlConfig) -> String {
    build_mysql_url_parts(
        &config.username,
        &config.password,
        &config.host,
        config.port,
        &config.database,
    )
}

pub fn build_postgres_url(config: &SqlConfig) -> String {
    build_postgres_url_parts(
        &config.username,
        &config.password,
        &config.host,
        config.port,
        &config.database,
    )
}

pub fn build_mongodb_uri(config: &MongoConfig) -> Result<String, String> {
    let explicit = config.uri.trim();
    if !explicit.is_empty() {
        if explicit.starts_with("mongodb://") || explicit.starts_with("mongodb+srv://") {
            return Ok(explicit.to_string());
        }
        return Err("MongoDB URI must start with mongodb:// or mongodb+srv://".to_string());
    }
    let host = config.host.trim();
    if host.is_empty() {
        return Err("MongoDB host or URI is required".to_string());
    }
    let credentials = if config.username.is_empty() {
        String::new()
    } else {
        format!(
            "{}:{}@",
            urlencoding::encode(&config.username),
            urlencoding::encode(&config.password)
        )
    };
    let mut query = Vec::new();
    if let Some(source) = config
        .auth_source
        .as_deref()
        .filter(|value| !value.trim().is_empty())
    {
        query.push(format!("authSource={}", urlencoding::encode(source.trim())));
    }
    if let Some(tls) = config.tls {
        query.push(format!("tls={tls}"));
    }
    let suffix = if query.is_empty() {
        String::new()
    } else {
        format!("?{}", query.join("&"))
    };
    Ok(format!(
        "mongodb://{credentials}{host}:{}/{}{}",
        config.port,
        urlencoding::encode(config.database.trim()),
        suffix
    ))
}

/// Generates a short datasource description when users leave description empty.
pub fn format_datasource_description(db_type: &str, table_names: &[&str]) -> String {
    let db_label = match db_type {
        "mysql" => "MySQL".to_string(),
        "tidb" => "TiDB".to_string(),
        "postgres" => "PostgreSQL".to_string(),
        "clickhouse" => "ClickHouse".to_string(),
        "presto" => "Presto".to_string(),
        "trino" => "Trino".to_string(),
        "mongodb" => "MongoDB".to_string(),
        other => other.to_ascii_uppercase(),
    };
    let tables_str = if table_names.len() > 10 {
        format!(
            "{} and {} more tables",
            table_names[..10].join(", "),
            table_names.len() - 10
        )
    } else {
        table_names.join(", ")
    };
    format!("{db_label} database containing {tables_str}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_trino_schemas_keeps_primary_first_and_dedupes() {
        let schemas = normalize_trino_schemas("mps_prod", ["ods", "mps_prod", "  ", "dwd"]);
        assert_eq!(schemas, vec!["mps_prod", "ods", "dwd"]);
    }

    #[test]
    fn normalize_trino_schemas_defaults_when_empty() {
        let schemas = normalize_trino_schemas("", std::iter::empty::<&str>());
        assert_eq!(schemas, vec!["default"]);
    }

    #[test]
    fn builds_mongodb_uri_without_leaking_raw_credentials() {
        let config = MongoConfig {
            uri: String::new(),
            host: "localhost".to_string(),
            port: 27017,
            database: "sales".to_string(),
            username: "a@b".to_string(),
            password: "p:/x".to_string(),
            auth_source: Some("admin".to_string()),
            tls: Some(false),
        };
        let uri = build_mongodb_uri(&config).expect("valid config");
        assert!(uri.contains("a%40b:p%3A%2Fx@localhost:27017/sales"));
        assert!(uri.contains("authSource=admin"));
    }
}
