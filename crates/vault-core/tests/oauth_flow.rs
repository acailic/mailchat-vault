//! OAuth2 login-flow tests against the in-process mock OAuth server
//! (`oauth_mock`). These exercise the real protocol path — PKCE
//! authorization URL, loopback redirect listener with state validation,
//! single-use code exchange with the verifier, refresh grants — with no
//! network access and no real Google credentials.

mod oauth_mock;

use std::time::Duration;

use base64::Engine;
use oauth_mock::{browser_visit, MockOAuth};
use sha2::Digest;
use vault_core::oauth::{
    self, begin_login, refresh_access_token, run_login, OAuthConfig, ENV_CLIENT_ID,
    ENV_CLIENT_SECRET, LOGIN_TIMEOUT,
};

const CLIENT_ID: &str = "test-client.apps.googleusercontent.com";
const CLIENT_SECRET: &str = "test-secret-GOCSPX";
const USER: &str = "alice@example.com";

fn mock() -> MockOAuth {
    MockOAuth::start(CLIENT_ID, CLIENT_SECRET)
}

/// Run the whole sign-in against the mock: the "browser" callback visits
/// the authorization URL and follows the redirect to the loopback
/// listener, exactly like a real desktop sign-in.
fn login(server: &MockOAuth) -> oauth::TokenSet {
    let config = server.config();
    run_login(
        &config,
        Some(USER),
        |url| browser_visit(url).map(|_| ()),
        LOGIN_TIMEOUT,
    )
    .expect("login completes")
}

fn pair_value<'a>(pairs: &'a [(String, String)], key: &str) -> Option<&'a str> {
    pairs
        .iter()
        .find(|(k, _)| k == key)
        .map(|(_, v)| v.as_str())
}

#[test]
fn full_login_flow_returns_refreshable_tokens() {
    let server = mock();
    let tokens = login(&server);

    assert_eq!(tokens.access_token, "access-1");
    let refresh = tokens
        .refresh_token
        .clone()
        .expect("sign-in issues a refresh token");
    assert_eq!(refresh, "refresh-1");

    // The grant chain on the wire: one authorization, one token exchange.
    let auth = server.auth_requests();
    assert_eq!(auth.len(), 1, "exactly one authorization request");
    let token_requests = server.token_requests();
    assert_eq!(token_requests.len(), 1, "exactly one token request");

    // A refresh grant mints a new access token and keeps the original
    // refresh token (the endpoint does not rotate it).
    let refreshed = refresh_access_token(&server.config(), &refresh).unwrap();
    assert_eq!(refreshed.access_token, "access-2");
    assert_eq!(
        refreshed.refresh_token.as_deref(),
        Some(refresh.as_str()),
        "the refresh grant must not lose the stored refresh token"
    );
    assert_eq!(server.token_requests().len(), 2);
    assert_eq!(
        pair_value(&server.token_requests()[1], "grant_type"),
        Some("refresh_token")
    );
}

#[test]
fn authorization_url_carries_the_desktop_flow_parameters() {
    let server = mock();
    let handle = begin_login(&server.config(), Some(USER)).unwrap();

    let (base, query) = handle.auth_url.split_once('?').unwrap();
    assert!(base.ends_with("/auth"), "auth URL points at /auth: {base}");
    let pairs: Vec<(String, String)> = query
        .split('&')
        .map(|p| {
            p.split_once('=')
                .map(|(k, v)| (k.to_string(), percent_decode(v)))
        })
        .collect::<Option<_>>()
        .unwrap();
    let get = |key: &str| pair_value(&pairs, key).unwrap_or_else(|| panic!("missing {key}"));

    assert_eq!(get("client_id"), CLIENT_ID);
    assert_eq!(get("response_type"), "code");
    assert_eq!(get("scope"), "https://mail.google.com/");
    assert_eq!(get("code_challenge_method"), "S256");
    assert_eq!(get("access_type"), "offline");
    assert_eq!(get("login_hint"), USER);
    assert_eq!(get("redirect_uri"), handle.redirect_uri);
    assert!(
        handle.redirect_uri.starts_with("http://127.0.0.1:"),
        "redirect must be a literal loopback address: {}",
        handle.redirect_uri
    );
    assert_eq!(get("code_challenge").len(), 43, "S256 challenge shape");
    assert!(!get("state").is_empty());
    assert_eq!(
        pair_value(&pairs, "prompt"),
        Some("consent"),
        "consent is forced so repeat sign-ins still get a refresh token"
    );

    // Nothing was sent anywhere yet: no authorization hit the server.
    assert!(server.auth_requests().is_empty());
}

#[test]
fn code_exchange_sends_the_pkce_verifier_and_codes_are_single_use() {
    let server = mock();
    let tokens = login(&server);

    let exchange = &server.token_requests()[0];
    assert_eq!(
        pair_value(exchange, "grant_type"),
        Some("authorization_code")
    );
    assert_eq!(pair_value(exchange, "client_id"), Some(CLIENT_ID));
    assert_eq!(pair_value(exchange, "client_secret"), Some(CLIENT_SECRET));
    let verifier = pair_value(exchange, "code_verifier").expect("PKCE verifier sent");
    assert_eq!(verifier.len(), 43);

    // S256(verifier) equals the challenge from the authorization request.
    let auth_request = server.auth_requests();
    let challenge = pair_value(&auth_request[0], "code_challenge").unwrap();
    let digest = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .encode(sha2::Sha256::digest(verifier.as_bytes()));
    assert_eq!(digest, challenge);

    // The exchanged code matched the one the "consent screen" issued.
    let code = pair_value(exchange, "code").unwrap();
    assert!(code.starts_with("authcode-"), "code from the mock: {code}");

    // Replaying the same code (with the same verifier and redirect) fails:
    // codes are single-use. Grab the redirect_uri the app registered.
    let redirect = pair_value(exchange, "redirect_uri").unwrap().to_string();
    let replay = oauth::exchange_code(&server.config(), code, &redirect, verifier);
    assert!(replay.is_err(), "replayed code must be rejected");
    assert!(replay.unwrap_err().to_string().contains("invalid_grant"));

    // The tokens are unaffected and still refreshable.
    let refresh = tokens.refresh_token.unwrap();
    assert!(refresh_access_token(&server.config(), &refresh).is_ok());
}

