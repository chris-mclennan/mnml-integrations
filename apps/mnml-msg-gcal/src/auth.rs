//! OAuth 2.0 loopback flow for the Calendar API. Same shape
//! `gcloud auth login` uses:
//!
//!   1. Read client_id + client_secret from
//!      `~/.config/mnml-msg-gcal/client.toml`.
//!   2. Open the browser to Google's OAuth consent screen with
//!      redirect_uri = http://127.0.0.1:<port>.
//!   3. Local HTTP server catches the redirect, extracts the code.
//!   4. Exchange code for access_token + refresh_token.
//!   5. Cache to `~/.config/mnml-msg-gcal/token.json`.
//!
//! Refresh happens transparently on 401 from the API client.

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

const SCOPES: &str = "https://www.googleapis.com/auth/calendar.events \
                      https://www.googleapis.com/auth/calendar.readonly";
const AUTH_URL: &str = "https://accounts.google.com/o/oauth2/v2/auth";
const TOKEN_URL: &str = "https://oauth2.googleapis.com/token";

pub fn client_config_path() -> PathBuf {
    dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from(std::env::var("HOME").unwrap_or_default()).join(".config"))
        .join("mnml-msg-gcal")
        .join("client.toml")
}

pub fn token_path() -> PathBuf {
    dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from(std::env::var("HOME").unwrap_or_default()).join(".config"))
        .join("mnml-msg-gcal")
        .join("token.json")
}

#[derive(Debug, Clone, Deserialize)]
pub struct ClientConfig {
    pub client_id: String,
    pub client_secret: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Token {
    pub access_token: String,
    pub refresh_token: String,
    pub token_type: String,
    /// UNIX epoch seconds when the access_token expires.
    pub expires_at: i64,
}

impl Token {
    /// True when the access token is within 60s of expiry.
    pub fn is_expired(&self) -> bool {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);
        self.expires_at.saturating_sub(now) < 60
    }
}

pub fn load_client_config() -> Result<ClientConfig> {
    let p = client_config_path();
    let text =
        std::fs::read_to_string(&p).with_context(|| format!("read {}", p.display()))?;
    let cfg: ClientConfig =
        toml::from_str(&text).with_context(|| format!("parse {}", p.display()))?;
    if cfg.client_id.is_empty() || cfg.client_secret.is_empty() {
        bail!(
            "empty client_id or client_secret at {} — see --check for setup",
            p.display()
        );
    }
    Ok(cfg)
}

pub fn load_token() -> Result<Token> {
    let p = token_path();
    let text = std::fs::read_to_string(&p).with_context(|| format!("read {}", p.display()))?;
    let tok: Token =
        serde_json::from_str(&text).with_context(|| format!("parse {}", p.display()))?;
    Ok(tok)
}

pub fn save_token(tok: &Token) -> Result<()> {
    let p = token_path();
    if let Some(parent) = p.parent() {
        std::fs::create_dir_all(parent).ok();
    }
    let text = serde_json::to_string_pretty(tok)?;
    std::fs::write(&p, text).with_context(|| format!("write {}", p.display()))?;
    // Best-effort chmod 0600 — the token grants Calendar API scope.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o600));
    }
    Ok(())
}

/// Kick off the interactive OAuth flow. Binds a loopback TCP
/// listener, opens the browser to Google's consent screen, catches
/// the redirect, exchanges the code for tokens, and returns them.
pub fn interactive_login() -> Result<Token> {
    let client = load_client_config()?;

    // 1. Bind a loopback listener; OS picks the port.
    let listener = TcpListener::bind("127.0.0.1:0")
        .context("bind loopback listener for OAuth redirect")?;
    let port = listener
        .local_addr()
        .context("read loopback port")?
        .port();
    let redirect_uri = format!("http://127.0.0.1:{port}");

    // 2. Build the auth URL.
    let auth_url = format!(
        "{AUTH_URL}?response_type=code\
         &client_id={cid}\
         &redirect_uri={redir}\
         &scope={scope}\
         &access_type=offline\
         &prompt=consent",
        cid = urlencoding_minimal(&client.client_id),
        redir = urlencoding_minimal(&redirect_uri),
        scope = urlencoding_minimal(SCOPES),
    );

    // 3. Open the browser (best-effort; user can copy the URL
    // manually if this fails).
    println!("Opening browser to Google's OAuth consent screen…");
    println!("If your browser doesn't open, visit this URL manually:");
    println!("{auth_url}");
    let _ = webbrowser::open(&auth_url);

    // 4. Wait for the redirect. Google adds ?code=... to the URL
    // when the user grants consent, or ?error=... on denial.
    let (mut stream, _) = listener
        .accept()
        .context("accept OAuth redirect on loopback listener")?;
    let request_line = {
        let mut reader = BufReader::new(&stream);
        let mut line = String::new();
        reader.read_line(&mut line).context("read request line")?;
        line
    };
    // Parse "GET /path?query HTTP/1.1"
    let target = request_line.split_whitespace().nth(1).unwrap_or("");
    let code = parse_code_from_target(target).ok_or_else(|| {
        anyhow::anyhow!(
            "OAuth redirect didn't include `code` — request was: {}",
            request_line.trim()
        )
    })?;

    // 5. Send a success response so the user knows they can close
    // the browser tab.
    let body = b"<html><body><h1>Signed in</h1><p>You can close this tab and return to the terminal.</p></body></html>";
    let response = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    );
    let _ = stream.write_all(response.as_bytes());
    let _ = stream.write_all(body);
    let _ = stream.flush();

    // 6. Exchange the code for tokens.
    println!("Exchanging authorization code for tokens…");
    let token = exchange_code(&client, &code, &redirect_uri)?;

    // 7. Persist.
    save_token(&token)?;
    println!("Token cached at {}", token_path().display());

    Ok(token)
}

