use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::io::ErrorKind;
use std::path::PathBuf;

#[derive(Serialize, Deserialize)]
pub struct Config {
    pub app_id: String,
    #[serde(default, skip_serializing_if = "OauthConfig::is_empty")]
    pub oauth: OauthConfig,
}

#[derive(Serialize, Deserialize, Default)]
pub struct OauthConfig {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub client_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub client_secret: Option<String>,
}

impl OauthConfig {
    fn is_empty(&self) -> bool {
        self.client_id.is_none() && self.client_secret.is_none()
    }
}

impl Config {
    pub fn project_number(&self) -> Result<String> {
        let parts: Vec<&str> = self.app_id.split(':').collect();
        match parts.as_slice() {
            [_, number, _, _]
                if !number.is_empty() && number.chars().all(|c| c.is_ascii_digit()) =>
            {
                Ok((*number).to_string())
            }
            _ => bail!(
                "invalid app_id \"{}\": expected format like 1:1234567890:android:0a1b2c3d4e5f",
                self.app_id
            ),
        }
    }
}

pub fn config_dir() -> Result<PathBuf> {
    if let Some(dir) = std::env::var_os("XDG_CONFIG_HOME").filter(|v| !v.is_empty()) {
        return Ok(PathBuf::from(dir).join("fad"));
    }
    let home = dirs::home_dir().context("could not determine home directory")?;
    Ok(home.join(".config").join("fad"))
}

pub fn config_path() -> Result<PathBuf> {
    Ok(config_dir()?.join("config.toml"))
}

pub fn load_optional() -> Result<Option<Config>> {
    let path = config_path()?;
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(e) if e.kind() == ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e).context(format!("failed to read {}", path.display())),
    };
    let config =
        toml::from_str(&text).with_context(|| format!("failed to parse {}", path.display()))?;
    Ok(Some(config))
}

pub fn load() -> Result<Config> {
    match load_optional()? {
        Some(config) => Ok(config),
        None => bail!(
            "config file not found: {}\nRun `fad login` to select an app, or create the file with:\n\n  app_id = \"1:1234567890:android:0a1b2c3d4e5f\"",
            config_path()?.display()
        ),
    }
}

pub fn save(config: &Config) -> Result<()> {
    let dir = config_dir()?;
    std::fs::create_dir_all(&dir).with_context(|| format!("failed to create {}", dir.display()))?;
    let path = config_path()?;
    std::fs::write(&path, to_toml(config)?)
        .with_context(|| format!("failed to write {}", path.display()))?;
    Ok(())
}

fn to_toml(config: &Config) -> Result<String> {
    toml::to_string(config).context("failed to serialize the config")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_project_number_from_app_id() {
        let config: Config =
            toml::from_str("app_id = \"1:1234567890:android:0a1b2c3d4e5f\"").unwrap();
        assert_eq!(config.project_number().unwrap(), "1234567890");
    }

    #[test]
    fn rejects_invalid_app_id() {
        let config: Config = toml::from_str("app_id = \"not-an-app-id\"").unwrap();
        assert!(config.project_number().is_err());
    }

    #[test]
    fn parses_optional_oauth_section() {
        let config: Config = toml::from_str(
            "app_id = \"1:1:android:a\"\n[oauth]\nclient_id = \"cid\"\nclient_secret = \"cs\"",
        )
        .unwrap();
        assert_eq!(config.oauth.client_id.as_deref(), Some("cid"));
        assert_eq!(config.oauth.client_secret.as_deref(), Some("cs"));
    }

    #[test]
    fn serializes_config_without_empty_oauth_section() {
        let config = Config {
            app_id: "1:1:android:a".to_string(),
            oauth: OauthConfig::default(),
        };
        assert_eq!(to_toml(&config).unwrap(), "app_id = \"1:1:android:a\"\n");
    }

    #[test]
    fn serialized_config_preserves_oauth_overrides() {
        let config = Config {
            app_id: "1:1:android:a".to_string(),
            oauth: OauthConfig {
                client_id: Some("cid".to_string()),
                client_secret: Some("cs".to_string()),
            },
        };
        let reparsed: Config = toml::from_str(&to_toml(&config).unwrap()).unwrap();
        assert_eq!(reparsed.app_id, "1:1:android:a");
        assert_eq!(reparsed.oauth.client_id.as_deref(), Some("cid"));
        assert_eq!(reparsed.oauth.client_secret.as_deref(), Some("cs"));
    }
}
