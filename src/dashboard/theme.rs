use std::{collections::BTreeMap, sync::Arc};

use gpui::{Image, ImageFormat};

pub const BG_CANVAS: u32 = 0x0D0F12;
pub const BG_SURFACE: u32 = 0x14171C;
pub const BG_RECESSED: u32 = 0x0A0C0F;
pub const BG_ELEVATED: u32 = 0x1E232B;
pub const BG_HOVER: u32 = 0xFFFFFF0D;
pub const BG_ROW_HOVER: u32 = 0xFFFFFF08;

pub const BORDER_OUTER: u32 = 0xFFFFFF1F;
pub const BORDER_SUBTLE: u32 = 0xFFFFFF12;
pub const BORDER_STRONG: u32 = 0xFFFFFF24;
pub const GRID_LINE: u32 = 0xFFFFFF0A;
pub const TRACK_BG: u32 = 0xFFFFFF0C;

pub const TEXT_PRIMARY: u32 = 0xF3F4F6;
pub const TEXT_SECONDARY: u32 = 0x9CA3AF;
pub const TEXT_MUTED: u32 = 0x6B7280;

pub const ACCENT_LIME: u32 = 0xC5F277;
pub const ACCENT_LIME_SOFT: u32 = 0xC5F2771F;
pub const ACCENT_LIME_BORDER: u32 = 0xC5F27745;
pub const ACCENT_LIME_DIM: u32 = 0xC5F2774D;

pub const ACCENT_COST: u32 = 0x6EE7B7;
pub const ACCENT_COST_DIM: u32 = 0x6EE7B74D;

pub const ACCENT_CALLS: u32 = 0x7DD3FC;
pub const ACCENT_CALLS_DIM: u32 = 0x7DD3FC4D;

pub const ACCENT_CLAUDE: u32 = 0xE8A87C;
pub const ACCENT_REASONING: u32 = 0xA78BFA;
pub const ACCENT_AMBER: u32 = 0xF5B971;
pub const ACCENT_AMBER_SOFT: u32 = 0xF5B97116;
pub const ACCENT_AMBER_BORDER: u32 = 0xF5B97138;
pub const ACCENT_ERR: u32 = 0xF87171;
pub const ACCENT_ERR_SOFT: u32 = 0xF871711A;
pub const ACCENT_ERR_BORDER: u32 = 0xF8717140;

pub const FONT_UI: &str = "Segoe UI Variable";
pub const FONT_MONO: &str = "Cascadia Mono";
pub const FONT_ICON: &str = "Segoe Fluent Icons";

pub const COMPACT_WIDTH: f32 = 520.0;
pub const COMPACT_HEIGHT: f32 = 720.0;
pub const WIDE_WIDTH: f32 = 880.0;
pub const WIDE_HEIGHT: f32 = 836.0;

pub struct Assets {
    pub brand: Arc<Image>,
    pub providers: BTreeMap<&'static str, Arc<Image>>,
}

