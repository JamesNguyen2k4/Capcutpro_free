use gpui::{Rgba, rgb};

pub struct Theme;

impl Theme {
    pub const BG_APP: u32 = 0x0d0d0f;
    pub const BG_PANEL: u32 = 0x151518;
    pub const BG_SURFACE: u32 = 0x1e1e24;
    pub const BG_HOVER: u32 = 0x282830;
    pub const BG_PREVIEW: u32 = 0x050507;

    pub const BORDER: u32 = 0x2a2a32;

    pub const TEXT_PRIMARY: u32 = 0xf4f4f5;
    pub const TEXT_SECONDARY: u32 = 0xa1a1aa;
    pub const TEXT_MUTED: u32 = 0x71717a;

    pub const ACCENT: u32 = 0x3b82f6;
    pub const ACCENT_HOVER: u32 = 0x2563eb;
    pub const PLAYHEAD: u32 = 0xef4444;
    pub const SUCCESS: u32 = 0x22c55e;

    pub fn bg_app() -> Rgba {
        rgb(Self::BG_APP)
    }

    pub fn bg_panel() -> Rgba {
        rgb(Self::BG_PANEL)
    }

    pub fn bg_surface() -> Rgba {
        rgb(Self::BG_SURFACE)
    }

    pub fn bg_hover() -> Rgba {
        rgb(Self::BG_HOVER)
    }

    pub fn bg_preview() -> Rgba {
        rgb(Self::BG_PREVIEW)
    }

    pub fn border() -> Rgba {
        rgb(Self::BORDER)
    }

    pub fn text_primary() -> Rgba {
        rgb(Self::TEXT_PRIMARY)
    }

    pub fn text_secondary() -> Rgba {
        rgb(Self::TEXT_SECONDARY)
    }

    pub fn text_muted() -> Rgba {
        rgb(Self::TEXT_MUTED)
    }

    pub fn accent() -> Rgba {
        rgb(Self::ACCENT)
    }
}
