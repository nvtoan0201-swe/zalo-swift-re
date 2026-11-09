//! Design tokens: brand, accent, semantic colors per appearance, fonts.

use gpui::{App, Global, Hsla, Rgba, rgba};

pub const FONT: &str = "Inter Variable";
pub const ROUNDED: &str = "Nunito";

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Appearance {
    System,
    Light,
    Dark,
}

#[derive(Clone, Debug)]
pub struct Theme {
    pub dark: bool,
    pub accent: Hsla,
    pub text_scale: f32,
}

impl Global for Theme {}

pub fn hex(v: u32) -> Hsla {
    Hsla::from(Rgba { r: ((v >> 16) & 0xff) as f32 / 255.0, g: ((v >> 8) & 0xff) as f32 / 255.0, b: (v & 0xff) as f32 / 255.0, a: 1.0 })
}

pub fn hexa(v: u32) -> Hsla {
    Hsla::from(rgba(v))
}

pub fn with_alpha(c: Hsla, a: f32) -> Hsla {
    Hsla { a, ..c }
}

/// `color.opacity(f)` in SwiftUI terms: multiplies the existing alpha.
pub fn fade(c: Hsla, f: f32) -> Hsla {
    Hsla { a: c.a * f, ..c }
}

pub const BLUE: u32 = 0x0068FF;

/// Accent colors offered in Settings: (key, title, color).
pub const ACCENTS: [(&str, &str, u32); 7] = [
    ("blue", "Xanh dương", 0x0068FF),
    ("teal", "Xanh ngọc", 0x009EB3),
    ("indigo", "Chàm", 0x5C57D6),
    ("pink", "Hồng", 0xED407A),
    ("orange", "Cam", 0xF5801A),
    ("green", "Xanh lá", 0x29A854),
    ("graphite", "Than chì", 0x6E737D),
];

pub fn accent_color(key: &str) -> u32 {
    ACCENTS.iter().find(|a| a.0 == key).map(|a| a.2).unwrap_or(BLUE)
}

impl Theme {
    pub fn get(cx: &App) -> &Theme {
        cx.global::<Theme>()
    }

    fn pick(&self, light: u32, dark: u32) -> Hsla {
        hexa(if self.dark { dark } else { light })
    }

    pub fn label(&self) -> Hsla {
        self.pick(0x000000D9, 0xFFFFFFD9)
    }
    pub fn secondary(&self) -> Hsla {
        self.pick(0x00000080, 0xFFFFFF8C)
    }
    pub fn tertiary(&self) -> Hsla {
        self.pick(0x00000042, 0xFFFFFF40)
    }
    pub fn quaternary(&self) -> Hsla {
        self.pick(0x0000001A, 0xFFFFFF1A)
    }
    pub fn separator(&self) -> Hsla {
        self.pick(0x0000001A, 0xFFFFFF1A)
    }
    pub fn window_bg(&self) -> Hsla {
        self.pick(0xECECECFF, 0x323232FF)
    }
    pub fn content_bg(&self) -> Hsla {
        self.pick(0xFFFFFFFF, 0x1E1E1EFF)
    }
    pub fn sidebar_bg(&self) -> Hsla {
        self.pick(0xF4F4F6F0, 0x2A2A2CF0)
    }
    pub fn incoming_bubble(&self) -> Hsla {
        self.pick(0xE9E9EBFF, 0x3B3B3BFF)
    }
    pub fn red(&self) -> Hsla {
        self.pick(0xFF3B30FF, 0xFF453AFF)
    }
    pub fn orange(&self) -> Hsla {
        self.pick(0xFF9500FF, 0xFF9F0AFF)
    }
    pub fn green(&self) -> Hsla {
        self.pick(0x34C759FF, 0x30D158FF)
    }
    pub fn yellow(&self) -> Hsla {
        self.pick(0xFFCC00FF, 0xFFD60AFF)
    }
    pub fn glass(&self) -> Hsla {
        self.pick(0xFFFFFFB8, 0x3A3A3CB8)
    }
    pub fn glass_border(&self) -> Hsla {
        self.pick(0x00000014, 0xFFFFFF1F)
    }
    pub fn selection_inactive(&self) -> Hsla {
        self.pick(0xDCDCDCFF, 0x464646FF)
    }
    pub fn hover(&self) -> Hsla {
        self.pick(0x0000000D, 0xFFFFFF14)
    }
    pub fn white(&self) -> Hsla {
        hex(0xFFFFFF)
    }
    /// Accent mixed toward black/white for pressed/hover states.
    pub fn accent_pressed(&self) -> Hsla {
        let mut c = self.accent;
        c.l = (c.l * 0.88).clamp(0.0, 1.0);
        c
    }
}

/// `org.freedesktop.appearance color-scheme` through the Settings portal (1 = dark).
pub fn system_prefers_dark() -> bool {
    let out = std::process::Command::new("gdbus")
        .args([
            "call",
            "--session",
            "--dest",
            "org.freedesktop.portal.Desktop",
            "--object-path",
            "/org/freedesktop/portal/desktop",
            "--method",
            "org.freedesktop.portal.Settings.ReadOne",
            "org.freedesktop.appearance",
            "color-scheme",
        ])
        .output();
    match out {
        Ok(o) => String::from_utf8_lossy(&o.stdout).contains("uint32 1"),
        Err(_) => false,
    }
}
