use std::fs::File;
use std::path::Path;
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use indicatif::{ProgressBar, ProgressStyle};
use serde::Deserialize;

use crate::auth;
use crate::config::Config;

const BASE_URL: &str = "https://firebaseappdistribution.googleapis.com/v1";
const UPLOAD_BASE_URL: &str = "https://firebaseappdistribution.googleapis.com/upload/v1";
const UPLOAD_POLL_INTERVAL: Duration = Duration::from_secs(2);
const UPLOAD_POLL_TIMEOUT: Duration = Duration::from_secs(300);

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Release {
    pub name: String,
    #[serde(default)]
    pub display_version: String,
    #[serde(default)]
    pub build_version: String,
    #[serde(default)]
    pub create_time: String,
    #[serde(default)]
    pub binary_download_uri: String,
    #[serde(default)]
    pub release_notes: Option<ReleaseNotes>,
}

#[derive(Debug, Deserialize)]
pub struct ReleaseNotes {
    #[serde(default)]
    pub text: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ListReleasesResponse {
    #[serde(default)]
    releases: Vec<Release>,
    #[serde(default)]
    next_page_token: Option<String>,
}

#[derive(Deserialize)]
struct Operation {
    name: String,
    #[serde(default)]
    done: bool,
    #[serde(default)]
    error: Option<OperationError>,
    #[serde(default)]
    response: Option<UploadReleaseResponse>,
}

#[derive(Deserialize)]
struct OperationError {
    #[serde(default)]
    message: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct UploadReleaseResponse {
    #[serde(default)]
    release: Option<Release>,
}

impl Release {
    pub fn id(&self) -> &str {
        self.name.rsplit('/').next().unwrap_or(&self.name)
    }

    pub fn version(&self) -> String {
        match (
            self.display_version.is_empty(),
            self.build_version.is_empty(),
        ) {
            (false, false) => format!("{} ({})", self.display_version, self.build_version),
            (false, true) => self.display_version.clone(),
            (true, false) => self.build_version.clone(),
            (true, true) => "-".to_string(),
        }
    }

    pub fn notes(&self) -> &str {
        self.release_notes
            .as_ref()
            .map(|notes| notes.text.as_str())
            .unwrap_or("")
    }

    pub fn binary_type(&self) -> &'static str {
        let path = self
            .binary_download_uri
            .split(['?', '#'])
            .next()
            .unwrap_or("")
            .to_ascii_lowercase();
        if path.ends_with(".apk") {
            "APK"
        } else if path.ends_with(".aab") {
            "AAB"
        } else {
            "-"
        }
    }
}

pub struct Client {
    http: reqwest::blocking::Client,
    token: String,
    base_url: String,
    upload_base_url: String,
    project_number: String,
    app_id: String,
}

impl Client {
    pub fn new(config: &Config) -> Result<Self> {
        let token = auth::get_access_token()?;
        Self::with_token(
            config,
            token,
            BASE_URL.to_string(),
            UPLOAD_BASE_URL.to_string(),
        )
    }

    fn with_token(
        config: &Config,
        token: String,
        base_url: String,
        upload_base_url: String,
    ) -> Result<Self> {
        let http = reqwest::blocking::Client::builder()
            .timeout(None)
            .connect_timeout(Duration::from_secs(10))
            .build()
            .context("failed to build the HTTP client")?;
        Ok(Self {
            http,
            token,
            base_url,
            upload_base_url,
            project_number: config.project_number()?,
            app_id: config.app_id.clone(),
        })
    }

    fn releases_url(&self) -> String {
        format!(
            "{}/projects/{}/apps/{}/releases",
            self.base_url, self.project_number, self.app_id
        )
    }

    fn upload_url(&self) -> String {
        format!(
            "{}/projects/{}/apps/{}/releases:upload",
            self.upload_base_url, self.project_number, self.app_id
        )
    }

    pub fn list_releases(&self) -> Result<Vec<Release>> {
        let mut releases = Vec::new();
        let mut page_token = String::new();
        loop {
            let mut request = self
                .http
                .get(self.releases_url())
                .query(&[("pageSize", "100")])
                .bearer_auth(&self.token);
            if !page_token.is_empty() {
                request = request.query(&[("pageToken", page_token.as_str())]);
            }
            let resp = request
                .send()
                .context("failed to reach the App Distribution API")?;
            let resp = check(resp)?;
            let list: ListReleasesResponse =
                resp.json().context("failed to parse the release list")?;
            releases.extend(list.releases);
            match list.next_page_token {
                Some(next) if !next.is_empty() => page_token = next,
                _ => break,
            }
        }
        Ok(releases)
    }

