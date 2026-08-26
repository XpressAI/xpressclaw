use std::net::{IpAddr, SocketAddr};

use axum::extract::{ConnectInfo, DefaultBodyLimit, State};
use axum::http::header::{CACHE_CONTROL, RETRY_AFTER, SET_COOKIE};
use axum::http::{HeaderMap, HeaderValue, Method, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use serde::{Deserialize, Serialize};
use zeroize::Zeroizing;

use crate::auth::{
    cookie_value, expired_session_cookie, session_cookie, DesktopCredentialError, LoginError,
    CSRF_HEADER,
};
use crate::state::AppState;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/bootstrap", get(bootstrap))
        .route("/identity-proof", post(identity_proof))
        .route("/login", post(login))
        .route("/logout", post(logout))
        .route("/desktop-ticket", post(desktop_ticket))
        .route("/exchange", post(exchange_ticket))
        // Credential-bearing payloads are deliberately tiny. Keep this bound
        // independent of the larger attachment limit on the main API router.
        .layer(DefaultBodyLimit::max(8 * 1024))
}

#[derive(Serialize)]
struct BootstrapResponse {
    instance_id: String,
    identity_public_key: String,
    authentication_enabled: bool,
    credential_kind: &'static str,
    authenticated: bool,
    csrf_token: Option<String>,
}

async fn bootstrap(State(state): State<AppState>, headers: HeaderMap) -> Response {
    let (authenticated, csrf_token) = if !state.auth.enabled() {
        (true, None)
    } else {
        let csrf = cookie_value(&headers).and_then(|cookie| state.auth.authenticate(cookie));
        (csrf.is_some(), csrf.map(|value| value.to_string()))
    };
    no_store(Json(BootstrapResponse {
        instance_id: state.auth.instance_id().to_string(),
        identity_public_key: state.auth.identity_public_key(),
        authentication_enabled: state.auth.enabled(),
        credential_kind: state.auth.credential_kind().as_str(),
        authenticated,
        csrf_token,
    }))
}

#[derive(Deserialize)]
struct IdentityProofRequest {
    challenge: String,
    #[serde(default)]
    client_public_key: Option<String>,
}

async fn identity_proof(
    State(state): State<AppState>,
    Json(body): Json<IdentityProofRequest>,
) -> Response {
    let Ok(challenge) = URL_SAFE_NO_PAD.decode(&body.challenge) else {
        return error_response(StatusCode::BAD_REQUEST, "Identity challenge is invalid");
    };
    if challenge.len() != 32 {
        return error_response(StatusCode::BAD_REQUEST, "Identity challenge is invalid");
    }
    if let Some(client_public_key) = body.client_public_key {
        let Ok(client_public_key) = URL_SAFE_NO_PAD.decode(client_public_key) else {
            return error_response(
                StatusCode::BAD_REQUEST,
                "Desktop credential channel key is invalid",
            );
        };
        let proof = match state
            .auth
            .begin_desktop_credential_exchange(&challenge, &client_public_key)
        {
            Ok(proof) => proof,
            Err(error) => return desktop_channel_error_response(error),
        };
        return no_store(Json(serde_json::json!({
            "instance_id": state.auth.instance_id(),
            "identity_public_key": state.auth.identity_public_key(),
            "exchange_id": proof.exchange_id,
            "server_public_key": proof.server_public_key,
            "signature": proof.signature,
        })));
    }
    no_store(Json(serde_json::json!({
        "instance_id": state.auth.instance_id(),
        "identity_public_key": state.auth.identity_public_key(),
        "signature": state.auth.sign_identity_challenge(&challenge),
    })))
}

#[derive(Deserialize)]
struct LoginRequest {
    credential: String,
}

async fn login(
    State(state): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Json(body): Json<LoginRequest>,
) -> Response {
    if body.credential.is_empty() || body.credential.len() > 4096 {
        return error_response(StatusCode::BAD_REQUEST, "Credential length is invalid");
    }
    if let Some(existing) = cookie_value(&headers) {
        state.auth.logout(existing);
    }
    match state
        .auth
        .login(
            Zeroizing::new(body.credential),
            login_client_ip(&headers, peer),
        )
        .await
    {
        Ok((session, csrf)) => session_response(
            &session,
            &csrf,
            request_uses_externally_terminated_https(&headers),
        ),
        Err(error) => login_error_response(error),
    }
}