/// Refresh the access token via the refresh_token grant. Persists
/// the refreshed token back to disk.
pub fn refresh_token(cur: &Token) -> Result<Token> {
    let client = load_client_config()?;
    let http = reqwest::blocking::Client::new();
    let resp = http
        .post(TOKEN_URL)
        .form(&[
            ("client_id", client.client_id.as_str()),
            ("client_secret", client.client_secret.as_str()),
            ("refresh_token", cur.refresh_token.as_str()),
            ("grant_type", "refresh_token"),
        ])
        .send()
        .context("POST /token (refresh)")?;
    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().unwrap_or_default();
        bail!("refresh failed: {status}: {body}");
    }
    let body: TokenResponse = resp.json().context("parse token response (refresh)")?;
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let refreshed = Token {
        access_token: body.access_token,
        // Google doesn't rotate refresh_tokens on every refresh —
        // keep the existing one if a new one wasn't returned.
        refresh_token: body.refresh_token.unwrap_or_else(|| cur.refresh_token.clone()),
        token_type: body.token_type,
        expires_at: now + body.expires_in as i64,
    };
    save_token(&refreshed)?;
    Ok(refreshed)
}

#[derive(Deserialize)]
struct TokenResponse {
    access_token: String,
    #[serde(default)]
    refresh_token: Option<String>,
    token_type: String,
    expires_in: u64,
}

fn exchange_code(client: &ClientConfig, code: &str, redirect_uri: &str) -> Result<Token> {
    let http = reqwest::blocking::Client::new();
    let resp = http
        .post(TOKEN_URL)
        .form(&[
            ("client_id", client.client_id.as_str()),
            ("client_secret", client.client_secret.as_str()),
            ("code", code),
            ("redirect_uri", redirect_uri),
            ("grant_type", "authorization_code"),
        ])
        .send()
        .context("POST /token (authorization_code)")?;
    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().unwrap_or_default();
        bail!("token exchange failed: {status}: {body}");
    }
    let body: TokenResponse = resp.json().context("parse token response")?;
    let refresh = body.refresh_token.ok_or_else(|| {
        anyhow::anyhow!(
            "token response missing refresh_token — the app may already be authorized. \
             Revoke the existing grant at myaccount.google.com/permissions and re-run."
        )
    })?;
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    Ok(Token {
        access_token: body.access_token,
        refresh_token: refresh,
        token_type: body.token_type,
        expires_at: now + body.expires_in as i64,
    })
}

fn parse_code_from_target(target: &str) -> Option<String> {
    // target is like "/?code=<code>&scope=... HTTP/1.1"
    // Extract query string.
    let q = target.split('?').nth(1)?;
    for pair in q.split('&') {
        let mut it = pair.splitn(2, '=');
        let k = it.next()?;
        let v = it.next()?;
        if k == "code" {
            return Some(url_decode(v));
        }
        if k == "error" {
            return None;
        }
    }
    None
}

fn url_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = String::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            let hi = hex(bytes[i + 1]);
            let lo = hex(bytes[i + 2]);
            if let (Some(h), Some(l)) = (hi, lo) {
                out.push((h * 16 + l) as char);
                i += 3;
                continue;
            }
        } else if bytes[i] == b'+' {
            out.push(' ');
            i += 1;
            continue;
        }
        out.push(bytes[i] as char);
        i += 1;
    }
    out
}

fn hex(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

fn urlencoding_minimal(s: &str) -> String {
    let mut out = String::with_capacity(s.len() * 3);
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{:02X}", b)),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_code_from_target_ok() {
        assert_eq!(
            parse_code_from_target("/?code=4%2F0abc&scope=xxx"),
            Some("4/0abc".to_string())
        );
    }

    #[test]
    fn parse_code_returns_none_on_error() {
        assert_eq!(
            parse_code_from_target("/?error=access_denied"),
            None
        );
    }

    #[test]
    fn parse_code_none_when_missing() {
        assert_eq!(parse_code_from_target("/?state=abc"), None);
    }

    #[test]
    fn is_expired_true_when_past() {
        let tok = Token {
            access_token: "a".into(),
            refresh_token: "r".into(),
            token_type: "Bearer".into(),
            expires_at: 0,
        };
        assert!(tok.is_expired());
    }
}
