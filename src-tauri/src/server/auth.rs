use axum::{
    extract::State,
    http::{header::AUTHORIZATION, HeaderMap, Request, StatusCode},
    middleware::Next,
    response::{IntoResponse, Response},
    Json,
};
use serde_json::{json, Value};
use super::state::HttpState;

pub async fn require_api_key(
    State(state): State<HttpState>,
    request: Request<axum::body::Body>,
    next: Next,
) -> Response {
    let path = request.uri().path().to_string();
    let require_auth = state.config.require_auth;
    if !require_auth {
        return next.run(request).await;
    }

    let Some(presented) = presented_key(request.headers()) else {
        return unauthorized_response(&path);
    };

    if state.keys.lock().await.verify(&presented) {
        next.run(request).await
    } else {
        unauthorized_response(&path)
    }
}

pub fn presented_key(headers: &HeaderMap) -> Option<String> {
    bearer_key(headers).or_else(|| header_value(headers, "x-api-key"))
}

pub fn unauthorized_json(path: &str) -> Value {
    if path.starts_with("/v1/messages") {
        json!({
            "type": "error",
            "error": {
                "type": "authentication_error",
                "message": "invalid or missing proxy API key"
            }
        })
    } else {
        json!({
            "error": {
                "message": "invalid or missing proxy API key",
                "type": "invalid_request_error",
                "code": "invalid_api_key"
            }
        })
    }
}

fn unauthorized_response(path: &str) -> Response {
    (StatusCode::UNAUTHORIZED, Json(unauthorized_json(path))).into_response()
}

fn bearer_key(headers: &HeaderMap) -> Option<String> {
    let value = headers.get(AUTHORIZATION)?.to_str().ok()?.trim();
    let (scheme, token) = value.split_once(' ')?;
    if !scheme.eq_ignore_ascii_case("bearer") {
        return None;
    }
    let token = token.trim();
    (!token.is_empty()).then(|| token.to_owned())
}

fn header_value(headers: &HeaderMap, name: &str) -> Option<String> {
    headers
        .get(name)?
        .to_str()
        .ok()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
}