async fn logout(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if !state.auth.enabled() {
        return cleared_cookie(StatusCode::NO_CONTENT.into_response());
    }
    let Some(cookie) = cookie_value(&headers) else {
        return unauthorized();
    };
    let Some(csrf) = headers
        .get(CSRF_HEADER)
        .and_then(|value| value.to_str().ok())
    else {
        return error_response(StatusCode::FORBIDDEN, "CSRF token required");
    };
    if !state.auth.verify_csrf(cookie, csrf) {
        return error_response(StatusCode::FORBIDDEN, "CSRF token is invalid or expired");
    }
    state.auth.logout(cookie);
    cleared_cookie(StatusCode::NO_CONTENT.into_response())
}

#[derive(Deserialize)]
struct DesktopTicketRequest {
    exchange_id: String,
    ciphertext: String,
}

async fn desktop_ticket(
    State(state): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Json(body): Json<DesktopTicketRequest>,
) -> Response {
    let channel = match state
        .auth
        .open_desktop_credential(&body.exchange_id, &body.ciphertext)
    {
        Ok(channel) => channel,
        Err(error) => return desktop_channel_error_response(error),
    };
    match state
        .auth
        .create_desktop_ticket(channel.credential.clone(), login_client_ip(&headers, peer))
        .await
    {
        Ok(ticket) => {
            let mut plaintext = Zeroizing::new(
                serde_json::to_vec(&serde_json::json!({
                    "ticket": ticket.as_str(),
                    "instance_id": state.auth.instance_id(),
                    "expires_in_seconds": 30,
                }))
                .expect("Desktop ticket response is serializable"),
            );
            let ciphertext = match state
                .auth
                .seal_desktop_credential_response(&channel, &mut plaintext)
            {
                Ok(ciphertext) => ciphertext,
                Err(error) => return desktop_channel_error_response(error),
            };
            no_store(Json(serde_json::json!({ "ciphertext": ciphertext })))
        }
        Err(error) => login_error_response(error),
    }
}

#[derive(Deserialize)]
struct ExchangeRequest {
    ticket: String,
}

async fn exchange_ticket(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<ExchangeRequest>,
) -> Response {
    if body.ticket.is_empty() || body.ticket.len() > 256 {
        return error_response(StatusCode::BAD_REQUEST, "Desktop ticket length is invalid");
    }
    match state.auth.exchange_desktop_ticket(&body.ticket) {
        Some((session, csrf)) => session_response(
            &session,
            &csrf,
            request_uses_externally_terminated_https(&headers),
        ),
        None => error_response(
            StatusCode::UNAUTHORIZED,
            "Desktop login ticket is invalid or expired",
        ),
    }
}

fn session_response(session: &str, csrf: &str, secure: bool) -> Response {
    let mut response = Json(serde_json::json!({
        "authenticated": true,
        "csrf_token": csrf,
    }))
    .into_response();
    response.headers_mut().insert(
        SET_COOKIE,
        HeaderValue::from_str(&session_cookie(session, secure)).expect("valid session cookie"),
    );
    response
        .headers_mut()
        .insert(CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response
}

/// Browser fetch supplies an Origin derived from the actual page. This lets a
/// TLS-terminating proxy receive Secure cookies without trusting forwarded
/// transport headers. A non-browser client can only influence its own cookie.
fn request_uses_externally_terminated_https(headers: &HeaderMap) -> bool {
    headers
        .get(axum::http::header::ORIGIN)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<axum::http::Uri>().ok())
        .and_then(|origin| origin.scheme_str().map(str::to_owned))
        .as_deref()
        == Some("https")
}

/// Use the directly connected peer for throttling unless it is a loopback
/// reverse proxy. A same-host proxy is inside the instance's host boundary and
/// may append or replace X-Forwarded-For; its final hop is the address it
/// actually observed. Forwarded identity from every non-loopback peer remains
/// untrusted and cannot be used to evade throttling.
fn login_client_ip(headers: &HeaderMap, peer: SocketAddr) -> IpAddr {
    if !peer.ip().is_loopback() {
        return peer.ip();
    }
    headers
        .get("x-forwarded-for")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.rsplit(',').next())
        .and_then(|value| value.trim().parse().ok())
        .unwrap_or_else(|| peer.ip())
}

