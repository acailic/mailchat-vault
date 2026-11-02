//! In-process mock OAuth2 authorization+token server (the hermetic
//! stand-in for accounts.google.com / oauth2.googleapis.com).
//!
//! Serves exactly the two endpoints the vault-core OAuth flow uses:
//!
//! - `GET /auth?…` — the "consent screen": validates the authorization
//!   request shape (response_type, client_id, loopback redirect_uri,
//!   PKCE S256 challenge, state, offline access, login_hint) and answers
//!   `302 Location: {redirect_uri}?code=…&state=…`. A real browser (and,
//!   in tests, an HTTP client with redirect following) lands on the
//!   app's loopback listener.
//! - `POST /token` (form-encoded) — the grant endpoint: validates client
//!   credentials, single-use codes, matching redirect_uri, and the PKCE
//!   verifier (S256 against the recorded challenge) for
//!   `authorization_code`; validates the refresh token for
//!   `refresh_token`. Issues `access-N` / `refresh-N` tokens with a
//!   monotonically increasing grant counter and a configurable expiry.
//!
//! No network access, no real credentials; everything stays on loopback.

// Shared between test binaries that use different subsets of the helpers.
#![allow(dead_code)]

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};

use base64::engine::general_purpose::URL_SAFE_NO_PAD as BASE64_URL;
use base64::Engine;
use sha2::{Digest, Sha256};
use vault_core::oauth::OAuthConfig;

/// Handle to the running mock OAuth server.
pub struct MockOAuth {
    /// Base URL to point an [`OAuthConfig`] at, e.g. `http://127.0.0.1:39721`.
    pub base_url: String,
    pub addr: SocketAddr,
    state: Arc<Shared>,
}

struct Shared {
    client_id: String,
    client_secret: String,
    /// Outstanding (unconsumed) authorization codes with their recorded
    /// PKCE challenge, redirect URI, and state.
    issued_codes: Mutex<Vec<IssuedCode>>,
    /// Refresh tokens ever issued (refresh grants must present one).
    refresh_tokens: Mutex<Vec<String>>,
    /// Grant counter: token `access-{n}` / `refresh-{n}` values.
    grants: AtomicU32,
    /// Code counter: `authcode-{n}` values.
    codes: AtomicU32,
    /// When set, the consent "screen" redirects back with
    /// `error=access_denied` (the user pressed "deny").
    deny_consent: Mutex<bool>,
    /// Access-token lifetime advertised in grants, seconds.
    expires_in: AtomicU32,
    /// Every token request's form pairs, in order.
    token_requests: Mutex<Vec<Vec<(String, String)>>>,
    /// Every authorization request's query pairs, in order.
    auth_requests: Mutex<Vec<Vec<(String, String)>>>,
    /// Whether refresh grants are honored (off = revoked refresh token).
    refresh_allowed: Mutex<bool>,
}

struct IssuedCode {
    code: String,
    redirect_uri: String,
    challenge: String,
    state: String,
}

impl MockOAuth {
    /// Start the server on an ephemeral loopback port, accepting exactly
    /// this client id/secret pair.
    pub fn start(client_id: &str, client_secret: &str) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind mock oauth");
        let addr = listener.local_addr().expect("mock addr");
        let state = Arc::new(Shared {
            client_id: client_id.to_string(),
            client_secret: client_secret.to_string(),
            issued_codes: Mutex::new(Vec::new()),
            refresh_tokens: Mutex::new(Vec::new()),
            grants: AtomicU32::new(0),
            codes: AtomicU32::new(0),
            deny_consent: Mutex::new(false),
            expires_in: AtomicU32::new(3600),
            token_requests: Mutex::new(Vec::new()),
            auth_requests: Mutex::new(Vec::new()),
            refresh_allowed: Mutex::new(true),
        });
        {
            let state = Arc::clone(&state);
            std::thread::spawn(move || {
                for stream in listener.incoming() {
                    let Ok(stream) = stream else { break };
                    let state = Arc::clone(&state);
                    std::thread::spawn(move || {
                        if let Err(e) = serve(stream, &state) {
                            // Client drops mid-request are normal; the rest
                            // are mock bugs worth seeing in test output.
                            eprintln!("mock oauth connection error: {e}");
                        }
                    });
                }
            });
        }
        Self {
            base_url: format!("http://{addr}"),
            addr,
            state,
        }
    }

    /// An [`OAuthConfig`] whose endpoints point at this mock.
    pub fn config(&self) -> OAuthConfig {
        OAuthConfig {
            client_id: self.state.client_id.clone(),
            client_secret: self.state.client_secret.clone(),
            auth_url: format!("{}/auth", self.base_url),
            token_url: format!("{}/token", self.base_url),
            scope: "https://mail.google.com/".to_string(),
        }
    }

    /// Make the consent "screen" deny access (redirects back with
    /// `error=access_denied`).
    pub fn deny_consent(&self) {
        *self.state.deny_consent.lock().unwrap() = true;
    }

    /// Refuse refresh grants (simulates a revoked refresh token).
    pub fn revoke_refresh(&self) {
        *self.state.refresh_allowed.lock().unwrap() = false;
    }

    /// The `access-N` value the next grant will carry.
    pub fn next_access_token(&self) -> String {
        format!("access-{}", self.state.grants.load(Ordering::SeqCst) + 1)
    }

    /// Observed token requests (form pairs, in order).
    pub fn token_requests(&self) -> Vec<Vec<(String, String)>> {
        self.state.token_requests.lock().unwrap().clone()
    }

    /// Observed authorization requests (query pairs, in order).
    pub fn auth_requests(&self) -> Vec<Vec<(String, String)>> {
        self.state.auth_requests.lock().unwrap().clone()
    }

    /// The refresh token from the nth grant (1-based); panics when the
    /// grant did not issue one.
    pub fn refresh_token(&self, grant: u32) -> String {
        format!("refresh-{grant}")
    }
}

