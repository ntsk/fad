use std::fs::File;
use std::path::Path;

use anyhow::{bail, Context, Result};
use indicatif::{ProgressBar, ProgressStyle};
use serde::Deserialize;

use crate::auth;
use crate::config::Config;

const BASE_URL: &str = "https://firebaseappdistribution.googleapis.com/v1";

#[derive(Deserialize)]
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

#[derive(Deserialize)]
pub struct ReleaseNotes {
    #[serde(default)]
    pub text: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ListReleasesResponse {
    #[serde(default)]
    releases: Vec<Release>,
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
    project_number: String,
    app_id: String,
}

impl Client {
    pub fn new(config: &Config) -> Result<Self> {
        let token = auth::get_access_token()?;
        let http = reqwest::blocking::Client::builder()
            .timeout(None)
            .connect_timeout(std::time::Duration::from_secs(10))
            .build()
            .context("failed to build the HTTP client")?;
        Ok(Self {
            http,
            token,
            project_number: config.project_number()?,
            app_id: config.app_id.clone(),
        })
    }

    fn releases_url(&self) -> String {
        format!(
            "{BASE_URL}/projects/{}/apps/{}/releases",
            self.project_number, self.app_id
        )
    }

    pub fn list_releases(&self) -> Result<Vec<Release>> {
        let resp = self
            .http
            .get(format!("{}?pageSize=100", self.releases_url()))
            .bearer_auth(&self.token)
            .send()
            .context("failed to reach the App Distribution API")?;
        let resp = check(resp)?;
        let list: ListReleasesResponse = resp.json().context("failed to parse the release list")?;
        Ok(list.releases)
    }

    pub fn get_release(&self, release_id: &str) -> Result<Release> {
        let resp = self
            .http
            .get(format!("{}/{release_id}", self.releases_url()))
            .bearer_auth(&self.token)
            .send()
            .context("failed to reach the App Distribution API")?;
        if resp.status() == reqwest::StatusCode::NOT_FOUND {
            bail!(
                "release not found: {release_id} (run `fad install --list` to see available IDs)"
            );
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
        let progress = match resp.content_length() {
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
        std::io::copy(&mut reader, &mut file).context("download failed")?;
        progress.finish_and_clear();
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
    bail!(
        "App Distribution API error ({status}): {}",
        api_error_message(&body)
    );
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

    fn release(value: serde_json::Value) -> Release {
        serde_json::from_value(value).unwrap()
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
}
