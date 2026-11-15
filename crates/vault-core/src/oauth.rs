//! Google OAuth2 (XOAUTH2) login for Gmail IMAP.
//!
//! The second way to authenticate the IMAP sync (next to the app
//! password): a standard desktop OAuth2 authorization-code flow with PKCE
//! (RFC 7636) and a loopback redirect (RFC 8252):
//!
//! 1. [`begin_login`] binds a temporary listener on `127.0.0.1` and builds
//!    the Google authorization URL (scopes: full IMAP access, offline use).
//! 2. The app opens that URL in the user's browser ([`run_login`] takes an
//!    `open_browser` callback; the Tauri shell uses the system browser).
//! 3. Google redirects to `http://127.0.0.1:{port}/?code=…&state=…`; the
//!    listener validates the state, answers with a "you may close this
//!    tab" page, and hands back the authorization code
//!    ([`LoginHandle::wait_for_redirect`]).
//! 4. [`exchange_code`] trades code + PKCE verifier + client credentials
//!    for an access token and a refresh token at the token endpoint.
//!
//! Syncs later use [`refresh_access_token`] to mint short-lived access
//! tokens, and [`xoauth2_sasl`] builds the SASL string the IMAP client
//! sends for `AUTHENTICATE XOAUTH2` (see [`crate::imap::ImapAuth`]).
//!
//! # Secret handling
//!
//! The refresh token is the long-lived secret. It never enters the vault
//! database (the ingest-side no-persist tests byte-scan storage for it);
//! the Tauri shell keeps it in the OS keyring (service `mailchat-vault`,
//! entry `oauth:user@host`) with the same session-memory fallback as
//! passwords — see `src-tauri/src/secrets.rs`. Access tokens are
//! short-lived (≈1 h) and live only in process memory. [`TokenSet`]'s
//! `Debug` output redacts both.
//!
//! # Client credentials
//!
//! Google requires every user to register their own OAuth client
//! ("Desktop app" type) in Google Cloud Console; the client id/secret are
//! configured through the environment ([`OAuthConfig::from_env`]) and are
//! never stored by the app. See the README ("IMAP sync → Gmail (Google
//! sign-in)") for the registration walkthrough. This BYO-client design is
//! deliberate: personal-use clients (fewer than 100 users) are exempt
//! from Google's OAuth verification, so the app itself never needs a
//! verified public client — the flow stays labeled advanced setup. See
//! `docs/evidence/gmail-oauth-scope-2026-11-15.md` for the scope and
//! verification analysis (issue #82).
//!
//! # Testing
//!
//! The flow is fully hermetic: tests point the endpoints at an in-process
//! mock OAuth server (`tests/oauth_mock`) and drive the loopback redirect
//! with an HTTP client standing in for the browser — no network, no real
//! Google credentials. `tests/imap/oauth_sync.rs` runs the whole chain
//! (mock OAuth → token → mock IMAP `AUTHENTICATE XOAUTH2` → archive).

use std::fmt;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::thread;
use std::time::Duration;

use base64::engine::general_purpose::URL_SAFE_NO_PAD as BASE64_URL;
use base64::Engine;
use serde::Deserialize;
use sha2::{Digest, Sha256};
use url::Url;

use crate::{Error, Result};

/// Google's authorization endpoint (consent screen).
pub const GMAIL_AUTH_URL: &str = "https://accounts.google.com/o/oauth2/v2/auth";
/// Google's token endpoint (code exchange + refresh).
pub const GMAIL_TOKEN_URL: &str = "https://oauth2.googleapis.com/token";
/// The scope Gmail requires for IMAP `AUTHENTICATE XOAUTH2`: **full mail
/// access** — one of Google's *restricted* scopes. This is not the vault's
/// choice to narrow: Google's XOAUTH2 documentation states the scope for
/// IMAP/POP/SMTP access is exactly this URL (a token granted any narrower
/// scope is refused at SASL auth — the error challenge names this scope
/// back), and that apps which do not need it must migrate to the Gmail API
/// for granular scopes. The vault only ever issues read-only IMAP commands
/// (`EXAMINE`, `UID SEARCH`, `UID FETCH` — see [`crate::imap`]), but
/// read-only *commands* do not reduce the OAuth *permission*: the grant
/// still covers read-write mail access. Least privilege therefore cannot
/// be reached on the IMAP path — the narrower-scope alternative is a Gmail
/// API connector with `gmail.readonly`, a separate project (issue #82);
/// until then this flow stays labeled advanced setup, BYO client only.
/// Analysis: `docs/evidence/gmail-oauth-scope-2026-11-15.md`.
pub const GMAIL_SCOPE: &str = "https://mail.google.com/";