fn login_error_response(error: LoginError) -> Response {
    match error {
        LoginError::Invalid => error_response(StatusCode::UNAUTHORIZED, "Invalid credential"),
        LoginError::Internal => error_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            "Could not create a secure login session",
        ),
        LoginError::RateLimited {
            retry_after_seconds,
        } => {
            let mut response = error_response(
                StatusCode::TOO_MANY_REQUESTS,
                "Too many login attempts; try again shortly",
            );
            response.headers_mut().insert(
                RETRY_AFTER,
                HeaderValue::from_str(&retry_after_seconds.to_string()).unwrap(),
            );
            response
        }
        LoginError::RestartRequired => error_response(
            StatusCode::SERVICE_UNAVAILABLE,
            "Authentication was changed; restart XpressClaw to generate a new login token",
        ),
        LoginError::Disabled => error_response(
            StatusCode::CONFLICT,
            "Authentication is disabled for this running instance",
        ),
    }
}

fn desktop_channel_error_response(error: DesktopCredentialError) -> Response {
    match error {
        DesktopCredentialError::Invalid => error_response(
            StatusCode::BAD_REQUEST,
            "Desktop credential channel is invalid or expired",
        ),
        DesktopCredentialError::Internal => error_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            "Could not create a secure Desktop credential channel",
        ),
    }
}

pub(crate) fn unauthorized() -> Response {
    cleared_cookie(error_response(
        StatusCode::UNAUTHORIZED,
        "Authentication required",
    ))
}

pub(crate) fn error_response(status: StatusCode, message: &str) -> Response {
    let mut response = (status, Json(serde_json::json!({ "error": message }))).into_response();
    response
        .headers_mut()
        .insert(CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response
}

fn cleared_cookie(mut response: Response) -> Response {
    response.headers_mut().insert(
        SET_COOKIE,
        HeaderValue::from_str(&expired_session_cookie()).expect("valid expired cookie"),
    );
    response
}

fn no_store<T: IntoResponse>(value: T) -> Response {
    let mut response = value.into_response();
    response
        .headers_mut()
        .insert(CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response
}

pub(crate) fn is_safe_method(method: &Method) -> bool {
    matches!(*method, Method::GET | Method::HEAD | Method::OPTIONS)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn socket(ip: &str) -> SocketAddr {
        SocketAddr::new(ip.parse().unwrap(), 43123)
    }

    #[test]
    fn secure_cookie_detection_uses_browser_origin_not_forwarded_headers() {
        let mut headers = HeaderMap::new();
        headers.insert("x-forwarded-proto", HeaderValue::from_static("https"));
        assert!(!request_uses_externally_terminated_https(&headers));

        headers.insert(
            axum::http::header::ORIGIN,
            HeaderValue::from_static("https://xpressclaw.example"),
        );
        assert!(request_uses_externally_terminated_https(&headers));
    }

    #[test]
    fn login_throttling_uses_only_a_loopback_proxys_observed_client() {
        let mut headers = HeaderMap::new();
        headers.insert(
            "x-forwarded-for",
            HeaderValue::from_static("203.0.113.7, 198.51.100.22"),
        );

        assert_eq!(
            login_client_ip(&headers, socket("127.0.0.1")),
            "198.51.100.22".parse::<IpAddr>().unwrap()
        );
        assert_eq!(
            login_client_ip(&headers, socket("192.0.2.44")),
            "192.0.2.44".parse::<IpAddr>().unwrap()
        );

        headers.insert("x-forwarded-for", HeaderValue::from_static("not-an-ip"));
        assert_eq!(
            login_client_ip(&headers, socket("::1")),
            "::1".parse::<IpAddr>().unwrap()
        );
    }

    #[tokio::test]
    async fn proxied_login_failures_do_not_lock_out_another_client() {
        let root = tempfile::tempdir().unwrap();
        let auth = crate::auth::InstanceAuth::load(
            root.path(),
            "instance".into(),
            true,
            Some(Zeroizing::new("expected".into())),
        )
        .unwrap();
        let proxy = socket("127.0.0.1");
        let mut attacker = HeaderMap::new();
        attacker.insert("x-forwarded-for", HeaderValue::from_static("198.51.100.31"));
        let mut operator = HeaderMap::new();
        operator.insert("x-forwarded-for", HeaderValue::from_static("198.51.100.32"));

        for _ in 0..5 {
            assert_eq!(
                auth.login(
                    Zeroizing::new("wrong".into()),
                    login_client_ip(&attacker, proxy),
                )
                .await,
                Err(crate::auth::LoginError::Invalid)
            );
        }
        assert!(auth
            .login(
                Zeroizing::new("expected".into()),
                login_client_ip(&operator, proxy),
            )
            .await
            .is_ok());
    }
}
