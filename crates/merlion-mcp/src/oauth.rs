//! OAuth2 PKCE flow for HTTP MCP servers (spec 2024-11-05).
//!
//! Servers that require authorization advertise OAuth metadata at
//! `<server_url>/.well-known/oauth-authorization-server`. We discover that,
//! generate a PKCE pair, spin up a localhost listener for the redirect,
//! prompt the user to open the authorization URL in their browser, and
//! exchange the resulting `code` at the token endpoint. The acquired
//! tokens are persisted in `~/.merlion/mcp-tokens.yaml` keyed on the
//! server's logical name from `McpRegistry`.
//!
//! For `client_id`: if the server advertises a `registration_endpoint`
//! we'll do dynamic client registration (RFC 7591). Otherwise the user
//! must pre-register and provide a static `oauth_client_id` in mcp.yaml.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine as _;
use rand::rngs::OsRng;
use rand::RngCore;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

use crate::{Error, Result};

const PKCE_VERIFIER_LEN: usize = 64;
const CALLBACK_PATH: &str = "/callback";
const CLIENT_NAME: &str = "Merlion";

/// OAuth2 server metadata returned from
/// `<server_url>/.well-known/oauth-authorization-server`. We only model
/// the fields we care about; everything else is ignored.
#[derive(Debug, Clone, Deserialize)]
struct ServerMetadata {
    authorization_endpoint: String,
    token_endpoint: String,
    #[serde(default)]
    registration_endpoint: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Tokens {
    pub access_token: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refresh_token: Option<String>,
    /// Unix timestamp (seconds) when the access_token expires. `None` if
    /// the server didn't return `expires_in`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<i64>,
    pub token_type: String,
}

impl Tokens {
    /// Returns `true` if `expires_at` is set and is in the past (allowing
    /// a 30-second clock-skew margin).
    pub fn is_expired(&self) -> bool {
        let Some(exp) = self.expires_at else {
            return false;
        };
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);
        now + 30 >= exp
    }
}

#[derive(Debug, Deserialize)]
struct TokenResponse {
    access_token: String,
    #[serde(default)]
    refresh_token: Option<String>,
    #[serde(default)]
    expires_in: Option<i64>,
    #[serde(default = "default_token_type")]
    token_type: String,
}

fn default_token_type() -> String {
    "Bearer".to_string()
}

#[derive(Debug, Deserialize)]
struct RegistrationResponse {
    client_id: String,
}

pub struct OauthFlow {
    /// The MCP server's URL — used to derive the well-known metadata path.
    pub server_url: String,
    /// Pre-registered client_id. If `None`, we'll try dynamic client
    /// registration against `registration_endpoint`.
    pub static_client_id: Option<String>,
    /// Requested OAuth scopes. Joined with spaces per RFC 6749.
    pub scopes: Vec<String>,
}

impl OauthFlow {
    /// Run the full PKCE authorization-code flow. Blocks waiting for the
    /// browser callback to localhost.
    pub async fn authorize(&self) -> Result<Tokens> {
        let metadata = self.discover_metadata().await?;

        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .map_err(|e| Error::Other(format!("bind callback listener: {e}")))?;
        let port = listener
            .local_addr()
            .map_err(|e| Error::Other(format!("local_addr: {e}")))?
            .port();
        let redirect_uri = format!("http://127.0.0.1:{port}{CALLBACK_PATH}");

        let client_id = self.resolve_client_id(&metadata, &redirect_uri).await?;

        let verifier = generate_code_verifier();
        let challenge = code_challenge_s256(&verifier);

        let mut auth_url = format!(
            "{auth}?response_type=code&client_id={cid}&redirect_uri={ru}&code_challenge={cc}&code_challenge_method=S256",
            auth = metadata.authorization_endpoint,
            cid = urlencoding::encode(&client_id),
            ru = urlencoding::encode(&redirect_uri),
            cc = urlencoding::encode(&challenge),
        );
        if !self.scopes.is_empty() {
            let scope = self.scopes.join(" ");
            auth_url.push_str("&scope=");
            auth_url.push_str(&urlencoding::encode(&scope));
        }

        println!("Open this URL in your browser to authorize:\n\n  {auth_url}\n");
        println!("Waiting for the callback on {redirect_uri} ...");

        let code = wait_for_callback(listener).await?;

        let http = reqwest::Client::new();
        let mut form = vec![
            ("grant_type", "authorization_code".to_string()),
            ("code", code),
            ("code_verifier", verifier),
            ("client_id", client_id.clone()),
            ("redirect_uri", redirect_uri.clone()),
        ];
        if !self.scopes.is_empty() {
            form.push(("scope", self.scopes.join(" ")));
        }
        let resp = http
            .post(&metadata.token_endpoint)
            .form(&form)
            .send()
            .await
            .map_err(|e| Error::Other(format!("token request: {e}")))?;
        let status = resp.status();
        if !status.is_success() {
            let text = resp.text().await.unwrap_or_default();
            return Err(Error::Other(format!(
                "token endpoint returned {status}: {text}"
            )));
        }
        let tr: TokenResponse = resp
            .json()
            .await
            .map_err(|e| Error::Other(format!("parse token response: {e}")))?;

        Ok(token_response_to_tokens(tr))
    }