/// Environment variable carrying the user's own Google OAuth client id.
pub const ENV_CLIENT_ID: &str = "MAILCHAT_GMAIL_CLIENT_ID";
/// Environment variable carrying the user's own Google OAuth client secret.
pub const ENV_CLIENT_SECRET: &str = "MAILCHAT_GMAIL_CLIENT_SECRET";

/// How long a token-endpoint HTTP request may take.
const TOKEN_TIMEOUT: Duration = Duration::from_secs(30);
/// How long the loopback listener waits for the browser's redirect.
pub const LOGIN_TIMEOUT: Duration = Duration::from_secs(5 * 60);
/// Read budget for one HTTP request line block on the loopback listener.
const REDIRECT_READ_TIMEOUT: Duration = Duration::from_secs(10);
/// Poll interval of the loopback accept loop.
const ACCEPT_POLL: Duration = Duration::from_millis(50);
/// Client credentials + endpoints for the OAuth2 flow. Gmail defaults via
/// [`OAuthConfig::gmail`]; tests point the endpoints at a mock server.
#[derive(Clone)]
pub struct OAuthConfig {
    /// The user's registered OAuth client id (env-configured).
    pub client_id: String,
    /// The client secret ("Desktop app" clients ship one; Google treats it
    /// as an identifier, not a true secret).
    pub client_secret: String,
    /// Authorization endpoint.
    pub auth_url: String,
    /// Token endpoint.
    pub token_url: String,
    /// Space-separated scopes requested.
    pub scope: String,
}

impl OAuthConfig {
    /// Gmail preset: Google endpoints and the IMAP scope.
    pub fn gmail(client_id: impl Into<String>, client_secret: impl Into<String>) -> Self {
        Self {
            client_id: client_id.into(),
            client_secret: client_secret.into(),
            auth_url: GMAIL_AUTH_URL.to_string(),
            token_url: GMAIL_TOKEN_URL.to_string(),
            scope: GMAIL_SCOPE.to_string(),
        }
    }

    /// Read the client credentials from the process environment.
    /// `Ok(None)` when neither variable is set (Gmail sign-in disabled),
    /// `Err` when only one of the two is set.
    pub fn from_env() -> Result<Option<Self>> {
        Self::from_lookup(|name| std::env::var(name).ok())
    }

    /// Pure variant of [`Self::from_env`] for tests.
    pub fn from_lookup(lookup: impl Fn(&str) -> Option<String>) -> Result<Option<Self>> {
        let value = |name: &str| {
            lookup(name)
                .map(|v| v.trim().to_string())
                .filter(|v| !v.is_empty())
        };
        let client_id = value(ENV_CLIENT_ID);
        let client_secret = value(ENV_CLIENT_SECRET);
        match (client_id, client_secret) {
            (Some(id), Some(secret)) => Ok(Some(Self::gmail(id, secret))),
            (None, None) => Ok(None),
            (Some(_), None) => Err(Error::OAuth(format!(
                "{ENV_CLIENT_ID} is set but {ENV_CLIENT_SECRET} is not — set both to enable \
                 Gmail sign-in, or unset {ENV_CLIENT_ID}"
            ))),
            (None, Some(_)) => Err(Error::OAuth(format!(
                "{ENV_CLIENT_SECRET} is set but {ENV_CLIENT_ID} is not — set both to enable \
                 Gmail sign-in, or unset {ENV_CLIENT_SECRET}"
            ))),
        }
    }

    /// Host of the token endpoint, for error messages.
    fn token_host(&self) -> String {
        Url::parse(&self.token_url)
            .ok()
            .and_then(|u| u.host_str().map(|h| h.to_string()))
            .unwrap_or_else(|| self.token_url.clone())
    }
}