/// Serve exactly one HTTP request, then close (Connection: close).
fn serve(mut stream: TcpStream, state: &Shared) -> std::io::Result<()> {
    let mut reader = BufReader::new(stream.try_clone()?);
    let mut request_line = String::new();
    if reader.read_line(&mut request_line)? == 0 {
        return Ok(());
    }
    let mut parts = request_line.split_whitespace();
    let method = parts.next().unwrap_or_default().to_string();
    let path = parts.next().unwrap_or_default().to_string();

    // Headers until the blank line.
    let mut headers: HashMap<String, String> = HashMap::new();
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line)? == 0 {
            break;
        }
        let line = line.trim_end();
        if line.is_empty() {
            break;
        }
        if let Some((name, value)) = line.split_once(':') {
            headers.insert(name.trim().to_lowercase(), value.trim().to_string());
        }
    }
    // Optional body (token endpoint: form-encoded).
    let mut body = String::new();
    if let Some(length) = headers
        .get("content-length")
        .and_then(|v| v.parse::<usize>().ok())
    {
        let mut bytes = vec![0u8; length];
        reader.read_exact(&mut bytes)?;
        body = String::from_utf8_lossy(&bytes).into_owned();
    }

    let (route, query) = match path.split_once('?') {
        Some((route, query)) => (route.to_string(), Some(query.to_string())),
        None => (path.clone(), None),
    };
    match (method.as_str(), route.as_str()) {
        ("GET", "/auth") => {
            let params: HashMap<String, String> = form_pairs(query.as_deref().unwrap_or(""))
                .into_iter()
                .collect();
            state
                .auth_requests
                .lock()
                .unwrap()
                .push(form_pairs(query.as_deref().unwrap_or("")));
            let bad = |stream: &mut TcpStream, message: &str| {
                respond_json(
                    stream,
                    "400 Bad Request",
                    &format!(
                        "{{\"error\":\"invalid_request\",\"error_description\":\"{message}\"}}"
                    ),
                )
            };
            // Validate exactly what the real consent screen enforces.
            if params.get("response_type").map(String::as_str) != Some("code") {
                return bad(&mut stream, "response_type must be code");
            }
            if params.get("client_id").map(|s| s.as_str()) != Some(state.client_id.as_str()) {
                return bad(&mut stream, "unknown client_id");
            }
            let redirect_uri = match params.get("redirect_uri") {
                Some(uri)
                    if uri.starts_with("http://127.0.0.1:")
                        || uri.starts_with("http://localhost:") =>
                {
                    uri.clone()
                }
                _ => return bad(&mut stream, "redirect_uri must be a loopback address"),
            };
            if params.get("code_challenge_method").map(String::as_str) != Some("S256") {
                return bad(&mut stream, "code_challenge_method must be S256");
            }
            let challenge = match params.get("code_challenge") {
                Some(c) if c.len() == 43 => c.clone(),
                _ => return bad(&mut stream, "code_challenge must be a 43-char S256 value"),
            };
            let state_param = params.get("state").cloned().unwrap_or_default();
            if state_param.is_empty() {
                return bad(&mut stream, "state is required");
            }
            if params.get("access_type").map(String::as_str) != Some("offline") {
                return bad(
                    &mut stream,
                    "access_type must be offline (refresh token wanted)",
                );
            }

            if *state.deny_consent.lock().unwrap() {
                let location = format!("{redirect_uri}?error=access_denied&state={state_param}");
                return respond_redirect(&mut stream, &location);
            }
            let code = format!(
                "authcode-{}",
                state.codes.fetch_add(1, Ordering::SeqCst) + 1
            );
            state.issued_codes.lock().unwrap().push(IssuedCode {
                code: code.clone(),
                redirect_uri: redirect_uri.clone(),
                challenge,
                state: state_param.clone(),
            });
            respond_redirect(
                &mut stream,
                &format!("{redirect_uri}?code={code}&state={state_param}"),
            )
        }
        ("POST", "/token") => {
            let pairs = form_pairs(&body);
            state.token_requests.lock().unwrap().push(pairs.clone());
            let params: HashMap<String, String> = pairs.into_iter().collect();
            if params.get("client_id").map(|s| s.as_str()) != Some(state.client_id.as_str())
                || params.get("client_secret").map(|s| s.as_str())
                    != Some(state.client_secret.as_str())
            {
                return respond_json(
                    &mut stream,
                    "400 Bad Request",
                    "{\"error\":\"invalid_client\"}",
                );
            }
            let grant = state.grants.fetch_add(1, Ordering::SeqCst) + 1;
            let expires_in = state.expires_in.load(Ordering::SeqCst);
            match params.get("grant_type").map(String::as_str) {
                Some("authorization_code") => {
                    let code = params.get("code").cloned().unwrap_or_default();
                    let verifier = params.get("code_verifier").cloned().unwrap_or_default();
                    let redirect_uri = params.get("redirect_uri").cloned().unwrap_or_default();
                    let mut issued = state.issued_codes.lock().unwrap();
                    let position = issued.iter().position(|issued| issued.code == code);
                    let Some(position) = position else {
                        return respond_json(
                            &mut stream,
                            "400 Bad Request",
                            "{\"error\":\"invalid_grant\"}",
                        );
                    };
                    let recorded = issued.remove(position); // single-use
                    if redirect_uri != recorded.redirect_uri {
                        return respond_json(
                            &mut stream,
                            "400 Bad Request",
                            "{\"error\":\"invalid_grant\",\"error_description\":\"redirect_uri mismatch\"}",
                        );
                    }
                    let digest = BASE64_URL.encode(Sha256::digest(verifier.as_bytes()));
                    if verifier.is_empty() || digest != recorded.challenge {
                        return respond_json(
                            &mut stream,
                            "400 Bad Request",
                            "{\"error\":\"invalid_grant\",\"error_description\":\"PKCE verification failed\"}",
                        );
                    }
                    let refresh = format!("refresh-{grant}");
                    state.refresh_tokens.lock().unwrap().push(refresh.clone());
                    let body = format!(
                        "{{\"access_token\":\"access-{grant}\",\"refresh_token\":\"{refresh}\",\
                         \"expires_in\":{expires_in},\"scope\":\"https://mail.google.com/\",\
                         \"token_type\":\"Bearer\"}}"
                    );
                    respond_json(&mut stream, "200 OK", &body)
                }
                Some("refresh_token") => {
                    let token = params.get("refresh_token").cloned().unwrap_or_default();
                    let known = state.refresh_tokens.lock().unwrap().contains(&token);
                    if !known || !*state.refresh_allowed.lock().unwrap() {
                        return respond_json(
                            &mut stream,
                            "400 Bad Request",
                            "{\"error\":\"invalid_grant\"}",
                        );
                    }
                    // Refresh grants rotate the access token only.
                    let body = format!(
                        "{{\"access_token\":\"access-{grant}\",\"expires_in\":{expires_in},\
                         \"scope\":\"https://mail.google.com/\",\"token_type\":\"Bearer\"}}"
                    );
                    respond_json(&mut stream, "200 OK", &body)
                }
                _ => respond_json(
                    &mut stream,
                    "400 Bad Request",
                    "{\"error\":\"unsupported_grant_type\"}",
                ),
            }
        }
        _ => respond_json(&mut stream, "404 Not Found", "{\"error\":\"not_found\"}"),
    }
}