    /// Exchange a `refresh_token` for fresh tokens.
    pub async fn refresh(&self, refresh_token: &str) -> Result<Tokens> {
        let metadata = self.discover_metadata().await?;
        let client_id = self
            .static_client_id
            .clone()
            .ok_or_else(|| Error::Other(
                "refresh requires a known client_id; either pass one via mcp.yaml or re-run authorize".into(),
            ))?;

        let http = reqwest::Client::new();
        let form = [
            ("grant_type", "refresh_token"),
            ("refresh_token", refresh_token),
            ("client_id", client_id.as_str()),
        ];
        let resp = http
            .post(&metadata.token_endpoint)
            .form(&form)
            .send()
            .await
            .map_err(|e| Error::Other(format!("refresh request: {e}")))?;
        let status = resp.status();
        if !status.is_success() {
            let text = resp.text().await.unwrap_or_default();
            return Err(Error::Other(format!(
                "refresh endpoint returned {status}: {text}"
            )));
        }
        let tr: TokenResponse = resp
            .json()
            .await
            .map_err(|e| Error::Other(format!("parse refresh response: {e}")))?;
        Ok(token_response_to_tokens(tr))
    }

    async fn discover_metadata(&self) -> Result<ServerMetadata> {
        let base = self.server_url.trim_end_matches('/');
        // The well-known path applies to the origin, not the full path,
        // but plenty of MCP servers serve it from the same base — try the
        // base first, fall back to origin.
        let candidates = [
            format!("{base}/.well-known/oauth-authorization-server"),
            origin_of(&self.server_url)
                .map(|o| format!("{o}/.well-known/oauth-authorization-server"))
                .unwrap_or_default(),
        ];
        let http = reqwest::Client::new();
        let mut last_err = String::new();
        for url in candidates.iter().filter(|s| !s.is_empty()) {
            match http.get(url).send().await {
                Ok(resp) if resp.status().is_success() => {
                    return resp
                        .json::<ServerMetadata>()
                        .await
                        .map_err(|e| Error::Other(format!("parse oauth metadata: {e}")));
                }
                Ok(resp) => {
                    last_err = format!("GET {url} -> {}", resp.status());
                }
                Err(e) => {
                    last_err = format!("GET {url}: {e}");
                }
            }
        }
        Err(Error::Other(format!(
            "could not discover oauth metadata: {last_err}"
        )))
    }

    async fn resolve_client_id(
        &self,
        metadata: &ServerMetadata,
        redirect_uri: &str,
    ) -> Result<String> {
        if let Some(id) = &self.static_client_id {
            return Ok(id.clone());
        }
        let endpoint = metadata.registration_endpoint.as_deref().ok_or_else(|| {
            Error::Other(
                "no static client_id provided and server has no registration_endpoint".into(),
            )
        })?;
        let body = serde_json::json!({
            "redirect_uris": [redirect_uri],
            "client_name": CLIENT_NAME,
        });
        let http = reqwest::Client::new();
        let resp = http
            .post(endpoint)
            .json(&body)
            .send()
            .await
            .map_err(|e| Error::Other(format!("dynamic registration: {e}")))?;
        let status = resp.status();
        if !status.is_success() {
            let text = resp.text().await.unwrap_or_default();
            return Err(Error::Other(format!(
                "registration endpoint returned {status}: {text}"
            )));
        }
        let parsed: RegistrationResponse = resp
            .json()
            .await
            .map_err(|e| Error::Other(format!("parse registration response: {e}")))?;
        Ok(parsed.client_id)
    }
}

fn token_response_to_tokens(tr: TokenResponse) -> Tokens {
    let expires_at = tr.expires_in.map(|secs| {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);
        now + secs
    });
    Tokens {
        access_token: tr.access_token,
        refresh_token: tr.refresh_token,
        expires_at,
        token_type: tr.token_type,
    }
}

fn origin_of(url: &str) -> Option<String> {
    let (scheme, rest) = url.split_once("://")?;
    let host_end = rest.find('/').unwrap_or(rest.len());
    let host = &rest[..host_end];
    Some(format!("{scheme}://{host}"))
}

/// Generate a PKCE `code_verifier` per RFC 7636 §4.1: 43-128 chars from the
/// URL-safe alphabet. We produce 64 chars of base64url-no-pad encoding of
/// 48 random bytes (48 bytes → exactly 64 base64 chars, no padding).
pub fn generate_code_verifier() -> String {
    let mut bytes = [0u8; 48];
    OsRng.fill_bytes(&mut bytes);
    let s = URL_SAFE_NO_PAD.encode(bytes);
    // Defensive — should be exactly 64 already.
    s.chars().take(PKCE_VERIFIER_LEN).collect()
}

