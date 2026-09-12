use std::{env, fs, path::PathBuf};

const DEFAULT_APPLICATION_ID: &str = "1548194622251999232";

fn main() {
    println!("cargo:rerun-if-env-changed=DISCODEX_DISCORD_APPLICATION_ID");
    println!("cargo:rerun-if-changed=assets/discodex.ico");

    let application_id = env::var("DISCODEX_DISCORD_APPLICATION_ID")
        .ok()
        .filter(|value| valid_application_id(value))
        .or_else(read_local_config_id)
        .unwrap_or_else(|| DEFAULT_APPLICATION_ID.to_string());

    println!("cargo:rustc-env=DISCODEX_EMBEDDED_APPLICATION_ID={application_id}");

    if cfg!(windows) && PathBuf::from("assets/discodex.ico").exists() {
        let mut resource = winresource::WindowsResource::new();
        resource.set_icon("assets/discodex.ico");
        resource.compile().expect("failed to embed Discodex icon");
    }
}

fn read_local_config_id() -> Option<String> {
    let appdata = env::var_os("APPDATA")?;
    let path = PathBuf::from(appdata).join("Discodex").join("config.toml");
    println!("cargo:rerun-if-changed={}", path.display());

    let text = fs::read_to_string(path).ok()?;
    text.lines().find_map(|line| {
        let (key, value) = line.split_once('=')?;
        if key.trim() != "discord_application_id" {
            return None;
        }
        let value = value.trim().trim_matches('"').trim_matches('\'');
        valid_application_id(value).then(|| value.to_string())
    })
}

fn valid_application_id(value: &str) -> bool {
    let value = value.trim();
    !value.is_empty() && value.len() <= 32 && value.bytes().all(|byte| byte.is_ascii_digit())
}