/// An HTTP client standing in for the user's browser: visits the
/// authorization URL, follows the 302 back to the app's loopback
/// listener, and returns the final page body — including error pages
/// (4xx), which are exactly what the mismatch tests assert on.
pub fn browser_visit(auth_url: &str) -> std::result::Result<String, String> {
    let response = match ureq::get(auth_url)
        .timeout(std::time::Duration::from_secs(10))
        .call()
    {
        Ok(response) => response,
        // ureq turns 4xx/5xx into Err(Status); the browser still "sees"
        // that page, so hand it back to the test.
        Err(ureq::Error::Status(_code, response)) => response,
        Err(e) => return Err(format!("browser GET failed: {e}")),
    };
    response
        .into_string()
        .map_err(|e| format!("browser response unreadable: {e}"))
}

fn respond_json(stream: &mut TcpStream, status: &str, body: &str) -> std::io::Result<()> {
    write_response(stream, status, "application/json", body)
}

fn respond_redirect(stream: &mut TcpStream, location: &str) -> std::io::Result<()> {
    let response = format!(
        "HTTP/1.1 302 Found\r\nLocation: {location}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
    );
    stream.write_all(response.as_bytes())?;
    stream.flush()
}

fn write_response(
    stream: &mut TcpStream,
    status: &str,
    content_type: &str,
    body: &str,
) -> std::io::Result<()> {
    let response = format!(
        "HTTP/1.1 {status}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\n\
         Connection: close\r\n\r\n{body}",
        body.len()
    );
    stream.write_all(response.as_bytes())?;
    stream.flush()
}

/// Parse `a=1&b=2` pairs (`+` as space, percent-decoding) — the same
/// shape as [`vault_core::oauth`]'s private parser, duplicated here so
/// the mock stays standalone.
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