/// `code_challenge = base64url(sha256(code_verifier))` (no padding) — RFC
/// 7636 §4.2.
pub fn code_challenge_s256(verifier: &str) -> String {
    let digest = Sha256::digest(verifier.as_bytes());
    URL_SAFE_NO_PAD.encode(digest)
}

/// Read a single HTTP request from the listener, extract `?code=...` from
/// the request line, send back a tiny HTML success page, and return the
/// code.
async fn wait_for_callback(listener: TcpListener) -> Result<String> {
    // 5-minute upper bound — if the user gets distracted we shouldn't hang
    // forever.
    let accept = tokio::time::timeout(Duration::from_secs(300), listener.accept()).await;
    let (mut sock, _) = match accept {
        Ok(Ok(pair)) => pair,
        Ok(Err(e)) => return Err(Error::Other(format!("accept callback: {e}"))),
        Err(_) => {
            return Err(Error::Other(
                "timed out waiting for OAuth callback (5 min)".into(),
            ))
        }
    };

    let mut buf = Vec::with_capacity(2048);
    let mut tmp = [0u8; 1024];
    loop {
        match tokio::time::timeout(Duration::from_secs(5), sock.read(&mut tmp)).await {
            Ok(Ok(0)) => break,
            Ok(Ok(n)) => {
                buf.extend_from_slice(&tmp[..n]);
                if buf.windows(4).any(|w| w == b"\r\n\r\n") {
                    break;
                }
            }
            _ => break,
        }
    }

    let request_line = std::str::from_utf8(&buf)
        .ok()
        .and_then(|s| s.lines().next())
        .ok_or_else(|| Error::Other("malformed callback request".into()))?
        .to_string();

    let code = parse_code_from_request_line(&request_line)
        .ok_or_else(|| Error::Other(format!("no `code` param in callback: {request_line}")))?;

    let body = "<!doctype html><html><body><h2>Authorization complete</h2>\
                <p>You may close this tab.</p></body></html>";
    let response = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    let _ = sock.write_all(response.as_bytes()).await;
    let _ = sock.shutdown().await;

    Ok(code)
}

fn parse_code_from_request_line(line: &str) -> Option<String> {
    let mut parts = line.split_whitespace();
    let _method = parts.next()?;
    let target = parts.next()?;
    let query = target.split_once('?').map(|(_, q)| q)?;
    for kv in query.split('&') {
        if let Some(("code", v)) = kv.split_once('=') {
            return urlencoding::decode(v).ok().map(|s| s.into_owned());
        }
    }
    None
}

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct TokenStore {
    #[serde(default)]
    pub servers: BTreeMap<String, Tokens>,
}

impl TokenStore {
    pub fn load(path: &Path) -> Result<Self> {
        if !path.exists() {
            return Ok(Self::default());
        }
        let text = std::fs::read_to_string(path)?;
        if text.trim().is_empty() {
            return Ok(Self::default());
        }
        let parsed: Self = serde_yaml::from_str(&text)?;
        Ok(parsed)
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let yaml = serde_yaml::to_string(self)?;
        std::fs::write(path, yaml)?;
        // Best-effort 0600 so tokens aren't world-readable. Silently
        // tolerate platforms / filesystems that don't support it.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            if let Ok(meta) = std::fs::metadata(path) {
                let mut perms = meta.permissions();
                perms.set_mode(0o600);
                let _ = std::fs::set_permissions(path, perms);
            }
        }
        Ok(())
    }

    /// Default location: `$MERLION_HOME/mcp-tokens.yaml` or
    /// `~/.merlion/mcp-tokens.yaml`.
    pub fn default_path() -> PathBuf {
        if let Ok(home) = std::env::var("MERLION_HOME") {
            return PathBuf::from(home).join("mcp-tokens.yaml");
        }
        dirs::home_dir()
            .map(|h| h.join(".merlion").join("mcp-tokens.yaml"))
            .unwrap_or_else(|| PathBuf::from(".merlion/mcp-tokens.yaml"))
    }

    pub fn get(&self, name: &str) -> Option<&Tokens> {
        self.servers.get(name)
    }

    pub fn set(&mut self, name: String, tokens: Tokens) {
        self.servers.insert(name, tokens);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rfc7636_test_vector() {
        // RFC 7636 Appendix B: verifier "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk"
        // → challenge "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"
        let verifier = "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk";
        let challenge = code_challenge_s256(verifier);
        assert_eq!(challenge, "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM");
    }

    #[test]
    fn parse_code_from_request_line_extracts_query() {
        let line = "GET /callback?code=abc123&state=xyz HTTP/1.1";
        assert_eq!(
            parse_code_from_request_line(line).as_deref(),
            Some("abc123")
        );
    }

    #[test]
    fn parse_code_handles_url_encoded() {
        let line = "GET /callback?code=a%2Bb%2Fc HTTP/1.1";
        assert_eq!(parse_code_from_request_line(line).as_deref(), Some("a+b/c"));
    }

    #[test]
    fn origin_of_strips_path() {
        assert_eq!(
            origin_of("https://example.com/mcp/endpoint").as_deref(),
            Some("https://example.com")
        );
    }
}
