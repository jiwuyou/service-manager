use axum::{
    extract::{Request, State},
    http::{
        HeaderValue,
        header::{AUTHORIZATION, WWW_AUTHENTICATE},
    },
    middleware::Next,
    response::{IntoResponse, Response},
};

use crate::{error::AppError, server::AppState};

pub async fn require_bearer_token(
    State(state): State<AppState>,
    req: Request,
    next: Next,
) -> Response {
    let expected = state.config.auth_token.clone();
    if expected.is_empty() {
        return unauthorized();
    }

    let got = bearer_token(req.headers());
    match got {
        Some(tok) if constant_time_eq(&tok, &expected) => next.run(req).await,
        _ => unauthorized(),
    }
}

fn unauthorized() -> Response {
    let mut res = AppError::Unauthorized.into_response();
    res.headers_mut().insert(
        WWW_AUTHENTICATE,
        HeaderValue::from_static("Bearer realm=\"service-manager\""),
    );
    res
}

fn bearer_token(headers: &axum::http::HeaderMap) -> Option<String> {
    let h = headers.get(AUTHORIZATION)?.to_str().ok()?.trim();
    if h.is_empty() {
        return None;
    }
    const PREFIX: &str = "Bearer ";
    if h.len() < PREFIX.len() {
        return None;
    }
    if !h[..PREFIX.len()].eq_ignore_ascii_case(PREFIX) {
        return None;
    }
    let tok = h[PREFIX.len()..].trim();
    if tok.is_empty() {
        return None;
    }
    Some(tok.to_string())
}

fn constant_time_eq(a: &str, b: &str) -> bool {
    // Constant-time for equal-length strings.
    if a.len() != b.len() {
        return false;
    }
    let mut diff: u8 = 0;
    for (&x, &y) in a.as_bytes().iter().zip(b.as_bytes().iter()) {
        diff |= x ^ y;
    }
    diff == 0
}
