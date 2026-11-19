//! Admin authentication: one password (GC_ADMIN_PASSWORD). The UI logs in and gets
//! an HttpOnly session cookie; scripts send `Authorization: Bearer <password>`.

use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::Json;
use axum::extract::{Request, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use dashmap::DashMap;
use rand::RngCore;
use serde::{Deserialize, Serialize};
use subtle::ConstantTimeEq;

use crate::error::{ApiError, ApiResult};
use crate::state::AppState;

pub const COOKIE: &str = "gc_session";
const SESSION_TTL: Duration = Duration::from_secs(14 * 24 * 3600);

pub struct Auth {
    password: Option<String>,
    sessions: DashMap<String, Instant>,
}

impl Auth {
    /// `password: None` disables authentication entirely.
    pub fn new(password: Option<String>) -> Arc<Self> {
        Arc::new(Self {
            password,
            sessions: DashMap::new(),
        })
    }

    pub fn enabled(&self) -> bool {
        self.password.is_some()
    }

    fn password_ok(&self, candidate: &str) -> bool {
        match &self.password {
            Some(p) => bool::from(p.as_bytes().ct_eq(candidate.as_bytes())),
            None => true,
        }
    }

    fn new_session(&self) -> String {
        let mut b = [0u8; 32];
        rand::rng().fill_bytes(&mut b);
        let token = hex::encode(b);
        self.sessions.retain(|_, exp| *exp > Instant::now());
        self.sessions
            .insert(token.clone(), Instant::now() + SESSION_TTL);
        token
    }

    fn session_ok(&self, token: &str) -> bool {
        self.sessions
            .get(token)
            .is_some_and(|exp| *exp > Instant::now())
    }

    pub fn is_authenticated(&self, headers: &HeaderMap) -> bool {
        if !self.enabled() {
            return true;
        }
        if let Some(token) = bearer(headers) {
            return self.password_ok(token);
        }
        cookie(headers, COOKIE).is_some_and(|t| self.session_ok(t))
    }
}

fn bearer(headers: &HeaderMap) -> Option<&str> {
    headers
        .get(header::AUTHORIZATION)?
        .to_str()
        .ok()?
        .strip_prefix("Bearer ")
        .map(str::trim)
}

fn cookie<'a>(headers: &'a HeaderMap, name: &str) -> Option<&'a str> {
    headers
        .get_all(header::COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(';'))
        .filter_map(|kv| kv.trim().split_once('='))
        .find(|(k, _)| *k == name)
        .map(|(_, v)| v)
}

pub async fn require_auth(
    State(state): State<Arc<AppState>>,
    req: Request,
    next: Next,
) -> Response {
    if state.auth.is_authenticated(req.headers()) {
        next.run(req).await
    } else {
        ApiError::unauthorized().into_response()
    }
}

#[derive(Deserialize)]
pub struct LoginBody {
    password: String,
}

pub async fn login(
    State(state): State<Arc<AppState>>,
    Json(body): Json<LoginBody>,
) -> ApiResult<Response> {
    if !state.auth.enabled() {
        return Ok(StatusCode::NO_CONTENT.into_response());
    }
    if !state.auth.password_ok(&body.password) {
        // Slow down brute force a little.
        tokio::time::sleep(Duration::from_millis(500)).await;
        return Err(ApiError::new(StatusCode::UNAUTHORIZED, "invalid password"));
    }
    let token = state.auth.new_session();
    let cookie = format!(
        "{COOKIE}={token}; HttpOnly; SameSite=Strict; Path=/; Max-Age={}",
        SESSION_TTL.as_secs()
    );
    let mut resp = StatusCode::NO_CONTENT.into_response();
    resp.headers_mut().insert(
        header::SET_COOKIE,
        HeaderValue::from_str(&cookie).expect("ascii cookie"),
    );
    Ok(resp)
}

pub async fn logout(State(state): State<Arc<AppState>>, headers: HeaderMap) -> Response {
    if let Some(t) = cookie(&headers, COOKIE) {
        state.auth.sessions.remove(t);
    }
    let mut resp = StatusCode::NO_CONTENT.into_response();
    resp.headers_mut().insert(
        header::SET_COOKIE,
        HeaderValue::from_static("gc_session=; HttpOnly; SameSite=Strict; Path=/; Max-Age=0"),
    );
    resp
}

#[derive(Serialize)]
pub struct AuthStatusBody {
    authenticated: bool,
    auth_enabled: bool,
}

pub async fn status(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> Json<AuthStatusBody> {
    Json(AuthStatusBody {
        authenticated: state.auth.is_authenticated(&headers),
        auth_enabled: state.auth.enabled(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn headers(pairs: &[(&'static str, &str)]) -> HeaderMap {
        let mut h = HeaderMap::new();
        for (k, v) in pairs {
            h.append(*k, HeaderValue::from_str(v).unwrap());
        }
        h
    }

    #[test]
    fn bearer_and_cookie() {
        let auth = Auth::new(Some("secret".into()));
        assert!(!auth.is_authenticated(&HeaderMap::new()));
        assert!(auth.is_authenticated(&headers(&[("authorization", "Bearer secret")])));
        assert!(!auth.is_authenticated(&headers(&[("authorization", "Bearer nope")])));
        let token = auth.new_session();
        let c = format!("a=b; {COOKIE}={token}");
        assert!(auth.is_authenticated(&headers(&[("cookie", &c)])));
        assert!(!auth.is_authenticated(&headers(&[("cookie", "gc_session=forged")])));
    }

    #[test]
    fn disabled() {
        let auth = Auth::new(None);
        assert!(auth.is_authenticated(&HeaderMap::new()));
        assert!(!auth.enabled());
    }
}