#[test]
fn a_redirect_with_a_foreign_state_is_rejected_without_exchanging() {
    let server = mock();
    let mut handle = begin_login(&server.config(), None).unwrap();

    // A "browser" hits the loopback listener with the right shape but the
    // state of another sign-in attempt. The listener only accepts inside
    // wait_for_redirect, so drive both concurrently.
    let attack = format!(
        "{}/?code=authcode-attacker&state=someone-elses",
        handle.redirect_uri
    );
    let browser = std::thread::spawn(move || browser_visit(&attack));
    let result = handle.wait_for_redirect(Duration::from_secs(5));
    let page = browser
        .join()
        .expect("browser thread")
        .expect("the listener still answers the browser");
    assert!(
        page.contains("did not match"),
        "the browser-facing page explains the mismatch: {page}"
    );
    let error = result.expect_err("a foreign state must fail the login");
    assert!(error.to_string().contains("state mismatch"));

    // And critically: no code ever reached the token endpoint.
    assert!(server.token_requests().is_empty());
}

#[test]
fn denied_consent_surfaces_the_error_from_the_redirect() {
    let server = mock();
    server.deny_consent();
    let error = run_login(
        &server.config(),
        Some(USER),
        |url| browser_visit(url).map(|_| ()),
        Duration::from_secs(10),
    )
    .expect_err("denied consent must fail the login");
    assert!(
        error.to_string().contains("access_denied"),
        "error should name the refusal: {error}"
    );
    // No token request happened either.
    assert!(server.token_requests().is_empty());
}

#[test]
fn a_failed_browser_open_reports_the_url_to_open_manually() {
    let server = mock();
    let error = run_login(
        &server.config(),
        Some(USER),
        |_url| Err("no display".to_string()),
        Duration::from_secs(10),
    )
    .expect_err("an unopenable browser must fail the login");
    let message = error.to_string();
    assert!(message.contains("no display"), "{message}");
    assert!(
        message.contains(server.base_url.as_str()),
        "the error carries the authorization URL for manual opening: {message}"
    );
    assert!(server.auth_requests().is_empty());
}

#[test]
fn waiting_for_a_redirect_that_never_comes_times_out() {
    let server = mock();
    let mut handle = begin_login(&server.config(), None).unwrap();
    let start = std::time::Instant::now();
    let error = handle
        .wait_for_redirect(Duration::from_millis(300))
        .expect_err("no redirect must time out");
    assert!(error.to_string().contains("timed out"));
    assert!(
        start.elapsed() < Duration::from_secs(5),
        "the timeout must engage promptly, took {:?}",
        start.elapsed()
    );
}

#[test]
fn a_stale_refresh_token_is_a_clear_error() {
    let server = mock();
    let _tokens = login(&server);
    server.revoke_refresh();
    let error = refresh_access_token(&server.config(), "refresh-1")
        .expect_err("revoked refresh tokens must fail");
    assert!(error.to_string().contains("invalid_grant"));
}

#[test]
fn wrong_client_credentials_are_refused_by_the_token_endpoint() {
    let server = mock();
    let mut config = server.config();
    config.client_secret = "the-wrong-secret".to_string();
    let error = run_login(
        &config,
        Some(USER),
        |url| browser_visit(url).map(|_| ()),
        Duration::from_secs(10),
    )
    .expect_err("the wrong secret must fail at the token endpoint");
    // The browser round trip succeeded; the exchange did not.
    assert!(error.to_string().contains("invalid_client"), "{error}");
    // The authorization used the *right* id (the URL came from the
    // server's own config before the override).
    assert_eq!(server.auth_requests().len(), 1);
}

/// Minimal percent-decoding for assertion-side URL parsing.
fn percent_decode(input: &str) -> String {
    let bytes = input.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            let hex = |b: u8| (b as char).to_digit(16);
            if let (Some(hi), Some(lo)) = (hex(bytes[i + 1]), hex(bytes[i + 2])) {
                out.push(((hi << 4) | lo) as u8);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// The env contract from the README: both credentials or neither.
#[test]
fn env_configuration_enables_or_disables_the_flow() {
    let unset = OAuthConfig::from_lookup(|_| None).unwrap();
    assert!(unset.is_none(), "no env vars -> Gmail sign-in disabled");

    let only_id =
        OAuthConfig::from_lookup(|name| (name == ENV_CLIENT_ID).then_some(CLIENT_ID.to_string()))
            .unwrap_err();
    assert!(only_id.to_string().contains(ENV_CLIENT_SECRET));

    let only_secret = OAuthConfig::from_lookup(|name| {
        (name == ENV_CLIENT_SECRET).then_some(CLIENT_SECRET.to_string())
    })
    .unwrap_err();
    assert!(only_secret.to_string().contains(ENV_CLIENT_ID));
}