impl fmt::Debug for OAuthConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("OAuthConfig")
            .field("client_id", &self.client_id)
            .field("client_secret", &"<redacted>")
            .field("auth_url", &self.auth_url)
            .field("token_url", &self.token_url)
            .field("scope", &self.scope)
            .finish()
    }
}

/// Tokens from one grant: a short-lived access token and (on the
/// authorization-code grant) the long-lived refresh token.
#[derive(Clone)]
pub struct TokenSet {
    /// Bearer token for `AUTHENTICATE XOAUTH2` (≈1 h lifetime).
    pub access_token: String,
    /// Present after sign-in; minted into new access tokens via
    /// [`refresh_access_token`].
    pub refresh_token: Option<String>,
}

impl fmt::Debug for TokenSet {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TokenSet")
            .field("access_token", &"<redacted>")
            .field(
                "refresh_token",
                &self.refresh_token.as_ref().map(|_| "<redacted>"),
            )
            .finish()
    }
}

/// PKCE pair (RFC 7636): the verifier travels to the token endpoint, the
/// S256 challenge travels in the authorization URL.
struct Pkce {
    verifier: String,
    challenge: String,
}

impl Pkce {
    fn generate() -> Self {
        // 32 random bytes → 43-char base64url verifier (RFC 7636 §4.1).
        let mut bytes = [0u8; 32];
        getrandom::fill(&mut bytes)
            .map_err(|e| Error::OAuth(format!("no system randomness available: {e}")))
            .expect("system randomness is available on supported platforms");
        let verifier = BASE64_URL.encode(bytes);
        let challenge = BASE64_URL.encode(Sha256::digest(verifier.as_bytes()));
        Self {
            verifier,
            challenge,
        }
    }
}

/// A started login: the authorization URL to open in the user's browser,
/// plus the listener/verifier/state that complete it. Obtain via
/// [`begin_login`], then [`LoginHandle::wait_for_redirect`], then
/// [`exchange_code`] — or use the one-shot [`run_login`].
pub struct LoginHandle {
    listener: TcpListener,
    state: String,
    pkce: Pkce,
    /// The `redirect_uri` sent to Google (`http://127.0.0.1:{port}`).
    pub redirect_uri: String,
    /// The authorization URL: open this in the user's browser.
    pub auth_url: String,
}

/// Begin a login: bind the loopback listener, generate PKCE + state, and
/// build the authorization URL. `login_hint` (the Gmail address) lets
/// Google preselect the account. Nothing has been sent anywhere yet.
pub fn begin_login(config: &OAuthConfig, login_hint: Option<&str>) -> Result<LoginHandle> {
    // RFC 8252 loopback redirect: literal 127.0.0.1 (not "localhost", which
    // may resolve to ::1 and miss our listener), ephemeral port — Google
    // ignores the port for loopback redirect URIs.
    let listener = TcpListener::bind("127.0.0.1:0")
        .map_err(|e| Error::OAuth(format!("could not bind a loopback listener: {e}")))?;
    let port = listener
        .local_addr()
        .map_err(|e| Error::OAuth(format!("loopback listener has no address: {e}")))?
        .port();
    let redirect_uri = format!("http://127.0.0.1:{port}");
    let pkce = Pkce::generate();
    let mut state_bytes = [0u8; 16];
    getrandom::fill(&mut state_bytes)
        .map_err(|e| Error::OAuth(format!("no system randomness available: {e}")))?;
    let state = BASE64_URL.encode(state_bytes);

    let mut url = Url::parse(&config.auth_url)
        .map_err(|e| Error::OAuth(format!("invalid auth endpoint {}: {e}", config.auth_url)))?;
    {
        let mut pairs = url.query_pairs_mut();
        pairs.append_pair("client_id", &config.client_id);
        pairs.append_pair("redirect_uri", &redirect_uri);
        pairs.append_pair("response_type", "code");
        pairs.append_pair("scope", &config.scope);
        pairs.append_pair("state", &state);
        pairs.append_pair("code_challenge", &pkce.challenge);
        pairs.append_pair("code_challenge_method", "S256");
        // access_type=offline + prompt=consent guarantee a refresh token
        // even on repeat sign-ins (Google only returns one on first consent
        // otherwise).
        pairs.append_pair("access_type", "offline");
        pairs.append_pair("prompt", "consent");
        if let Some(hint) = login_hint {
            pairs.append_pair("login_hint", hint);
        }
    }

    Ok(LoginHandle {
        listener,
        state,
        pkce,
        redirect_uri,
        auth_url: url.to_string(),
    })
}

