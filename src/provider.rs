use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Provider {
    Codex,
    ClaudeCode,
    ChatGpt,
    Claude,
    Gemini,
    Cursor,
    Windsurf,
    Antigravity,
    Trae,
    Kiro,
    Zed,
    OpenCode,
    Aider,
    GitHubCopilot,
    Cline,
    RooCode,
    Continue,
    Perplexity,
    MicrosoftCopilot,
    Poe,
}

impl Provider {
    #[cfg(test)]
    pub const ALL: [Self; 20] = [
        Self::Codex,
        Self::ClaudeCode,
        Self::ChatGpt,
        Self::Claude,
        Self::Gemini,
        Self::Cursor,
        Self::Windsurf,
        Self::Antigravity,
        Self::Trae,
        Self::Kiro,
        Self::Zed,
        Self::OpenCode,
        Self::Aider,
        Self::GitHubCopilot,
        Self::Cline,
        Self::RooCode,
        Self::Continue,
        Self::Perplexity,
        Self::MicrosoftCopilot,
        Self::Poe,
    ];

    // Use product artwork, not parent-company avatars (e.g. Google or Microsoft).
    // Pin the icon package so upstream releases cannot silently change the images.
    pub const fn logo_url(self) -> &'static str {
        match self {
            Self::Codex => {
                "https://raw.githubusercontent.com/braces157/discodex/main/assets/codex-bundle-blue.png"
            }
            Self::ClaudeCode => {
                "https://cdn.jsdelivr.net/npm/@lobehub/icons-static-png@1.97.0/dark/claudecode-color.png"
            }
            Self::ChatGpt => {
                "https://cdn.jsdelivr.net/npm/@lobehub/icons-static-png@1.97.0/dark/openai.png"
            }
            Self::Claude => {
                "https://cdn.jsdelivr.net/npm/@lobehub/icons-static-png@1.97.0/dark/claude-color.png"
            }
            Self::Gemini => {
                "https://cdn.jsdelivr.net/npm/@lobehub/icons-static-png@1.97.0/dark/gemini-color.png"
            }
            Self::Cursor => {
                "https://cdn.jsdelivr.net/npm/@lobehub/icons-static-png@1.97.0/dark/cursor.png"
            }
            Self::Windsurf => {
                "https://cdn.jsdelivr.net/npm/@lobehub/icons-static-png@1.97.0/dark/windsurf.png"
            }
            Self::Antigravity => {
                "https://cdn.jsdelivr.net/npm/@lobehub/icons-static-png@1.97.0/dark/antigravity-color.png"
            }
            Self::Trae => {
                "https://cdn.jsdelivr.net/npm/@lobehub/icons-static-png@1.97.0/dark/trae-color.png"
            }
            Self::Kiro => {
                "https://cdn.jsdelivr.net/npm/@lobehub/icons-static-png@1.97.0/dark/kiro-color.png"
            }
            Self::Zed => "https://avatars.githubusercontent.com/u/79345384?v=4",
            Self::OpenCode => {
                "https://cdn.jsdelivr.net/npm/@lobehub/icons-static-png@1.97.0/dark/opencode.png"
            }
            Self::Aider => "https://avatars.githubusercontent.com/u/172139148?v=4",
            Self::GitHubCopilot => {
                "https://cdn.jsdelivr.net/npm/@lobehub/icons-static-png@1.97.0/dark/githubcopilot.png"
            }
            Self::Cline => {
                "https://cdn.jsdelivr.net/npm/@lobehub/icons-static-png@1.97.0/dark/cline.png"
            }
            Self::RooCode => {
                "https://cdn.jsdelivr.net/npm/@lobehub/icons-static-png@1.97.0/dark/roocode.png"
            }
            Self::Continue => "https://avatars.githubusercontent.com/u/127876214?v=4",
            Self::Perplexity => {
                "https://cdn.jsdelivr.net/npm/@lobehub/icons-static-png@1.97.0/dark/perplexity-color.png"
            }
            Self::MicrosoftCopilot => {
                "https://cdn.jsdelivr.net/npm/@lobehub/icons-static-png@1.97.0/dark/copilot-color.png"
            }
            Self::Poe => {
                "https://cdn.jsdelivr.net/npm/@lobehub/icons-static-png@1.97.0/dark/poe-color.png"
            }
        }
    }

    #[allow(dead_code)]
    pub const fn image_url(self) -> &'static str {
        self.logo_url()
    }

    pub fn display_name(self) -> &'static str {
        match self {
            Self::Codex => "OpenAI Codex",
            Self::ClaudeCode => "Claude Code",
            Self::ChatGpt => "ChatGPT",
            Self::Claude => "Claude",
            Self::Gemini => "Google Gemini",
            Self::Cursor => "Cursor",
            Self::Windsurf => "Windsurf",
            Self::Antigravity => "Google Antigravity",
            Self::Trae => "Trae",
            Self::Kiro => "Kiro",
            Self::Zed => "Zed AI",
            Self::OpenCode => "OpenCode",
            Self::Aider => "Aider",
            Self::GitHubCopilot => "GitHub Copilot",
            Self::Cline => "Cline",
            Self::RooCode => "Roo Code",
            Self::Continue => "Continue",
            Self::Perplexity => "Perplexity",
            Self::MicrosoftCopilot => "Microsoft Copilot",
            Self::Poe => "Poe",
        }
    }

    pub fn has_session_tracking(self) -> bool {
        matches!(self, Self::Codex | Self::ClaudeCode | Self::Antigravity)
    }

    pub fn default_activity(self) -> &'static str {
        match self {
            Self::Codex | Self::ClaudeCode | Self::Antigravity | Self::OpenCode | Self::Aider => {
                "Working with coding agent"
            }
            Self::Cursor
            | Self::Windsurf
            | Self::Trae
            | Self::Kiro
            | Self::Zed
            | Self::GitHubCopilot
            | Self::Cline
            | Self::RooCode
            | Self::Continue => "Working with AI editor",
            _ => "Using AI assistant",
        }
    }

    pub fn from_executable(executable: &str) -> Option<Self> {
        let name = executable.to_ascii_lowercase();
        let name = name.strip_suffix(".exe").unwrap_or(&name);
        match name {
            "chatgpt" => Some(Self::ChatGpt),
            "claude" => Some(Self::Claude),
            "claude-code" | "claudecode" => Some(Self::ClaudeCode),
            "gemini" => Some(Self::Gemini),
            "cursor" => Some(Self::Cursor),
            "windsurf" => Some(Self::Windsurf),
            "antigravity" => Some(Self::Antigravity),
            "codex" => Some(Self::Codex),
            "trae" => Some(Self::Trae),
            "kiro" => Some(Self::Kiro),
            "zed" => Some(Self::Zed),
            "opencode" => Some(Self::OpenCode),
            "aider" => Some(Self::Aider),
            "githubcopilot" | "github-copilot" | "copilot-chat" => Some(Self::GitHubCopilot),
            "cline" => Some(Self::Cline),
            "roo" | "roocode" | "roo-code" => Some(Self::RooCode),
            "continue" => Some(Self::Continue),
            "perplexity" => Some(Self::Perplexity),
            "copilot" | "microsoftcopilot" => Some(Self::MicrosoftCopilot),
            "poe" => Some(Self::Poe),
            _ => None,
        }
    }

    pub fn from_window(executable: &str, title: &str) -> Option<Self> {
        if let Some(provider) = Self::from_executable(executable) {
            return Some(provider);
        }

        let title = title.trim().to_ascii_lowercase();
        if title.is_empty() {
            return None;
        }

        // Terminal / CLI coding agents
        if title.contains("claude code") || title_starts_or_is(&title, "claude-code") {
            return Some(Self::ClaudeCode);
        }
        if title_starts_or_is(&title, "aider") || title_ends_or_is(&title, "aider") {
            return Some(Self::Aider);
        }
        if title_starts_or_is(&title, "opencode") || title_ends_or_is(&title, "opencode") {
            return Some(Self::OpenCode);
        }
        if title.contains("openai codex") {
            return Some(Self::Codex);
        }

        // Web AI assistants (ChatGPT, Claude, Gemini, Perplexity, Poe)
        if title.contains("chatgpt.com")
            || title.contains("chat.openai.com")
            || title_starts_or_is(&title, "chatgpt")
            || title_ends_or_is(&title, "chatgpt")
        {
            return Some(Self::ChatGpt);
        }
        if title.contains("claude.ai") {
            return Some(Self::Claude);
        }
        if title.contains("gemini.google.com")
            || title.contains("google ai studio")
            || title_starts_or_is(&title, "google gemini")
            || title_ends_or_is(&title, "google gemini")
        {
            return Some(Self::Gemini);
        }
        if title.contains("perplexity.ai")
            || title_starts_or_is(&title, "perplexity")
            || title_ends_or_is(&title, "perplexity")
        {
            return Some(Self::Perplexity);
        }
        if title.contains("poe.com") {
            return Some(Self::Poe);
        }

        // Copilot
        if title.contains("copilot.microsoft.com") || title.contains("microsoft copilot") {
            return Some(Self::MicrosoftCopilot);
        }
        if title.contains("github copilot") || title.contains("copilot chat") {
            return Some(Self::GitHubCopilot);
        }

        // Dedicated AI Editors & IDEs (match as whole app name or title suffix/prefix)
        if title_ends_or_is(&title, "cursor") {
            return Some(Self::Cursor);
        }
        if title_ends_or_is(&title, "windsurf") {
            return Some(Self::Windsurf);
        }
        if title_ends_or_is(&title, "trae") {
            return Some(Self::Trae);
        }
        if title_ends_or_is(&title, "kiro") {
            return Some(Self::Kiro);
        }
        if title_ends_or_is(&title, "zed") {
            return Some(Self::Zed);
        }
        if title.contains("roo code") || title.contains("roocode") {
            return Some(Self::RooCode);
        }
        if title_starts_or_is(&title, "cline") || title_ends_or_is(&title, "cline") {
            return Some(Self::Cline);
        }
        if title.contains("continue.dev")
            || title_starts_or_is(&title, "continue")
            || title_ends_or_is(&title, "continue")
        {
            return Some(Self::Continue);
        }

        None
    }
}