impl Assets {
    /// Loads embedded brand and provider PNG marks for the dashboard.
    pub fn load() -> Self {
        let icons: [(&'static str, &[u8]); 16] = [
            ("codex", include_bytes!("../../assets/logos/codex.png")),
            (
                "claude-code",
                include_bytes!("../../assets/logos/claude-code.png"),
            ),
            (
                "antigravity",
                include_bytes!("../../assets/logos/antigravity.png"),
            ),
            (
                "deepseek-harness",
                include_bytes!("../../assets/logos/deepseek-harness.png"),
            ),
            (
                "gemini-cli",
                include_bytes!("../../assets/logos/gemini-cli.png"),
            ),
            (
                "qwen-code",
                include_bytes!("../../assets/logos/qwen-code.png"),
            ),
            (
                "opencode",
                include_bytes!("../../assets/logos/opencode.png"),
            ),
            (
                "kilo-code",
                include_bytes!("../../assets/logos/kilo-code.png"),
            ),
            (
                "factory-droid",
                include_bytes!("../../assets/logos/factory-droid.png"),
            ),
            (
                "copilot-cli",
                include_bytes!("../../assets/logos/copilot-cli.png"),
            ),
            ("junie", include_bytes!("../../assets/logos/junie.png")),
            ("cline", include_bytes!("../../assets/logos/cline.png")),
            (
                "roo-code",
                include_bytes!("../../assets/logos/roo-code.png"),
            ),
            ("zed", include_bytes!("../../assets/logos/zed.png")),
            ("crush", include_bytes!("../../assets/logos/crush.png")),
            ("aider", include_bytes!("../../assets/logos/aider.png")),
        ];
        let mut providers = BTreeMap::new();
        for (slug, bytes) in icons {
            providers.insert(
                slug,
                Arc::new(Image::from_bytes(ImageFormat::Png, bytes.to_vec())),
            );
        }
        Self {
            brand: Arc::new(Image::from_bytes(
                ImageFormat::Png,
                include_bytes!("../../assets/brand/discodex-64.png").to_vec(),
            )),
            providers,
        }
    }

    pub fn provider_icon(&self, provider: &str) -> Option<Arc<Image>> {
        self.providers
            .get(provider.trim().to_ascii_lowercase().as_str())
            .cloned()
    }
}

pub fn with_alpha(rgb_hex: u32, alpha: u8) -> u32 {
    ((rgb_hex & 0x00FF_FFFF) << 8) | u32::from(alpha)
}

pub fn provider_display(provider: &str) -> String {
    match provider.trim().to_ascii_lowercase().as_str() {
        "codex" => "Codex".into(),
        "claude-code" => "Claude Code".into(),
        "antigravity" => "Antigravity".into(),
        "deepseek-harness" => "DeepSeek".into(),
        "gemini-cli" => "Gemini CLI".into(),
        "qwen-code" => "Qwen Code".into(),
        "opencode" => "OpenCode".into(),
        "kilo-code" => "Kilo Code".into(),
        "factory-droid" => "Factory Droid".into(),
        "copilot-cli" => "Copilot CLI".into(),
        "junie" => "Junie".into(),
        "cline" => "Cline".into(),
        "roo-code" => "Roo Code".into(),
        "zed" => "Zed".into(),
        "crush" => "Crush".into(),
        "aider" => "Aider".into(),
        "" => "Unknown".into(),
        _ => provider.into(),
    }
}

pub fn provider_color(provider: &str) -> u32 {
    match provider.trim().to_ascii_lowercase().as_str() {
        "codex" => ACCENT_LIME,
        "claude-code" => ACCENT_CLAUDE,
        "antigravity" => 0x7DD3FC,
        "deepseek-harness" => 0x818CF8,
        "gemini-cli" => 0x60A5FA,
        "qwen-code" => 0xA78BFA,
        "opencode" => 0x6EE7B7,
        "kilo-code" => 0xF472B6,
        "factory-droid" => 0xFBBF24,
        "copilot-cli" => 0x38BDF8,
        "junie" => 0x34D399,
        "cline" => 0xFB923C,
        "roo-code" => 0xC084FC,
        "zed" => 0x93C5FD,
        "crush" => 0xF87171,
        "aider" => 0xA3E635,
        _ => ACCENT_CALLS,
    }
}

pub fn provider_short(provider: &str) -> String {
    match provider.trim().to_ascii_lowercase().as_str() {
        "codex" => "CX".into(),
        "claude-code" => "CC".into(),
        "antigravity" => "AG".into(),
        "deepseek-harness" => "DS".into(),
        "gemini-cli" => "GM".into(),
        "qwen-code" => "QW".into(),
        "opencode" => "OC".into(),
        "kilo-code" => "KC".into(),
        "factory-droid" => "FD".into(),
        "copilot-cli" => "CP".into(),
        "junie" => "JN".into(),
        "cline" => "CL".into(),
        "roo-code" => "RC".into(),
        "zed" => "ZD".into(),
        "crush" => "CR".into(),
        "aider" => "AD".into(),
        other => other
            .chars()
            .filter(|c| c.is_ascii_alphanumeric())
            .take(2)
            .collect::<String>()
            .to_uppercase(),
    }
}