impl LoginHandle {
    /// Wait for the browser's redirect, up to `timeout`. Accepts exactly
    /// one connection, validates the state parameter, answers the browser
    /// with a close-this-tab page, and returns the authorization code.
    ///
    /// An `error=…` redirect (e.g. the user denied consent) is an `Err`.
    pub fn wait_for_redirect(&mut self, timeout: Duration) -> Result<String> {
        let deadline = std::time::Instant::now() + timeout;
        self.listener
            .set_nonblocking(true)
            .map_err(|e| Error::OAuth(format!("loopback listener broke: {e}")))?;
        let stream = loop {
            match self.listener.accept() {
                Ok((stream, _)) => break stream,
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    if std::time::Instant::now() >= deadline {
                        return Err(Error::OAuth(
                            "sign-in timed out waiting for the browser redirect".into(),
                        ));
                    }
                    thread::sleep(ACCEPT_POLL);
                }
                Err(e) => return Err(Error::OAuth(format!("loopback accept failed: {e}"))),
            }
        };
        stream
            .set_nonblocking(false)
            .map_err(|e| Error::OAuth(format!("loopback connection broke: {e}")))?;
        stream
            .set_read_timeout(Some(REDIRECT_READ_TIMEOUT))
            .map_err(|e| Error::OAuth(format!("loopback connection broke: {e}")))?;
        let mut stream = stream;

        // Read one request head (request line + headers) — the redirect is
        // a GET with no body; headers are ignored after the blank line.
        let mut buf = Vec::new();
        let mut byte = [0u8; 1];
        let head_end = b"\r\n\r\n";
        while !buf.ends_with(head_end) {
            match stream.read(&mut byte) {
                Ok(0) => break, // client hung up; parse what we have
                Ok(_) => buf.push(byte[0]),
                Err(e) => return Err(Error::OAuth(format!("unreadable redirect request: {e}"))),
            }
            if buf.len() > 8192 {
                return Err(Error::OAuth("redirect request too large".into()));
            }
        }
        let head = String::from_utf8_lossy(&buf);
        let request_line = head.lines().next().unwrap_or_default();
        let path = request_line.split(' ').nth(1).unwrap_or("");
        let query = path.split_once('?').map(|(_, q)| q).unwrap_or("");
        let params = form_pairs(query);

        // Always answer the browser, success or failure.
        let respond = |stream: &mut std::net::TcpStream, status: &str, body: &str| {
            let response = format!(
                "HTTP/1.1 {status}\r\nContent-Type: text/html; charset=utf-8\r\n\
                 Content-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            let _ = stream.write_all(response.as_bytes());
            let _ = stream.flush();
        };
        let page = |title: &str, message: &str| {
            format!(
                "<html><body style=\"font: 15px/1.5 system-ui\"><h2>mailchat-vault</h2>\
                 <p><b>{title}</b></p><p>{message}</p></body></html>"
            )
        };

        if let Some(error) = params
            .iter()
            .find(|(k, _)| k == "error")
            .map(|(_, v)| v.to_string())
        {
            let escaped = error
                .replace('&', "&amp;")
                .replace('<', "&lt;")
                .replace('>', "&gt;")
                .replace('"', "&quot;");
            respond(
                &mut stream,
                "400 Bad Request",
                &page(
                    "Sign-in was not completed.",
                    &format!("Google reported: {escaped}. You can close this tab and try again."),
                ),
            );
            return Err(Error::OAuth(format!("authorization was refused: {error}")));
        }
        let state = params
            .iter()
            .find(|(k, _)| k == "state")
            .map(|(_, v)| v.to_string());
        if state.as_deref() != Some(self.state.as_str()) {
            respond(
                &mut stream,
                "400 Bad Request",
                &page(
                    "Sign-in failed.",
                    "The redirect did not match this sign-in attempt. Close this tab and start \
                     the sign-in again from mailchat-vault.",
                ),
            );
            return Err(Error::OAuth(
                "state mismatch on the OAuth redirect — this redirect belongs to another \
                 sign-in attempt"
                    .into(),
            ));
        }
        let code = params
            .iter()
            .find(|(k, _)| k == "code")
            .map(|(_, v)| v.to_string());
        match code {
            Some(code) if !code.is_empty() => {
                respond(
                    &mut stream,
                    "200 OK",
                    &page(
                        "Sign-in complete.",
                        "You can close this tab and go back to mailchat-vault.",
                    ),
                );
                Ok(code)
            }
            _ => {
                respond(
                    &mut stream,
                    "400 Bad Request",
                    &page(
                        "Sign-in failed.",
                        "The redirect carried no authorization code. Close this tab and try \
                         again.",
                    ),
                );
                Err(Error::OAuth(
                    "the OAuth redirect carried no authorization code".into(),
                ))
            }
        }
    }
}

