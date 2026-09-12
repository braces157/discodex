use std::{fs, io, path::PathBuf};

use serde::{Deserialize, Serialize};
use winreg::{RegKey, enums::HKEY_CURRENT_USER};

const EMBEDDED_DISCORD_APPLICATION_ID: &str = env!("DISCODEX_EMBEDDED_APPLICATION_ID");

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct AppConfig {
    pub discord_application_id: String,
    pub enabled: bool,
    pub start_with_windows: bool,
    pub foreground_detection: bool,
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            discord_application_id: String::new(),
            enabled: true,
            start_with_windows: false,
            foreground_detection: true,
        }
    }
}

impl AppConfig {
    pub fn path() -> io::Result<PathBuf> {
        std::env::var_os("APPDATA")
            .map(PathBuf::from)
            .map(|dir| dir.join("Discodex").join("config.toml"))
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "APPDATA is unavailable"))
    }

    pub fn load_or_create() -> Result<(Self, PathBuf), Box<dyn std::error::Error>> {
        let path = Self::path()?;
        if path.exists() {
            return Ok((Self::load(&path)?, path));
        }

        let config = Self::default();
        config.save(&path)?;
        Ok((config, path))
    }

    pub fn load(path: &std::path::Path) -> Result<Self, Box<dyn std::error::Error>> {
        let text = fs::read_to_string(path)?;
        Ok(toml::from_str(&text)?)
    }

    pub fn save(&self, path: &std::path::Path) -> io::Result<()> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let text = toml::to_string_pretty(self)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
        fs::write(path, text)
    }

    pub fn effective_discord_application_id(&self) -> String {
        effective_application_id(
            &self.discord_application_id,
            EMBEDDED_DISCORD_APPLICATION_ID,
        )
    }
}

fn effective_application_id(configured: &str, embedded: &str) -> String {
    let configured = configured.trim();
    if !configured.is_empty() {
        configured.to_string()
    } else {
        embedded.trim().to_string()
    }
}

pub fn set_start_with_windows(enabled: bool) -> io::Result<()> {
    let hkcu = RegKey::predef(HKEY_CURRENT_USER);
    let (run_key, _) = hkcu.create_subkey("Software\\Microsoft\\Windows\\CurrentVersion\\Run")?;

    if enabled {
        let executable = std::env::current_exe()?;
        let command = format!("\"{}\"", executable.display());
        run_key.set_value("Discodex", &command)?;
    } else {
        match run_key.delete_value("Discodex") {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_fields_use_defaults() {
        let config: AppConfig = toml::from_str("discord_application_id = '123'").unwrap();
        assert_eq!(config.discord_application_id, "123");
        assert!(config.enabled);
        assert!(!config.start_with_windows);
        assert!(config.foreground_detection);
    }

    #[test]
    fn config_id_overrides_embedded_id() {
        assert_eq!(effective_application_id("222", "111"), "222");
        assert_eq!(effective_application_id("", "111"), "111");
        assert_eq!(effective_application_id("   ", "111"), "111");
    }
}
