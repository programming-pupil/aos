//! JWT authentication utilities and middleware for the web server.

use axum::{
    extract::State,
    http::{Request, StatusCode},
    middleware::Next,
    response::{IntoResponse, Response},
    Json,
};
use serde_json::json;
use tokio::time::{timeout, Duration};

use crate::routes::setup::is_system_initialized;
use crate::state::AppState;

/// Return the request path used in logs. Query strings can contain WebSocket
/// JWTs, OAuth codes, search terms, or connector credentials and must never be
/// copied into application logs.
pub(crate) fn sanitized_request_uri(uri: &axum::http::Uri) -> String {
    uri.path().to_string()
}

const WEBSOCKET_AUTH_PROTOCOL: &str = "aos-auth";

/// Extract a JWT from an HTTP Authorization header or the WebSocket subprotocol
/// list. Full account tokens are intentionally never accepted from query
/// parameters because URLs are routinely retained by browsers and proxies.
fn extract_token(req: &Request<axum::body::Body>) -> Option<String> {
    let header_token = req
        .headers()
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .map(std::borrow::ToOwned::to_owned);

    if header_token.is_some() {
        return header_token;
    }

    let protocols = req
        .headers()
        .get(axum::http::header::SEC_WEBSOCKET_PROTOCOL)?
        .to_str()
        .ok()?
        .split(',')
        .map(str::trim)
        .collect::<Vec<_>>();
    protocols
        .windows(2)
        .find(|pair| pair[0].eq_ignore_ascii_case(WEBSOCKET_AUTH_PROTOCOL))
        .map(|pair| pair[1])
        .filter(|token| !token.is_empty())
        .map(ToOwned::to_owned)
}

fn preview_session_id_from_path(path: &str) -> Option<&str> {
    let rest = path.strip_prefix("/api/v1/rd/preview-sessions/")?;
    let (session_id, suffix) = rest.split_once("/proxy")?;
    if session_id.is_empty() || (!suffix.is_empty() && !suffix.starts_with('/')) {
        return None;
    }
    Some(session_id)
}

fn preview_query_token(req: &Request<axum::body::Body>) -> Option<(String, String)> {
    let session_id = preview_session_id_from_path(req.uri().path())?.to_string();
    let token = req.uri().query().and_then(|query| {
        query.split('&').find_map(|pair| {
            let (key, value) = pair.split_once('=')?;
            (key == "preview_token")
                .then(|| {
                    urlencoding::decode(value)
                        .ok()
                        .map(|value| value.into_owned())
                })
                .flatten()
        })
    })?;
    Some((session_id, token))
}

fn json_error(status: StatusCode, error: &str, message: &str) -> Response {
    (
        status,
        Json(json!({
            "error": error,
            "message": message,
            "status": status.as_u16(),
        })),
    )
        .into_response()
}

/// Middleware that requires a valid JWT Bearer token.
pub async fn require_auth(
    State(state): State<AppState>,
    mut req: Request<axum::body::Body>,
    next: Next,
) -> Response {
    let method = req.method().clone();
    let uri = sanitized_request_uri(req.uri());
    let claims = if let Some(token) = extract_token(&req) {
        crate::auth::verify_token(&state, &token).await
    } else if let Some((session_id, token)) = preview_query_token(&req) {
        crate::auth::verify_preview_token(&state, &token, &session_id).await
    } else {
        tracing::warn!(
            method = %method,
            uri = %uri,
            "request rejected: missing bearer token"
        );
        return json_error(
            StatusCode::UNAUTHORIZED,
            "unauthorized",
            "missing bearer token",
        );
    };

    match claims {
        Ok(claims) => {
            req.extensions_mut().insert(claims);
            next.run(req).await
        }
        Err(error) => {
            tracing::warn!(
                method = %method,
                uri = %uri,
                error = %error,
                "request rejected: invalid bearer token"
            );
            json_error(
                StatusCode::UNAUTHORIZED,
                "unauthorized",
                "invalid bearer token",
            )
        }
    }
}

/// Global middleware that blocks non-setup APIs until first-boot initialization
/// completes.
pub async fn require_setup_initialized(
    State(state): State<AppState>,
    req: Request<axum::body::Body>,
    next: Next,
) -> Response {
    let path = req.uri().path();
    if path.starts_with("/api/v1/setup") {
        return next.run(req).await;
    }
    if state.setup_initialized_cached() {
        return next.run(req).await;
    }
    let method = req.method().clone();
    let uri = sanitized_request_uri(req.uri());

    match timeout(Duration::from_secs(3), is_system_initialized(&state.db)).await {
        Err(_) => {
            tracing::error!(
                method = %method,
                uri = %uri,
                timeout_ms = 3000_u64,
                "failed to evaluate setup initialization status before timeout"
            );
            json_error(
                StatusCode::SERVICE_UNAVAILABLE,
                "database_unavailable",
                "database is not responding while checking setup status",
            )
        }
        Ok(Ok(true)) => {
            state.mark_setup_initialized();
            next.run(req).await
        }
        Ok(Ok(false)) => {
            tracing::warn!(
                method = %method,
                uri = %uri,
                "request blocked: system setup is required"
            );
            json_error(
                StatusCode::PRECONDITION_REQUIRED,
                "setup_required",
                "system setup is required before using this API",
            )
        }
        Ok(Err(err)) => {
            tracing::error!(
                method = %method,
                uri = %uri,
                error = %err,
                error_debug = ?err,
                "failed to evaluate setup initialization status"
            );
            json_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "database_error",
                "failed to check setup status",
            )
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        extract_token, preview_query_token, preview_session_id_from_path, sanitized_request_uri,
    };
    use axum::{body::Body, http::Request};

    #[test]
    fn request_log_uri_never_contains_query_credentials() {
        let uri = "/ws/system-events?token=eyJ.secret.signature&api_key=private"
            .parse()
            .expect("valid URI");
        let sanitized = sanitized_request_uri(&uri);

        assert_eq!(sanitized, "/ws/system-events");
        assert!(!sanitized.contains("token"));
        assert!(!sanitized.contains("secret"));
    }

    #[test]
    fn websocket_subprotocol_carries_token_without_putting_it_in_url() {
        let request = Request::builder()
            .uri("/ws/system-events")
            .header("sec-websocket-protocol", "aos-auth, eyJ.header.signature")
            .body(Body::empty())
            .expect("request");

        assert_eq!(
            extract_token(&request).as_deref(),
            Some("eyJ.header.signature")
        );
        assert!(request.uri().query().is_none());
    }

    #[test]
    fn account_token_in_query_is_not_accepted() {
        let request = Request::builder()
            .uri("/ws/system-events?token=eyJ.leaked.signature")
            .body(Body::empty())
            .expect("request");

        assert!(extract_token(&request).is_none());
    }

    #[test]
    fn scoped_preview_token_is_only_parsed_on_matching_proxy_path() {
        let request = Request::builder()
            .uri("/api/v1/rd/preview-sessions/rdprev-1/proxy/assets/app.js?preview_token=scoped")
            .body(Body::empty())
            .expect("request");
        assert_eq!(
            preview_query_token(&request),
            Some(("rdprev-1".to_string(), "scoped".to_string()))
        );

        let other = Request::builder()
            .uri("/api/v1/users?preview_token=scoped")
            .body(Body::empty())
            .expect("request");
        assert!(preview_query_token(&other).is_none());
        assert!(preview_session_id_from_path("/api/v1/rd/preview-sessions/x/stop").is_none());
    }
}