/// The one-shot login: begin, open the browser, wait for the redirect,
/// exchange the code. `open_browser` receives the authorization URL and
/// reports whether a browser could be launched; its result is surfaced as
/// an error (with the URL, so the user can open it manually) before the
/// flow starts waiting.
pub fn run_login(
    config: &OAuthConfig,
    login_hint: Option<&str>,
    open_browser: impl FnOnce(&str) -> std::result::Result<(), String>,
    timeout: Duration,
) -> Result<TokenSet> {
    let mut handle = begin_login(config, login_hint)?;
    let auth_url = handle.auth_url.clone();
    let redirect_uri = handle.redirect_uri.clone();
    let verifier = handle.pkce.verifier.clone();
    let waiter = thread::spawn(move || handle.wait_for_redirect(timeout));
    if let Err(e) = open_browser(&auth_url) {
        return Err(Error::OAuth(format!(
            "could not open a browser ({e}). Open this URL manually and finish the sign-in, \
             then start the sign-in again: {auth_url}"
        )));
    }
    let code = waiter
        .join()
        .map_err(|_| Error::OAuth("the sign-in listener panicked".into()))??;
    exchange_code(config, &code, &redirect_uri, &verifier)
}

/// One grant from the token endpoint.
#[derive(Debug, Deserialize)]
struct TokenResponse {
    access_token: String,
    #[serde(default)]
    refresh_token: Option<String>,
    token_type: String,
}

/// POST one form-encoded grant to the token endpoint and turn the JSON
/// answer into a [`TokenSet`]. Shared by the code exchange and the refresh.
fn token_request(config: &OAuthConfig, grant: Vec<(&str, &str)>) -> Result<TokenSet> {
    let host = config.token_host();
    let response = match ureq::post(&config.token_url)
        .timeout(TOKEN_TIMEOUT)
        .send_form(&grant)
    {
        Ok(response) => response,
        // A refused grant (bad client, bad code, revoked token) carries a
        // JSON body whose `error` field names the reason — surface that,
        // not just the status code.
        Err(ureq::Error::Status(_status, response)) => {
            let error = oauth_error_code(response);
            return Err(Error::OAuth(format!(
                "token endpoint at {host} rejected the request: {error}"
            )));
        }
        Err(e) => return Err(Error::OAuth(format!("token request to {host} failed: {e}"))),
    };
    let body: TokenResponse = response
        .into_json()
        .map_err(|e| Error::OAuth(format!("unreadable token response from {host}: {e}")))?;
    if !body.token_type.eq_ignore_ascii_case("bearer") || body.access_token.is_empty() {
        return Err(Error::OAuth(format!(
            "token endpoint at {host} returned a non-bearer grant"
        )));
    }
    Ok(TokenSet {
        access_token: body.access_token,
        refresh_token: body.refresh_token,
    })
}

/// The `error` field of an OAuth error response body, or a generic
/// fallback when the body is unreadable.
fn oauth_error_code(response: ureq::Response) -> String {
    response
        .into_json::<serde_json::Value>()
        .ok()
        .and_then(|body| {
            body.get("error")
                .and_then(|e| e.as_str())
                .map(|e| e.to_string())
        })
        .unwrap_or_else(|| "unknown error".into())
}