    pub fn get_release(&self, release_id: &str) -> Result<Release> {
        let resp = self
            .http
            .get(format!("{}/{release_id}", self.releases_url()))
            .bearer_auth(&self.token)
            .send()
            .context("failed to reach the App Distribution API")?;
        if resp.status() == reqwest::StatusCode::NOT_FOUND {
            bail!("release not found: {release_id} (run `fad releases` to see available IDs)");
        }
        let resp = check(resp)?;
        resp.json().context("failed to parse the release")
    }

    pub fn download(&self, release: &Release, dest: &Path) -> Result<()> {
        if release.binary_download_uri.is_empty() {
            bail!("release {} has no binary download URI", release.id());
        }
        let resp = self
            .http
            .get(release.binary_download_uri.as_str())
            .send()
            .context("failed to start the binary download")?;
        let resp = check(resp)?;
        let expected = resp.content_length();
        let progress = match expected {
            Some(total) => ProgressBar::new(total).with_style(
                ProgressStyle::with_template(
                    "{bar:40} {bytes}/{total_bytes} ({bytes_per_sec}, {eta})",
                )?
                .progress_chars("=> "),
            ),
            None => ProgressBar::new_spinner(),
        };
        let mut reader = progress.wrap_read(resp);
        let mut file =
            File::create(dest).with_context(|| format!("failed to create {}", dest.display()))?;
        let written = std::io::copy(&mut reader, &mut file).context("download failed")?;
        progress.finish_and_clear();
        if let Some(expected) = expected {
            if written != expected {
                bail!("download was truncated ({written} of {expected} bytes)");
            }
        }
        Ok(())
    }

    pub fn upload_release(&self, path: &Path) -> Result<Release> {
        let bytes =
            std::fs::read(path).with_context(|| format!("failed to read {}", path.display()))?;
        let file_name = path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("app.bin");
        let resp = self
            .http
            .post(self.upload_url())
            .bearer_auth(&self.token)
            .header("X-Goog-Upload-Protocol", "raw")
            .header("X-Goog-Upload-File-Name", file_name)
            .header("Content-Type", "application/octet-stream")
            .body(bytes)
            .send()
            .context("failed to reach the App Distribution upload API")?;
        let resp = check(resp)?;
        let operation: Operation = resp.json().context("failed to parse the upload response")?;
        self.await_operation(operation)
    }

    fn await_operation(&self, mut operation: Operation) -> Result<Release> {
        let deadline = Instant::now() + UPLOAD_POLL_TIMEOUT;
        loop {
            if let Some(error) = operation.error {
                bail!("the upload could not be processed: {}", error.message);
            }
            if operation.done {
                return operation
                    .response
                    .and_then(|response| response.release)
                    .context("the upload finished but no release was returned");
            }
            if Instant::now() >= deadline {
                bail!("timed out waiting for the uploaded binary to be processed");
            }
            std::thread::sleep(UPLOAD_POLL_INTERVAL);
            let resp = self
                .http
                .get(format!("{}/{}", self.base_url, operation.name))
                .bearer_auth(&self.token)
                .send()
                .context("failed to reach the App Distribution API")?;
            let resp = check(resp)?;
            operation = resp.json().context("failed to parse the upload status")?;
        }
    }

