use anyhow::{bail, Context, Result};
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use rand::distributions::Alphanumeric;
use rand::Rng;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};
use url::Url;

use crate::config;

const DEFAULT_CLIENT_ID: &str =
    "563584335869-fgrhgmd47bqnekij5i8b5pr03ho849e6.apps.googleusercontent.com";
const DEFAULT_CLIENT_SECRET: &str = "j9iVZfS8kkCEFUPaAeJV0sAi";
const AUTH_URL: &str = "https://accounts.google.com/o/oauth2/auth";
const TOKEN_URL: &str = "https://oauth2.googleapis.com/token";
const SCOPES: &str =
    "openid email https://www.googleapis.com/auth/cloud-platform https://www.googleapis.com/auth/firebase";

#[derive(Serialize, Deserialize)]
struct Credentials {
    access_token: String,
    refresh_token: String,
    expires_at: u64,
}

#[derive(Deserialize)]
struct TokenResponse {
    access_token: String,
    expires_in: u64,
    #[serde(default)]
    refresh_token: Option<String>,
    #[serde(default)]
    id_token: Option<String>,
}

pub fn login() -> Result<()> {
    let (client_id, client_secret) = oauth_client()?;
    let listener = bind_listener()?;
    let port = listener.local_addr()?.port();
    let redirect_uri = format!("http://localhost:{port}");
    let state: String = rand::thread_rng()
        .sample_iter(&Alphanumeric)
        .take(32)
        .map(char::from)
        .collect();

    let mut auth_url = Url::parse(AUTH_URL)?;
    auth_url
        .query_pairs_mut()
        .append_pair("client_id", &client_id)
        .append_pair("redirect_uri", &redirect_uri)
        .append_pair("response_type", "code")
        .append_pair("scope", SCOPES)
        .append_pair("state", &state)
        .append_pair("access_type", "offline")
        .append_pair("prompt", "consent");

    println!("Opening the browser to sign in...");
    println!("If it does not open automatically, visit:\n\n{auth_url}\n");
    let _ = open::that(auth_url.as_str());
    println!("Waiting for the authorization response on {redirect_uri} ...");

    let code = wait_for_code(&listener, &state)?;

    let http = reqwest::blocking::Client::new();
    let resp = http
        .post(TOKEN_URL)
        .form(&[
            ("code", code.as_str()),
            ("client_id", client_id.as_str()),
            ("client_secret", client_secret.as_str()),
            ("redirect_uri", redirect_uri.as_str()),
            ("grant_type", "authorization_code"),
        ])
        .send()
        .context("failed to reach the token endpoint")?;
    if !resp.status().is_success() {
        let status = resp.status();
        bail!(
            "token exchange failed ({status}): {}",
            resp.text().unwrap_or_default()
        );
    }
    let token: TokenResponse = resp.json().context("failed to parse the token response")?;
    let refresh_token = token
        .refresh_token
        .clone()
        .context("no refresh token was returned; try `fad login` again")?;
    save_credentials(&Credentials {
        access_token: token.access_token.clone(),
        refresh_token,
        expires_at: now() + token.expires_in,
    })?;

    match token.id_token.as_deref().and_then(email_from_id_token) {
        Some(email) => println!("Logged in as {email}"),
        None => println!("Logged in successfully"),
    }
    Ok(())
}

pub fn get_access_token() -> Result<String> {
    let mut credentials = load_credentials()?;
    if credentials.expires_at > now() + 60 {
        return Ok(credentials.access_token);
    }
    let (client_id, client_secret) = oauth_client()?;
    let http = reqwest::blocking::Client::new();
    let resp = http
        .post(TOKEN_URL)
        .form(&[
            ("refresh_token", credentials.refresh_token.as_str()),
            ("client_id", client_id.as_str()),
            ("client_secret", client_secret.as_str()),
            ("grant_type", "refresh_token"),
        ])
        .send()
        .context("failed to reach the token endpoint")?;
    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().unwrap_or_default();
        bail!("failed to refresh the access token ({status}): {body}\nRun `fad login` again");
    }
    let token: TokenResponse = resp.json().context("failed to parse the token response")?;
    credentials.access_token = token.access_token.clone();
    credentials.expires_at = now() + token.expires_in;
    if let Some(refresh_token) = token.refresh_token {
        credentials.refresh_token = refresh_token;
    }
    save_credentials(&credentials)?;
    Ok(token.access_token)
}