/// Exchange the authorization code (from [`LoginHandle`]) for tokens.
/// Sends the PKCE verifier; the redirect URI must match the one in the
/// authorization request.
pub fn exchange_code(
    config: &OAuthConfig,
    code: &str,
    redirect_uri: &str,
    verifier: &str,
) -> Result<TokenSet> {
    let tokens = token_request(
        config,
        vec![
            ("grant_type", "authorization_code"),
            ("code", code),
            ("client_id", config.client_id.as_str()),
            ("client_secret", config.client_secret.as_str()),
            ("redirect_uri", redirect_uri),
            ("code_verifier", verifier),
        ],
    )?;
    if tokens.refresh_token.is_none() {
        return Err(Error::OAuth(
            "the token endpoint returned no refresh token — the sign-in cannot outlive this \
             session. Retry the sign-in (consent must be granted)"
                .into(),
        ));
    }
    Ok(tokens)
}

/// Mint a fresh access token from the stored refresh token. Google's
/// refresh grant returns no new refresh token; the passed one stays valid.
pub fn refresh_access_token(config: &OAuthConfig, refresh_token: &str) -> Result<TokenSet> {
    let mut tokens = token_request(
        config,
        vec![
            ("grant_type", "refresh_token"),
            ("refresh_token", refresh_token),
            ("client_id", config.client_id.as_str()),
            ("client_secret", config.client_secret.as_str()),
        ],
    )?;
    if tokens.refresh_token.is_none() {
        // Google (and the mock) omit it on refresh grants: the original
        // refresh token remains the long-lived secret.
        tokens.refresh_token = Some(refresh_token.to_string());
    }
    Ok(tokens)
}

/// The SASL XOAUTH2 initial response for IMAP `AUTHENTICATE`:
/// `user=…\x01auth=Bearer …\x01\x01` (base64-encoded by the IMAP client).
pub fn xoauth2_sasl(username: &str, access_token: &str) -> String {
    format!("user={username}\u{1}auth=Bearer {access_token}\u{1}\u{1}")
}

/// Parse `a=1&b=2` query/form pairs (`+` as space, percent-decoding).
fn form_pairs(input: &str) -> Vec<(String, String)> {
    input
        .split('&')
        .filter(|part| !part.is_empty())
        .map(|part| match part.split_once('=') {
            Some((k, v)) => (decode_component(k), decode_component(v)),
            None => (decode_component(part), String::new()),
        })
        .collect()
}