    pub fn set_release_notes(&self, release_name: &str, notes: &str) -> Result<()> {
        let resp = self
            .http
            .patch(format!("{}/{release_name}", self.base_url))
            .query(&[("updateMask", "release_notes.text")])
            .bearer_auth(&self.token)
            .json(&serde_json::json!({ "releaseNotes": { "text": notes } }))
            .send()
            .context("failed to reach the App Distribution API")?;
        check(resp)?;
        Ok(())
    }
}

pub(crate) fn check(resp: reqwest::blocking::Response) -> Result<reqwest::blocking::Response> {
    let status = resp.status();
    if status.is_success() {
        return Ok(resp);
    }
    if status == reqwest::StatusCode::UNAUTHORIZED {
        bail!("authentication failed; run `fad login` again");
    }
    let body = resp.text().unwrap_or_default();
    let message = api_error_message(&body);
    if status == reqwest::StatusCode::FORBIDDEN {
        bail!(
            "permission denied ({status}): {message}\nCheck that your account has access to the selected project and app"
        );
    }
    bail!("API error ({status}): {message}");
}

fn api_error_message(body: &str) -> String {
    serde_json::from_str::<serde_json::Value>(body)
        .ok()
        .and_then(|v| v.get("error")?.get("message")?.as_str().map(str::to_string))
        .unwrap_or_else(|| body.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use mockito::Matcher;

    fn release(value: serde_json::Value) -> Release {
        serde_json::from_value(value).unwrap()
    }

    fn test_client(server: &mockito::ServerGuard) -> Client {
        let config: Config = toml::from_str("app_id = \"1:1:android:a\"").unwrap();
        Client::with_token(
            &config,
            "test-token".to_string(),
            server.url(),
            server.url(),
        )
        .unwrap()
    }

    #[test]
    fn list_releases_follows_pagination() {
        let mut server = mockito::Server::new();
        let page1 = server
            .mock("GET", "/projects/1/apps/1:1:android:a/releases")
            .match_query(Matcher::UrlEncoded("pageSize".into(), "100".into()))
            .match_header("authorization", "Bearer test-token")
            .with_body(
                r#"{"releases":[{"name":"projects/1/apps/a/releases/r1"}],"nextPageToken":"t2"}"#,
            )
            .create();
        let page2 = server
            .mock("GET", "/projects/1/apps/1:1:android:a/releases")
            .match_query(Matcher::AllOf(vec![
                Matcher::UrlEncoded("pageSize".into(), "100".into()),
                Matcher::UrlEncoded("pageToken".into(), "t2".into()),
            ]))
            .with_body(r#"{"releases":[{"name":"projects/1/apps/a/releases/r2"}]}"#)
            .create();

        let releases = test_client(&server).list_releases().unwrap();

        let ids: Vec<&str> = releases.iter().map(|r| r.id()).collect();
        assert_eq!(ids, ["r1", "r2"]);
        page1.assert();
        page2.assert();
    }

    #[test]
    fn get_release_maps_not_found_to_helpful_error() {
        let mut server = mockito::Server::new();
        server
            .mock("GET", "/projects/1/apps/1:1:android:a/releases/xyz")
            .with_status(404)
            .create();

        let err = test_client(&server).get_release("xyz").unwrap_err();

        assert!(err.to_string().contains("release not found: xyz"));
        assert!(err.to_string().contains("fad releases"));
    }

    #[test]
    fn unauthorized_suggests_logging_in_again() {
        let mut server = mockito::Server::new();
        server
            .mock("GET", "/projects/1/apps/1:1:android:a/releases")
            .match_query(Matcher::Any)
            .with_status(401)
            .create();

        let err = test_client(&server).list_releases().unwrap_err();

        assert!(err.to_string().contains("fad login"));
    }

    #[test]
    fn forbidden_includes_permission_hint() {
        let mut server = mockito::Server::new();
        server
            .mock("GET", "/projects/1/apps/1:1:android:a/releases")
            .match_query(Matcher::Any)
            .with_status(403)
            .with_body(r#"{"error":{"message":"The caller does not have permission"}}"#)
            .create();

        let err = test_client(&server).list_releases().unwrap_err();

        let message = err.to_string();
        assert!(message.contains("permission denied"));
        assert!(message.contains("The caller does not have permission"));
    }

    #[test]
    fn server_error_extracts_api_message() {
        let mut server = mockito::Server::new();
        server
            .mock("GET", "/projects/1/apps/1:1:android:a/releases")
            .match_query(Matcher::Any)
            .with_status(500)
            .with_body(r#"{"error":{"message":"boom"}}"#)
            .create();

        let err = test_client(&server).list_releases().unwrap_err();

        assert!(err.to_string().contains("API error (500"));
        assert!(err.to_string().contains("boom"));
    }

    #[test]
    fn download_writes_binary_to_dest() {
        let mut server = mockito::Server::new();
        server
            .mock("GET", "/binaries/app.apk")
            .with_body("binary-content")
            .create();
        let release = release(serde_json::json!({
            "name": "projects/1/apps/a/releases/r1",
            "binaryDownloadUri": format!("{}/binaries/app.apk", server.url())
        }));
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("out.bin");

        test_client(&server).download(&release, &dest).unwrap();

        assert_eq!(std::fs::read_to_string(&dest).unwrap(), "binary-content");
    }

    #[test]
    fn download_without_uri_fails_early() {
        let server = mockito::Server::new();
        let release = release(serde_json::json!({ "name": "projects/1/apps/a/releases/r1" }));
        let dir = tempfile::tempdir().unwrap();

        let err = test_client(&server)
            .download(&release, &dir.path().join("out.bin"))
            .unwrap_err();

        assert!(err.to_string().contains("no binary download URI"));
    }

    #[test]
    fn extracts_release_id_from_resource_name() {
        let release = release(serde_json::json!({
            "name": "projects/123/apps/1:123:android:abc/releases/r1"
        }));
        assert_eq!(release.id(), "r1");
    }

    #[test]
    fn formats_version_from_display_and_build() {
        let release = release(serde_json::json!({
            "name": "n",
            "displayVersion": "1.2.3",
            "buildVersion": "45"
        }));
        assert_eq!(release.version(), "1.2.3 (45)");
    }

    #[test]
    fn falls_back_when_versions_are_missing() {
        let release = release(serde_json::json!({ "name": "n" }));
        assert_eq!(release.version(), "-");
        assert_eq!(release.notes(), "");
    }

    #[test]
    fn detects_binary_type_from_download_uri() {
        let apk = release(serde_json::json!({
            "name": "n",
            "binaryDownloadUri": "https://example.com/binaries/abc/app.apk?token=x.y"
        }));
        assert_eq!(apk.binary_type(), "APK");
        let aab = release(serde_json::json!({
            "name": "n",
            "binaryDownloadUri": "https://example.com/binaries/abc/app.aab"
        }));
        assert_eq!(aab.binary_type(), "AAB");
        let unknown = release(serde_json::json!({ "name": "n" }));
        assert_eq!(unknown.binary_type(), "-");
    }

    #[test]
    fn extracts_api_error_message_from_json_body() {
        let body = "{\"error\": {\"message\": \"boom\"}}";
        assert_eq!(api_error_message(body), "boom");
        assert_eq!(api_error_message("plain"), "plain");
    }

    #[test]
    fn upload_release_uploads_then_polls_operation() {
        let mut server = mockito::Server::new();
        let upload = server
            .mock("POST", "/projects/1/apps/1:1:android:a/releases:upload")
            .match_header("authorization", "Bearer test-token")
            .match_header("x-goog-upload-protocol", "raw")
            .match_header("x-goog-upload-file-name", "app.apk")
            .match_body("apk-bytes")
            .with_body(
                r#"{"name":"projects/1/apps/1:1:android:a/releases/-/operations/op1","done":false}"#,
            )
            .create();
        let poll = server
            .mock(
                "GET",
                "/projects/1/apps/1:1:android:a/releases/-/operations/op1",
            )
            .with_body(
                r#"{"name":"op1","done":true,"response":{"release":{"name":"projects/1/apps/a/releases/r7","displayVersion":"1.2.3","buildVersion":"45"}}}"#,
            )
            .create();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("app.apk");
        std::fs::write(&path, "apk-bytes").unwrap();

        let release = test_client(&server).upload_release(&path).unwrap();

        assert_eq!(release.id(), "r7");
        assert_eq!(release.version(), "1.2.3 (45)");
        upload.assert();
        poll.assert();
    }

    #[test]
    fn upload_release_reports_operation_error() {
        let mut server = mockito::Server::new();
        server
            .mock("POST", "/projects/1/apps/1:1:android:a/releases:upload")
            .with_body(r#"{"name":"op1","done":true,"error":{"message":"invalid binary"}}"#)
            .create();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("app.apk");
        std::fs::write(&path, "apk-bytes").unwrap();

        let err = test_client(&server).upload_release(&path).unwrap_err();

        assert!(err.to_string().contains("invalid binary"));
    }

    #[test]
    fn set_release_notes_sends_patch_with_update_mask() {
        let mut server = mockito::Server::new();
        let patch = server
            .mock("PATCH", "/projects/1/apps/a/releases/r7")
            .match_query(Matcher::UrlEncoded(
                "updateMask".into(),
                "release_notes.text".into(),
            ))
            .match_body(Matcher::JsonString(
                r#"{"releaseNotes":{"text":"Fix login"}}"#.into(),
            ))
            .with_body("{}")
            .create();

        test_client(&server)
            .set_release_notes("projects/1/apps/a/releases/r7", "Fix login")
            .unwrap();

        patch.assert();
    }
}
