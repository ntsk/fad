use anyhow::{bail, Context, Result};
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use rand::distr::{Alphanumeric, Distribution};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::io::{BufRead, BufReader, ErrorKind, Write};
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use url::Url;

use crate::apps;
use crate::config;

const DEFAULT_CLIENT_ID: &str =
    "563584335869-fgrhgmd47bqnekij5i8b5pr03ho849e6.apps.googleusercontent.com";
const DEFAULT_CLIENT_SECRET: &str = "j9iVZfS8kkCEFUPaAeJV0sAi";
const AUTH_URL: &str = "https://accounts.google.com/o/oauth2/auth";
const TOKEN_URL: &str = "https://oauth2.googleapis.com/token";
const REVOKE_URL: &str = "https://oauth2.googleapis.com/revoke";
const SCOPES: &str =
    "openid email https://www.googleapis.com/auth/cloud-platform https://www.googleapis.com/auth/firebase";
const LOGIN_TIMEOUT: Duration = Duration::from_secs(300);

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
    let redirect_uri = redirect_uri(port);
    let state: String = Alphanumeric
        .sample_iter(&mut rand::rng())
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

    let code = wait_for_code(&listener, &state, LOGIN_TIMEOUT)?;

    let http = crate::http::client();
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
    if let Err(err) = apps::select_and_save(&token.access_token) {
        eprintln!("Warning: app selection failed: {err:#}");
        eprintln!(
            "Set app_id in {} manually",
            config::config_path()?.display()
        );
    }
    Ok(())
}

pub fn logout() -> Result<()> {
    let path = credentials_path()?;
    if !path.exists() {
        println!("Not logged in");
        return Ok(());
    }
    let credentials: Option<Credentials> = std::fs::read_to_string(&path)
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok());
    if let Some(credentials) = credentials {
        revoke_token(&credentials.refresh_token);
    }
    std::fs::remove_file(&path).with_context(|| format!("failed to remove {}", path.display()))?;
    println!("Logged out");
    Ok(())
}

fn revoke_token(token: &str) {
    let http = crate::http::client();
    match http.post(REVOKE_URL).form(&[("token", token)]).send() {
        Ok(resp) if resp.status().is_success() => {}
        Ok(resp) => eprintln!("Warning: token revocation failed ({})", resp.status()),
        Err(err) => eprintln!("Warning: could not reach the revocation endpoint: {err}"),
    }
}