/// Percent-decode one component; malformed escapes pass through verbatim.
fn decode_component(input: &str) -> String {
    let bytes = input.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            b'%' if i + 2 < bytes.len() => {
                let hex = |b: u8| (b as char).to_digit(16);
                match (hex(bytes[i + 1]), hex(bytes[i + 2])) {
                    (Some(hi), Some(lo)) => {
                        out.push(((hi << 4) | lo) as u8);
                        i += 3;
                    }
                    _ => {
                        out.push(bytes[i]);
                        i += 1;
                    }
                }
            }
            b => {
                out.push(b);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;

    #[test]
    fn sasl_string_is_the_xoauth2_format() {
        let sasl = xoauth2_sasl("you@gmail.com", "tok123");
        assert_eq!(sasl, "user=you@gmail.com\u{1}auth=Bearer tok123\u{1}\u{1}");
        // And the shape RFC 7628 requires: the field separator plus the
        // two-char terminating sequence.
        assert!(sasl.ends_with("\u{1}\u{1}"));
        assert_eq!(sasl.matches('\u{1}').count(), 3);
    }

    #[test]
    fn gmail_scope_is_exactly_the_full_mail_scope_google_imap_requires() {
        // Google's XOAUTH2 documentation pins the IMAP/POP/SMTP scope to
        // exactly this URL; a token granted a narrower scope (e.g.
        // `gmail.readonly`) is refused at AUTHENTICATE with a
        // scope-mismatch challenge naming this scope. Read-only command
        // usage cannot narrow the permission, so a well-meant
        // "least-privilege" edit here would break every Google sign-in —
        // the granular alternative is a Gmail API connector (issue #82,
        // docs/evidence/gmail-oauth-scope-2026-11-15.md).
        assert_eq!(GMAIL_SCOPE, "https://mail.google.com/");
        // The Gmail preset — and therefore the env-configured client —
        // requests exactly it, unmodified.
        assert_eq!(OAuthConfig::gmail("id", "secret").scope, GMAIL_SCOPE);
    }

    #[test]
    fn config_requires_both_credentials_or_neither() {
        let none = OAuthConfig::from_lookup(|_| None).unwrap();
        assert!(none.is_none());
        let both = OAuthConfig::from_lookup(|name| {
            (name == ENV_CLIENT_ID)
                .then_some("id.apps.googleusercontent.com".to_string())
                .or((name == ENV_CLIENT_SECRET).then_some("secret-xyz".to_string()))
        })
        .unwrap()
        .expect("configured");
        assert_eq!(both.client_id, "id.apps.googleusercontent.com");
        assert_eq!(both.auth_url, GMAIL_AUTH_URL);
        assert_eq!(both.token_url, GMAIL_TOKEN_URL);
        assert_eq!(both.scope, GMAIL_SCOPE);

        let only_id = OAuthConfig::from_lookup(|name| {
            (name == ENV_CLIENT_ID).then_some("id.apps.googleusercontent.com".to_string())
        })
        .unwrap_err();
        assert!(only_id.to_string().contains(ENV_CLIENT_SECRET));
        let only_secret = OAuthConfig::from_lookup(|name| {
            (name == ENV_CLIENT_SECRET).then_some("secret-xyz".to_string())
        })
        .unwrap_err();
        assert!(only_secret.to_string().contains(ENV_CLIENT_ID));
    }

    #[test]
    fn secrets_never_print_in_debug_output() {
        let config = OAuthConfig::gmail("id.apps.googleusercontent.com", "gsecret");
        assert!(!format!("{config:?}").contains("gsecret"));
        let tokens = TokenSet {
            access_token: "atoken".into(),
            refresh_token: Some("rtoken".into()),
        };
        let printed = format!("{tokens:?}");
        assert!(!printed.contains("atoken"));
        assert!(!printed.contains("rtoken"));
        assert!(printed.contains("<redacted>"));
    }

    #[test]
    fn pkce_verifier_and_challenge_have_the_rfc_shape() {
        let pkce = Pkce::generate();
        // 32 bytes → 43 base64url chars, unpadded, URL-safe alphabet.
        assert_eq!(pkce.verifier.len(), 43);
        assert!(
            !pkce.verifier.contains('+')
                && !pkce.verifier.contains('/')
                && !pkce.verifier.contains('=')
        );
        assert_eq!(pkce.challenge.len(), 43);
        let digest = BASE64_URL.encode(Sha256::digest(pkce.verifier.as_bytes()));
        assert_eq!(pkce.challenge, digest, "challenge must be S256(verifier)");
        // Independent draws differ (randomness).
        assert_ne!(pkce.verifier, Pkce::generate().verifier);
    }

    #[test]
    fn form_pairs_decodes_queries_like_a_browser() {
        let pairs = form_pairs("code=a%2Bb&state=x%20y&error=access+denied");
        assert_eq!(
            pairs,
            vec![
                ("code".into(), "a+b".into()),
                ("state".into(), "x y".into()),
                ("error".into(), "access denied".into()),
            ]
        );
        // Malformed escapes pass through.
        assert_eq!(form_pairs("a=%zz"), vec![("a".into(), "%zz".into())]);
    }

    #[test]
    fn base64_encoding_matches_the_imap_client_expectations() {
        // The IMAP client base64-encodes the SASL string itself; pin the
        // standard-alphabet round trip our tests rely on.
        let sasl = xoauth2_sasl("u", "t");
        let encoded = BASE64_STANDARD.encode(sasl.as_bytes());
        assert_eq!(
            BASE64_STANDARD
                .decode(encoded)
                .map(|b| String::from_utf8(b).unwrap())
                .unwrap(),
            sasl
        );
    }
}
