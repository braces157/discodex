use std::{fs, io, path::PathBuf};

use serde::{Deserialize, Serialize};
use winreg::{
    RegKey,
    enums::{HKEY_CURRENT_USER, KEY_SET_VALUE},
};

const EMBEDDED_DISCORD_APPLICATION_ID: &str = env!("DISCODEX_EMBEDDED_APPLICATION_ID");

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct AppConfig {
    pub discord_application_id: String,
    pub enabled: bool,
    pub start_with_windows: bool,
    pub foreground_detection: bool,
    pub privacy: PrivacySettings,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct PrivacySettings {
    pub share_reasoning: bool,
    pub show_project_names: bool,
    pub show_file_names: bool,
}

impl Default for PrivacySettings {
    fn default() -> Self {
        Self {
            share_reasoning: false,
            show_project_names: true,
            show_file_names: true,
        }
    }
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            discord_application_id: String::new(),
            enabled: true,
            start_with_windows: false,
            foreground_detection: true,
            privacy: PrivacySettings::default(),
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

    pub fn reload(&self, path: &std::path::Path) -> Result<Self, Box<dyn std::error::Error>> {
        self.with_updates(&fs::read_to_string(path)?)
    }

    fn with_updates(&self, text: &str) -> Result<Self, Box<dyn std::error::Error>> {
        let updates: toml::Table = toml::from_str(text)?;
        let mut merged = toml::Table::try_from(self)?;
        for (key, value) in updates {
            if key == "privacy"
                && let Some(updates) = value.as_table()
                && let Some(existing) = merged
                    .get_mut("privacy")
                    .and_then(toml::Value::as_table_mut)
            {
                existing.extend(updates.clone());
            } else {
                merged.insert(key, value);
            }
        }
        Ok(merged.try_into()?)
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
    const RUN_KEY: &str = "Software\\Microsoft\\Windows\\CurrentVersion\\Run";
    let hkcu = RegKey::predef(HKEY_CURRENT_USER);

    if enabled {
        let executable = std::env::current_exe()?;
        let command = format!("\"{}\"", executable.display());
        let (run_key, _) = hkcu.create_subkey_with_flags(RUN_KEY, KEY_SET_VALUE)?;
        run_key.set_value("Discodex", &command)?;
    } else {
        // A normal launch with startup disabled needs no registry write access
        // unless a previous Discodex startup entry actually needs removing.
        let read_key = match hkcu.open_subkey(RUN_KEY) {
            Ok(key) => key,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
            Err(error) => return Err(error),
        };
        match read_key.get_raw_value("Discodex") {
            Ok(_) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
            Err(error) => return Err(error),
        }
        let run_key = hkcu.open_subkey_with_flags(RUN_KEY, KEY_SET_VALUE)?;
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
        assert!(!config.privacy.share_reasoning);
        assert!(config.privacy.show_project_names);
        assert!(config.privacy.show_file_names);
    }

    #[test]
    fn privacy_options_round_trip_and_partial_tables_keep_safe_defaults() {
        let config: AppConfig = toml::from_str("[privacy]\nshow_file_names = false").unwrap();
        assert!(!config.privacy.share_reasoning);
        assert!(!config.privacy.show_file_names);
        assert!(config.privacy.show_project_names);
        let encoded = toml::to_string(&config).unwrap();
        assert_eq!(toml::from_str::<AppConfig>(&encoded).unwrap(), config);
    }

    #[test]
    fn partial_config_reloads_never_reset_existing_privacy_settings() {
        let config = AppConfig {
            enabled: false,
            privacy: PrivacySettings {
                share_reasoning: false,
                show_project_names: false,
                show_file_names: false,
            },
            ..AppConfig::default()
        };
        for text in [
            "",
            "[privacy]",
            "enabled = false",
            "[privacy]\nshare_reasoning = false",
        ] {
            assert_eq!(config.with_updates(text).unwrap(), config);
        }
        assert!(
            config
                .with_updates("[privacy]\nshow_project_names =")
                .is_err()
        );
        let updated = config
            .with_updates("[privacy]\nshow_file_names = true")
            .unwrap();
        assert!(updated.privacy.show_file_names);
        assert!(!updated.privacy.show_project_names);
        assert!(!updated.enabled);
    }

    #[test]
    fn config_id_overrides_embedded_id() {
        assert_eq!(effective_application_id("222", "111"), "222");
        assert_eq!(effective_application_id("", "111"), "111");
        assert_eq!(effective_application_id("   ", "111"), "111");
    }
}