pub fn get_access_token() -> Result<String> {
    let mut credentials = load_credentials()?;
    if credentials.expires_at > now() + 60 {
        return Ok(credentials.access_token);
    }
    let (client_id, client_secret) = oauth_client()?;
    let http = crate::http::client();
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

fn redirect_uri(port: u16) -> String {
    format!("http://127.0.0.1:{port}")
}

fn bind_listener() -> Result<TcpListener> {
    match TcpListener::bind(("127.0.0.1", 9005)) {
        Ok(listener) => Ok(listener),
        Err(_) => TcpListener::bind(("127.0.0.1", 0))
            .context("failed to bind a local port for the OAuth redirect"),
    }
}

fn wait_for_code(
    listener: &TcpListener,
    expected_state: &str,
    timeout: Duration,
) -> Result<String> {
    listener
        .set_nonblocking(true)
        .context("failed to configure the local listener")?;
    let deadline = Instant::now() + timeout;
    loop {
        if Instant::now() >= deadline {
            bail!("timed out waiting for the authorization response; run `fad login` again");
        }
        match listener.accept() {
            Ok((mut stream, _)) => {
                let _ = stream.set_nonblocking(false);
                let _ = stream.set_read_timeout(Some(Duration::from_secs(10)));
                if let Some(code) = handle_connection(&mut stream, expected_state)? {
                    return Ok(code);
                }
            }
            Err(err) if err.kind() == ErrorKind::WouldBlock => {
                std::thread::sleep(Duration::from_millis(50));
            }
            Err(_) => std::thread::sleep(Duration::from_millis(50)),
        }
    }
}

fn handle_connection(stream: &mut TcpStream, expected_state: &str) -> Result<Option<String>> {
    let Some(path) = read_request_path(stream) else {
        respond(stream, "400 Bad Request", "Bad request");
        return Ok(None);
    };
    let Ok(url) = Url::parse(&format!("http://localhost{path}")) else {
        respond(stream, "400 Bad Request", "Bad request");
        return Ok(None);
    };
    let params: HashMap<String, String> = url.query_pairs().into_owned().collect();
    if let Some(error) = params.get("error") {
        respond(stream, "200 OK", "Login failed. You can close this tab.");
        bail!("authorization was denied: {error}");
    }
    match (params.get("code"), params.get("state")) {
        (Some(code), Some(state)) if state == expected_state => {
            respond(
                stream,
                "200 OK",
                "Login successful. You can close this tab and return to the terminal.",
            );
            Ok(Some(code.clone()))
        }
        (Some(_), Some(_)) => {
            respond(stream, "400 Bad Request", "State mismatch");
            bail!("state parameter mismatch; try `fad login` again");
        }
        _ => {
            respond(stream, "404 Not Found", "Not found");
            Ok(None)
        }
    }
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

#[cfg(test)]
mod tests {
    use super::*;
    use mockito::Matcher;
    use std::io::Read;
    use std::net::TcpStream as ClientStream;
    use std::thread;

    fn request(addr: std::net::SocketAddr, path: &str) -> String {
        let mut stream = ClientStream::connect(addr).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        stream
            .write_all(format!("GET {path} HTTP/1.1\r\nHost: localhost\r\n\r\n").as_bytes())
            .unwrap();
        let mut response = String::new();
        let _ = stream.read_to_string(&mut response);
        response
    }

    fn send_request(addr: std::net::SocketAddr, path: &str) -> thread::JoinHandle<String> {
        let path = path.to_string();
        thread::spawn(move || request(addr, &path))
    }

    #[test]
    fn wait_for_code_accepts_matching_state() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let addr = listener.local_addr().unwrap();
        let client = send_request(addr, "/?code=abc123&state=st1");

        let code = wait_for_code(&listener, "st1", Duration::from_secs(5)).unwrap();

        assert_eq!(code, "abc123");
        assert!(client.join().unwrap().contains("200 OK"));
    }

    #[test]
    fn wait_for_code_ignores_unrelated_requests() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let addr = listener.local_addr().unwrap();
        let client = thread::spawn(move || {
            let favicon = request(addr, "/favicon.ico");
            let callback = request(addr, "/?code=abc123&state=st1");
            (favicon, callback)
        });

        let code = wait_for_code(&listener, "st1", Duration::from_secs(5)).unwrap();

        assert_eq!(code, "abc123");
        let (favicon, callback) = client.join().unwrap();
        assert!(favicon.contains("404 Not Found"));
        assert!(callback.contains("200 OK"));
    }

    #[test]
    fn wait_for_code_rejects_state_mismatch() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let addr = listener.local_addr().unwrap();
        let client = send_request(addr, "/?code=abc123&state=wrong");

        let err = wait_for_code(&listener, "st1", Duration::from_secs(5)).unwrap_err();

        assert!(err.to_string().contains("state parameter mismatch"));
        assert!(client.join().unwrap().contains("400 Bad Request"));
    }

    #[test]
    fn wait_for_code_reports_denied_authorization() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let addr = listener.local_addr().unwrap();
        let client = send_request(addr, "/?error=access_denied");

        let err = wait_for_code(&listener, "st1", Duration::from_secs(5)).unwrap_err();

        assert!(err
            .to_string()
            .contains("authorization was denied: access_denied"));
        client.join().unwrap();
    }

    #[test]
    fn wait_for_code_times_out_without_response() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();

        let err = wait_for_code(&listener, "st1", Duration::from_millis(150)).unwrap_err();

        assert!(err.to_string().contains("timed out"));
    }

    #[test]
    fn redirect_uri_uses_ipv4_loopback() {
        assert_eq!(redirect_uri(9005), "http://127.0.0.1:9005");
    }

    #[test]
    fn extracts_email_from_id_token_payload() {
        let payload = URL_SAFE_NO_PAD.encode(r#"{"email":"user@example.com"}"#);
        let token = format!("header.{payload}.signature");

        assert_eq!(
            email_from_id_token(&token).as_deref(),
            Some("user@example.com")
        );
    }

    #[test]
    fn returns_none_for_malformed_id_token() {
        assert_eq!(email_from_id_token("not-a-jwt"), None);
        assert_eq!(email_from_id_token("a.!!!.c"), None);
        let payload = URL_SAFE_NO_PAD.encode(r#"{"sub":"123"}"#);
        assert_eq!(email_from_id_token(&format!("a.{payload}.c")), None);
    }

    const TEST_PRIVATE_KEY: &str = "-----BEGIN PRIVATE KEY-----\nMIIEvAIBADANBgkqhkiG9w0BAQEFAASCBKYwggSiAgEAAoIBAQCfwYtTneIRl8fL\nZQs1o7h35oapgKq4xkTKc49OXfZJBY5XNPuhQmODFKja4Xg6x+AcQcNeavfBg/cS\ndo9MN4oJPtwJr6s6bjr2cdLhCi2fCKYmm2GKEoprWcCHr92C7C2paO7A+zvHBr3E\nreg3C09MjjuIHBbWNr6WG6em8jVyamYKYP+i1Ybb8z1FfMWysAbOpyOiy5lMzt1j\nF6K+VV8MeGqWFWb+R9s5o6L7yXAmjh98mfkdy0qKbDwrOPYy17vcR8UEvlaqpi8t\nLNogE+/sUCEdr28T4iuFDcjFU2sQfXKCrIEW53sFQQyl4oaGCdKfkjqToPHfpKGt\n2dDJXHDTAgMBAAECggEAE7m2FlD8ROfUx4xmYe0hLczM+8jjS4VPoR+7phV7/3As\nLyBfoX2tA9ZdMwl76uYbCeIk2Vej18UPkLwK3YJODO4yBRAnuEM8DInpW9gB4g0T\nVtkApie7551hZF+Wnj/DM5O9Rx6+NsjiTZKbhZBj7jPxrdCqETEZPzeS784gQ0wl\nxvIiaQcEoHytWSB2LQCfZ+MxVQ7X7xh4gumlIBEpAJoytsGp3f0BOLpo9t9xO6qe\nYF4vcpUnrGZ4UVb24kQVJNjR9MFMgtMmSiB3TSXEmzwEVlphT8gR18gRtyH76zGM\nueN3I7KIGa8B5PN6i3CSiOp2Y68ky87p7p2Gs9+bwQKBgQDcCJb4nXx0686jshrN\nKhYUne25fGBVSXgxNOYdF5TB1EaVE6Hc7P0PRzsfMK0tabgSh64VGRQaliOhOFEx\nRNZrZu9GbIoK20POEaSizmOuouSEHH6c3A4U8b/+V+rLpBu0ZiVwXzV4ADwrFTF/\n71J3QV2X0sVBtl0o1JRnKEkx6QKBgQC53pzO3I4qoUOr/oJDBapiSvafq6pA6iSz\nKO/YGri8Cmkn3Cjbsy23ZE9nyVNuTBbnR0jHxk9ZaWVvTAP7ThXVTQrVex9gTnIp\ng5dbNO3TqtcQNkDnJ/d4JWcNZRD1DpDR3sSIlPgfvQsYXEIwQZReOz9p/k92HEEW\n+zPH/L87WwKBgHfKtWblVrzRJM86SB0qrJrM4H/7lvbX6PfhNObhz7s3NrYy2gzN\neXi37xgsCByRUgXEmKIj5S4UT5GWd527PIF8qQhOT1lZxrCKKnf4pYyOYpsKaGQ9\n6ey9MSnn84yq6+prMjbbnuCWQCu0fh6IzPzgOXRO69W60z1HfwQqiq8BAoGATX5e\n6nBSZbuutzr5nG/0Rd7zTEcKSN5WRsw+k18wvlWo2hGUh2UBHoEYCjGKM2ZN9kdm\nNMSduK2UuP58en5n4/KnHbKjtkd+mYhfxosezS1hVUUJclbbeqA9gvwsQb+86YNz\ndW6GtNTgl1t/zRbKgS86lTqObrQA/0/kmvDp2hkCgYBGxnAUQ2Zxm2h8D0M/dTWd\nRzE6PgnhIhhQJM2ydEwpOn9W9MnixCTVH3lZJ25WmtBhWLBoRFYj1KDDODD0sUIM\nJQSVN6ZtqSiYp86uq0W4AASfb4JwbBJqBmmbO8vTPX/RmN61Ou3edwm0lxlMZeSu\nVF203+OAA3KBE2OFlN243Q==\n-----END PRIVATE KEY-----\n";

    fn test_service_account(token_uri: &str) -> ServiceAccount {
        parse_service_account(
            &serde_json::json!({
                "type": "service_account",
                "client_email": "fad@my-project.iam.gserviceaccount.com",
                "private_key_id": "kid-123",
                "private_key": TEST_PRIVATE_KEY,
                "token_uri": token_uri,
            })
            .to_string(),
        )
        .unwrap()
    }

    #[test]
    fn parses_service_account_key_fields() {
        let sa = parse_service_account(
            &serde_json::json!({
                "type": "service_account",
                "client_email": "fad@my-project.iam.gserviceaccount.com",
                "private_key": TEST_PRIVATE_KEY,
            })
            .to_string(),
        )
        .unwrap();

        assert_eq!(sa.client_email, "fad@my-project.iam.gserviceaccount.com");
        assert_eq!(sa.token_uri, TOKEN_URL);
    }

    #[test]
    fn rejects_non_service_account_json() {
        let err = parse_service_account(r#"{"client_id":"x"}"#).unwrap_err();
        assert!(err.to_string().contains("service account"));
    }

    #[test]
    fn assertion_carries_jwt_bearer_claims() {
        let sa = test_service_account("https://oauth2.googleapis.com/token");

        let assertion = build_assertion(&sa, 1_000_000).unwrap();

        let claims_segment = assertion.split('.').nth(1).unwrap();
        let bytes = URL_SAFE_NO_PAD.decode(claims_segment).unwrap();
        let claims: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(claims["iss"], "fad@my-project.iam.gserviceaccount.com");
        assert_eq!(claims["aud"], "https://oauth2.googleapis.com/token");
        assert_eq!(claims["scope"], SA_SCOPE);
        assert_eq!(claims["iat"], 1_000_000);
        assert_eq!(claims["exp"], 1_000_000 + 3600);
    }

    #[test]
    fn token_exchange_uses_jwt_bearer_grant() {
        let mut server = mockito::Server::new();
        let mock = server
            .mock("POST", "/token")
            .match_body(Matcher::AllOf(vec![
                Matcher::UrlEncoded(
                    "grant_type".into(),
                    "urn:ietf:params:oauth:grant-type:jwt-bearer".into(),
                ),
                Matcher::UrlEncoded("assertion".into(), "signed-jwt".into()),
            ]))
            .with_body(r#"{"access_token":"sa-token","expires_in":3600}"#)
            .create();

        let http = crate::http::client();
        let token =
            request_service_account_token(&http, &format!("{}/token", server.url()), "signed-jwt")
                .unwrap();

        assert_eq!(token, "sa-token");
        mock.assert();
    }

    #[test]
    fn token_exchange_surfaces_error_body() {
        let mut server = mockito::Server::new();
        server
            .mock("POST", "/token")
            .with_status(400)
            .with_body(r#"{"error":"invalid_grant","error_description":"bad key"}"#)
            .create();

        let http = crate::http::client();
        let err =
            request_service_account_token(&http, &format!("{}/token", server.url()), "signed-jwt")
                .unwrap_err();

        assert!(err.to_string().contains("bad key"));
    }
}