fn title_ends_or_is(title: &str, name: &str) -> bool {
    if title == name {
        return true;
    }
    for sep in [" - ", " — ", " – ", " | ", " • "] {
        if title.ends_with(&format!("{sep}{name}")) {
            return true;
        }
    }
    false
}

fn title_starts_or_is(title: &str, name: &str) -> bool {
    if title == name {
        return true;
    }
    for sep in [" - ", " — ", " – ", ": ", " | ", " • "] {
        if title.starts_with(&format!("{name}{sep}")) {
            return true;
        }
    }
    false
}

#[derive(Debug, Clone)]
pub struct SessionSource {
    pub provider: Provider,
    pub root: PathBuf,
}

impl SessionSource {
    pub fn matches(&self, path: &Path) -> bool {
        let norm_path = normalize_path(path);
        let norm_root = normalize_path(&self.root);
        norm_path.starts_with(&norm_root) && is_session_log(self.provider, path)
    }
}

fn normalize_path(path: &Path) -> PathBuf {
    let s = path.to_string_lossy();
    let s = s.strip_prefix(r"\\?\").unwrap_or(&s);
    PathBuf::from(s.replace('/', "\\").to_ascii_lowercase())
}

pub fn session_sources() -> Vec<SessionSource> {
    let Some(home) = dirs::home_dir() else {
        return Vec::new();
    };

    vec![
        SessionSource {
            provider: Provider::Codex,
            root: home.join(".codex").join("sessions"),
        },
        SessionSource {
            provider: Provider::ClaudeCode,
            root: home.join(".claude").join("projects"),
        },
        SessionSource {
            provider: Provider::Antigravity,
            root: home.join(".gemini").join("antigravity").join("brain"),
        },
    ]
}

pub fn is_session_log(provider: Provider, path: &Path) -> bool {
    let filename = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("");
    let lower_filename = filename.to_ascii_lowercase();
    if !lower_filename.ends_with(".jsonl") {
        return false;
    }

    match provider {
        Provider::Codex => lower_filename.starts_with("rollout-"),
        Provider::ClaudeCode => true,
        Provider::Antigravity => lower_filename == "transcript.jsonl",
        _ => false,
    }
}

pub fn source_for_path<'a>(sources: &'a [SessionSource], path: &Path) -> Option<&'a SessionSource> {
    sources.iter().find(|source| source.matches(path))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_common_ai_executables() {
        assert_eq!(
            Provider::from_executable("Cursor.exe"),
            Some(Provider::Cursor)
        );
        assert_eq!(
            Provider::from_executable("chatgpt.exe"),
            Some(Provider::ChatGpt)
        );
        assert_eq!(Provider::from_executable("notepad.exe"), None);
        assert_eq!(
            Provider::from_window("chrome.exe", "ChatGPT - Google Chrome"),
            Some(Provider::ChatGpt)
        );
        assert_eq!(
            Provider::from_window("Code.exe", "GitHub Copilot Chat - Visual Studio Code"),
            Some(Provider::GitHubCopilot)
        );
        assert_eq!(
            Provider::from_window("Code.exe", "Discodex — Zed"),
            Some(Provider::Zed)
        );
    }

    #[test]
    fn avoids_false_positive_window_titles() {
        // Generic words containing 'cline', 'zed', 'continue', 'cursor', etc.
        assert_eq!(
            Provider::from_window("chrome.exe", "GitLab CI Pipeline - Google Chrome"),
            None
        );
        assert_eq!(
            Provider::from_window("Code.exe", "optimized.rs - Discodex - Visual Studio Code"),
            None
        );
        assert_eq!(
            Provider::from_window("chrome.exe", "Click here to continue - Google Chrome"),
            None
        );
        assert_eq!(
            Provider::from_window("chrome.exe", "How to use cursor in CSS - Google Chrome"),
            None
        );
        assert_eq!(
            Provider::from_window("chrome.exe", "First aider handbook - Google Chrome"),
            None
        );
        assert_eq!(
            Provider::from_window("chrome.exe", "Zed text editor review - Google Chrome"),
            None
        );
        assert_eq!(
            Provider::from_window("chrome.exe", "A steep decline in sales - Google Chrome"),
            None
        );
    }

    #[test]
    fn matches_legitimate_editor_and_browser_titles() {
        assert_eq!(
            Provider::from_window("Code.exe", "Cargo.toml - Cursor"),
            Some(Provider::Cursor)
        );
        assert_eq!(
            Provider::from_window("Code.exe", "main.rs - Windsurf"),
            Some(Provider::Windsurf)
        );
        assert_eq!(
            Provider::from_window("chrome.exe", "https://chatgpt.com/c/123 - Google Chrome"),
            Some(Provider::ChatGpt)
        );
        assert_eq!(
            Provider::from_window("msedge.exe", "Claude.ai - Chat - Microsoft Edge"),
            Some(Provider::Claude)
        );
        assert_eq!(
            Provider::from_window("firefox.exe", "Google AI Studio - Mozilla Firefox"),
            Some(Provider::Gemini)
        );
    }

    #[test]
    fn codex_requires_rollout_jsonl_but_claude_accepts_jsonl() {
        assert!(is_session_log(
            Provider::Codex,
            Path::new("rollout-1.jsonl")
        ));
        assert!(!is_session_log(Provider::Codex, Path::new("session.jsonl")));
        assert!(is_session_log(
            Provider::ClaudeCode,
            Path::new("session.jsonl")
        ));
        assert!(is_session_log(
            Provider::Antigravity,
            Path::new("transcript.jsonl")
        ));
        assert!(!is_session_log(
            Provider::Antigravity,
            Path::new("transcript_full.jsonl")
        ));
        assert!(!is_session_log(
            Provider::Antigravity,
            Path::new("session.jsonl")
        ));
    }

    #[test]
    fn session_source_matches_case_insensitive_and_prefix() {
        let source = SessionSource {
            provider: Provider::Antigravity,
            root: PathBuf::from(r"C:\Users\PC\.gemini\antigravity\brain"),
        };
        assert!(source.matches(Path::new(
            r"c:\users\pc\.gemini\antigravity\brain\123\.system_generated\logs\transcript.jsonl"
        )));
        assert!(source.matches(Path::new(
            r"\\?\C:\Users\PC\.gemini\antigravity\brain\123\.system_generated\logs\transcript.jsonl"
        )));
        assert!(!source.matches(Path::new(r"C:\Users\PC\.gemini\antigravity\brain\123\.system_generated\logs\transcript_full.jsonl")));
    }

    #[test]
    fn every_provider_has_valid_logo_url() {
        for provider in Provider::ALL {
            let logo_url = provider.logo_url();
            assert!(
                !logo_url.is_empty(),
                "Provider {provider:?} has empty logo_url"
            );
            assert!(
                logo_url.starts_with("https://"),
                "Provider {provider:?} logo_url should be https, got {logo_url}"
            );
            assert_eq!(
                provider.image_url(),
                logo_url,
                "image_url() alias should match logo_url()"
            );
            assert!(
                !provider.display_name().is_empty(),
                "Provider {provider:?} has empty display_name"
            );
        }
    }

    #[test]
    fn provider_all_contains_unique_and_exhaustive_variants() {
        use std::collections::HashSet;
        let mut set = HashSet::new();
        for p in Provider::ALL {
            assert!(set.insert(p), "Duplicate provider in ALL: {p:?}");
            // Exhaustive match ensures all enum variants are recognized
            match p {
                Provider::Codex => {}
                Provider::ClaudeCode => {}
                Provider::ChatGpt => {}
                Provider::Claude => {}
                Provider::Gemini => {}
                Provider::Cursor => {}
                Provider::Windsurf => {}
                Provider::Antigravity => {}
                Provider::Trae => {}
                Provider::Kiro => {}
                Provider::Zed => {}
                Provider::OpenCode => {}
                Provider::Aider => {}
                Provider::GitHubCopilot => {}
                Provider::Cline => {}
                Provider::RooCode => {}
                Provider::Continue => {}
                Provider::Perplexity => {}
                Provider::MicrosoftCopilot => {}
                Provider::Poe => {}
            }
        }
        assert_eq!(set.len(), 20);
    }
}