fn oauth_client() -> Result<(String, String)> {
    let oauth = config::load_optional()?
        .map(|c| c.oauth)
        .unwrap_or_default();
    Ok((
        oauth
            .client_id
            .unwrap_or_else(|| DEFAULT_CLIENT_ID.to_string()),
        oauth
            .client_secret
            .unwrap_or_else(|| DEFAULT_CLIENT_SECRET.to_string()),
    ))
}

fn bind_listener() -> Result<TcpListener> {
    match TcpListener::bind(("127.0.0.1", 9005)) {
        Ok(listener) => Ok(listener),
        Err(_) => TcpListener::bind(("127.0.0.1", 0))
            .context("failed to bind a local port for the OAuth redirect"),
    }
}

fn wait_for_code(listener: &TcpListener, expected_state: &str) -> Result<String> {
    for stream in listener.incoming() {
        let mut stream = match stream {
            Ok(stream) => stream,
            Err(_) => continue,
        };
        let Some(path) = read_request_path(&mut stream) else {
            respond(&mut stream, "400 Bad Request", "Bad request");
            continue;
        };
        let Ok(url) = Url::parse(&format!("http://localhost{path}")) else {
            respond(&mut stream, "400 Bad Request", "Bad request");
            continue;
        };
        let params: HashMap<String, String> = url.query_pairs().into_owned().collect();
        if let Some(error) = params.get("error") {
            respond(
                &mut stream,
                "200 OK",
                "Login failed. You can close this tab.",
            );
            bail!("authorization was denied: {error}");
        }
        match (params.get("code"), params.get("state")) {
            (Some(code), Some(state)) if state == expected_state => {
                respond(
                    &mut stream,
                    "200 OK",
                    "Login successful. You can close this tab and return to the terminal.",
                );
                return Ok(code.clone());
            }
            (Some(_), Some(_)) => {
                respond(&mut stream, "400 Bad Request", "State mismatch");
                bail!("state parameter mismatch; try `fad login` again");
            }
            _ => respond(&mut stream, "404 Not Found", "Not found"),
        }
    }
    bail!("the local listener was closed unexpectedly")
}

fn read_request_path(stream: &mut TcpStream) -> Option<String> {
    let mut reader = BufReader::new(stream.try_clone().ok()?);
    let mut request_line = String::new();
    reader.read_line(&mut request_line).ok()?;
    loop {
        let mut line = String::new();
        match reader.read_line(&mut line) {
            Ok(0) => break,
            Ok(_) if line == "\r\n" || line == "\n" => break,
            Ok(_) => continue,
            Err(_) => break,
        }
    }
    request_line.split_whitespace().nth(1).map(str::to_string)
}

fn respond(stream: &mut TcpStream, status: &str, body: &str) {
    let html = format!("<html><body><p>{body}</p></body></html>");
    let response = format!(
        "HTTP/1.1 {status}\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{html}",
        html.len()
    );
    let _ = stream.write_all(response.as_bytes());
    let _ = stream.flush();
}

fn credentials_path() -> Result<PathBuf> {
    Ok(config::config_dir()?.join("credentials.json"))
}

fn save_credentials(credentials: &Credentials) -> Result<()> {
    let dir = config::config_dir()?;
    std::fs::create_dir_all(&dir).with_context(|| format!("failed to create {}", dir.display()))?;
    let path = credentials_path()?;
    std::fs::write(&path, serde_json::to_vec_pretty(credentials)?)
        .with_context(|| format!("failed to write {}", path.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
    }
    Ok(())
}

fn load_credentials() -> Result<Credentials> {
    let path = credentials_path()?;
    let text = std::fs::read_to_string(&path).context("not logged in; run `fad login` first")?;
    serde_json::from_str(&text)
        .with_context(|| format!("failed to parse {}; run `fad login` again", path.display()))
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn email_from_id_token(token: &str) -> Option<String> {
    let payload = token.split('.').nth(1)?;
    let bytes = URL_SAFE_NO_PAD.decode(payload).ok()?;
    let claims: serde_json::Value = serde_json::from_slice(&bytes).ok()?;
    claims.get("email")?.as_str().map(str::to_string)
}
