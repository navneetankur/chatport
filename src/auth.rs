//! Optional HTTP Basic auth. Enabled when `[auth]` is set in config.
//! Plain HTTP on a LAN: keeps casual visitors out, not a sniffer.

use axum::extract::{Request, State};
use axum::http::{HeaderValue, StatusCode, header};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use base64::Engine;
use std::sync::Arc;

/// Expected `Authorization` header value, precomputed at startup.
#[derive(Clone)]
pub struct Expected(Arc<str>);

impl Expected {
    pub fn new(user: &str, password: &str) -> Self {
        let token = base64::engine::general_purpose::STANDARD.encode(format!("{user}:{password}"));
        return Self(format!("Basic {token}").into());
    }

    fn matches(&self, header: Option<&HeaderValue>) -> bool {
        let Some(got) = header.and_then(|h| h.to_str().ok()) else {
            return false;
        };
        // Scheme is case-insensitive per RFC 7617.
        let (scheme, rest) = got.split_once(' ').unwrap_or((got, ""));
        let (_, want) = self.0.split_once(' ').unwrap_or_default();
        return scheme.eq_ignore_ascii_case("basic") && rest.trim() == want;
    }
}

pub async fn require(State(exp): State<Expected>, req: Request, next: Next) -> Response {
    if exp.matches(req.headers().get(header::AUTHORIZATION)) {
        return next.run(req).await;
    }
    tracing::debug!("auth rejected: {} {}", req.method(), req.uri().path());
    return (
        StatusCode::UNAUTHORIZED,
        [(header::WWW_AUTHENTICATE, r#"Basic realm="chatport""#)],
    )
        .into_response();
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hv(s: &str) -> HeaderValue {
        HeaderValue::from_str(s).unwrap()
    }

    #[test]
    fn matches_only_right_credentials() {
        let e = Expected::new("me", "pw");
        // base64("me:pw") = "bWU6cHc="
        assert!(e.matches(Some(&hv("Basic bWU6cHc="))));
        assert!(e.matches(Some(&hv("basic bWU6cHc="))));
        assert!(!e.matches(Some(&hv("Basic bWU6eHg="))));
        assert!(!e.matches(Some(&hv("Bearer bWU6cHc="))));
        assert!(!e.matches(None));
    }
}
