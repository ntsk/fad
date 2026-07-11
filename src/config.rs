use anyhow::{bail, Context, Result};
use serde::Deserialize;
use std::io::ErrorKind;
use std::path::PathBuf;

#[derive(Deserialize)]
pub struct Config {
    pub app_id: String,
    #[serde(default)]
    pub oauth: OauthConfig,
}

#[derive(Deserialize, Default)]
pub struct OauthConfig {
    pub client_id: Option<String>,
    pub client_secret: Option<String>,
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
    let config = toml::from_str(&text)
        .with_context(|| format!("failed to parse {}", path.display()))?;
    Ok(Some(config))
}

pub fn load() -> Result<Config> {
    match load_optional()? {
        Some(config) => Ok(config),
        None => bail!(
            "config file not found: {}\nCreate it with:\n\n  app_id = \"1:1234567890:android:0a1b2c3d4e5f\"",
            config_path()?.display()
        ),
    }
}
