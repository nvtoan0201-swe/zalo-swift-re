//! Embedded UI assets (`shared/ui-assets`): icons, images, fonts.

use std::borrow::Cow;

use gpui::{App, AssetSource, SharedString};

#[derive(rust_embed::RustEmbed)]
#[folder = "../../../shared/ui-assets"]
#[exclude = "*.icns"]
struct Embedded;

/// Olaz-for-Linux own assets (spinner…), next to the verbatim shared ones.
#[derive(rust_embed::RustEmbed)]
#[folder = "assets"]
struct Own;

pub struct OlazAssets;

impl AssetSource for OlazAssets {
    fn load(&self, path: &str) -> anyhow::Result<Option<Cow<'static, [u8]>>> {
        if let Some(f) = Embedded::get(path).or_else(|| Own::get(path)) {
            return Ok(Some(f.data));
        }
        gpui_component_assets(path)
    }
    fn list(&self, path: &str) -> anyhow::Result<Vec<SharedString>> {
        Ok(Embedded::iter().filter(|p| p.starts_with(path)).map(|p| SharedString::from(p.to_string())).collect())
    }
}

/// gpui-component looks up its own icons (`icons/check.svg`, submenu chevrons…): serve them from its asset crate.
fn gpui_component_assets(path: &str) -> anyhow::Result<Option<Cow<'static, [u8]>>> {
    use gpui::AssetSource as _;
    Ok(gpui_kit_assets::Assets.load(path).ok().flatten())
}

pub fn bytes(path: &str) -> Option<Cow<'static, [u8]>> {
    Embedded::get(path).or_else(|| Own::get(path)).map(|f| f.data)
}

/// Inter (UI), Nunito (rounded), Noto Color Emoji — nothing from fontconfig for UI text.
pub fn register_fonts(cx: &mut App) {
    let fonts: Vec<Cow<'static, [u8]>> = ["fonts/InterVariable.ttf", "fonts/InterVariable-Italic.ttf", "fonts/Nunito[wght].ttf", "fonts/NotoColorEmoji-Regular.ttf"]
        .iter()
        .filter_map(|p| bytes(p))
        .collect();
    if let Err(e) = cx.text_system().add_fonts(fonts) {
        olaz_core::log::write(format!("ui: fonts not registered: {e}"));
    }
}

#[cfg(test)]
mod tests {
    use gpui::AssetSource as _;

    #[test]
    fn serves_gpui_component_icons() {
        for p in ["icons/check.svg", "icons/chevron-right.svg", "icons/close.svg"] {
            assert!(super::OlazAssets.load(p).unwrap().is_some(), "{p}");
        }
        assert!(super::OlazAssets.load("icons/phosphor/regular/tag.svg").unwrap().is_some());
    }
}
