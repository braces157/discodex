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

    pub fn default_activity(self) -> &'static str {
        match self {
            Self::Codex | Self::ClaudeCode | Self::OpenCode | Self::Aider => {
                "Working with coding agent"
            }
            Self::Cursor
            | Self::Windsurf
            | Self::Antigravity
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
            "gemini" => Some(Self::Gemini),
            "cursor" => Some(Self::Cursor),
            "windsurf" => Some(Self::Windsurf),
            "antigravity" => Some(Self::Antigravity),
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

        let title = title.to_ascii_lowercase();
        let matches = [
            ("claude code", Self::ClaudeCode),
            ("chatgpt", Self::ChatGpt),
            ("openai codex", Self::Codex),
            ("codex", Self::Codex),
            ("github copilot", Self::GitHubCopilot),
            ("copilot chat", Self::GitHubCopilot),
            ("microsoft copilot", Self::MicrosoftCopilot),
            ("perplexity", Self::Perplexity),
            ("google gemini", Self::Gemini),
            ("gemini", Self::Gemini),
            ("google ai studio", Self::Gemini),
            ("claude", Self::Claude),
            ("cursor", Self::Cursor),
            ("windsurf", Self::Windsurf),
            ("antigravity", Self::Antigravity),
            ("trae", Self::Trae),
            ("kiro", Self::Kiro),
            ("zed", Self::Zed),
            ("opencode", Self::OpenCode),
            ("aider", Self::Aider),
            ("roo code", Self::RooCode),
            ("roocode", Self::RooCode),
            ("cline", Self::Cline),
            ("continue", Self::Continue),
            ("poe", Self::Poe),
        ];

        matches
            .into_iter()
            .find_map(|(needle, provider)| title.contains(needle).then_some(provider))
    }
}

#[derive(Debug, Clone)]
pub struct SessionSource {
    pub provider: Provider,
    pub root: PathBuf,
}

impl SessionSource {
    pub fn matches(&self, path: &Path) -> bool {
        path.starts_with(&self.root) && is_session_log(self.provider, path)
    }
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
    ]
}

pub fn is_session_log(provider: Provider, path: &Path) -> bool {
    if path.extension().and_then(|ext| ext.to_str()) != Some("jsonl") {
        return false;
    }

    match provider {
        Provider::Codex => path
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.starts_with("rollout-")),
        Provider::ClaudeCode => true,
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
    }
}
