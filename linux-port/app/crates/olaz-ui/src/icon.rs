//! SF Symbol names (as used by the macOS app) → bundled Phosphor SVGs, mapped in `assets/icon-map.json`.

use std::collections::HashMap;
use std::sync::OnceLock;

use gpui::{Hsla, IntoElement, ParentElement, Styled, Svg, div, px, svg};

const MAP_JSON: &str = include_str!("../assets/icon-map.json");

struct Entry {
    icon: String,
    weight: String,
    mirror: bool,
}

fn map() -> &'static HashMap<String, Entry> {
    static MAP: OnceLock<HashMap<String, Entry>> = OnceLock::new();
    MAP.get_or_init(|| {
        let v: serde_json::Value = serde_json::from_str(MAP_JSON).unwrap_or_default();
        v.get("symbols")
            .and_then(|s| s.as_object())
            .map(|o| {
                o.iter()
                    .filter_map(|(k, e)| {
                        Some((
                            k.clone(),
                            Entry {
                                icon: e.get("icon")?.as_str()?.to_string(),
                                weight: e.get("weight").and_then(|w| w.as_str()).unwrap_or("regular").to_string(),
                                mirror: e.get("mirror").and_then(|m| m.as_bool()).unwrap_or(false),
                            },
                        ))
                    })
                    .collect()
            })
            .unwrap_or_default()
    })
}

/// Asset path of a symbol; `bold` = SF weight semibold or heavier (fill symbols stay fill).
pub fn path(symbol: &str, bold: bool) -> String {
    // Olaz's own icons are addressed by asset path.
    if symbol.starts_with("icons/") {
        return symbol.to_string();
    }
    match map().get(symbol) {
        Some(e) => {
            let weight = if e.weight == "fill" { "fill" } else if bold { "bold" } else { e.weight.as_str() };
            let p = file(weight, &e.icon);
            if crate::assets::bytes(&p).is_some() { p } else { file(&e.weight, &e.icon) }
        }
        None => "icons/phosphor/regular/question.svg".into(),
    }
}

fn file(weight: &str, name: &str) -> String {
    match weight {
        "regular" => format!("icons/phosphor/regular/{name}.svg"),
        w => format!("icons/phosphor/{w}/{name}-{w}.svg"),
    }
}

pub fn is_mirrored(symbol: &str) -> bool {
    map().get(symbol).is_some_and(|e| e.mirror)
}

/// The glyph for an SF Symbol at font size `size` (rendered at 1.15 × size, matching SF Symbols' optical size).
pub fn icon(symbol: &str, size: f32, color: Hsla) -> Svg {
    icon_weighted(symbol, size, color, false)
}

pub fn icon_weighted(symbol: &str, size: f32, color: Hsla, bold: bool) -> Svg {
    svg().path(path(symbol, bold)).size(px(size * 1.15)).flex_none().text_color(color)
}

/// An icon centered in a fixed square box.
pub fn icon_box(symbol: &str, size: f32, color: Hsla, bold: bool, frame: f32) -> impl IntoElement {
    div().size(px(frame)).flex().items_center().justify_center().flex_none().child(icon_weighted(symbol, size, color, bold))
}

#[cfg(test)]
mod tests {
    #[test]
    fn every_mapped_icon_exists() {
        for (k, e) in super::map().iter() {
            let p = super::file(&e.weight, &e.icon);
            assert!(crate::assets::bytes(&p).is_some(), "{k} → {p}");
        }
        assert!(super::path("tray", false).ends_with(".svg"));
    }
}
