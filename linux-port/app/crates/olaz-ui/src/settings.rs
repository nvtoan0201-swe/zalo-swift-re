//! App preferences (`olaz.*` keys, the UserDefaults equivalent) in `$XDG_CONFIG_HOME/olaz/settings.json`.

use gpui::Global;
use olaz_core::paths::XdgPaths;
use olaz_main::Settings as Store;
use serde_json::Value;

#[derive(Clone, Debug, PartialEq)]
pub struct Prefs {
    pub accent: String,
    /// "system" | "light" | "dark"
    pub appearance: String,
    /// "small" | "standard" | "large" | "extraLarge"
    pub text_size: String,
    /// "none" | "mist" | "silk" | "lotus" | "night"
    pub wallpaper: String,
    pub did_finish_setup: bool,
    pub setup_pending: bool,
    pub system_window_buttons: bool,
}

impl Default for Prefs {
    fn default() -> Self {
        Self {
            accent: "blue".into(),
            appearance: "system".into(),
            text_size: "standard".into(),
            wallpaper: "none".into(),
            did_finish_setup: false,
            setup_pending: false,
            system_window_buttons: false,
        }
    }
}

pub struct AppPrefs {
    pub prefs: Prefs,
    paths: XdgPaths,
}

impl Global for AppPrefs {}

impl AppPrefs {
    pub fn load(paths: &XdgPaths) -> Self {
        let store = Store::load(paths);
        let d = Prefs::default();
        let s = |k: &str, def: &str| store.get_str(k).unwrap_or_else(|| def.to_string());
        let b = |k: &str, def: bool| store.get_bool(k).unwrap_or(def);
        Self {
            prefs: Prefs {
                accent: s("olaz.accent", &d.accent),
                appearance: s("olaz.appearance", &d.appearance),
                text_size: s("olaz.textSize", &d.text_size),
                wallpaper: s("olaz.wallpaper", &d.wallpaper),
                did_finish_setup: b("olaz.didFinishSetup", false),
                setup_pending: b("olaz.setupPending", false),
                system_window_buttons: b("olaz.systemWindowButtons", false),
            },
            paths: paths.clone(),
        }
    }

    pub fn save(&self) {
        let mut store = Store::load(&self.paths);
        let p = &self.prefs;
        store.set("olaz.accent", Value::String(p.accent.clone()));
        store.set("olaz.appearance", Value::String(p.appearance.clone()));
        store.set("olaz.textSize", Value::String(p.text_size.clone()));
        store.set("olaz.wallpaper", Value::String(p.wallpaper.clone()));
        store.set("olaz.didFinishSetup", Value::Bool(p.did_finish_setup));
        store.set("olaz.setupPending", Value::Bool(p.setup_pending));
        store.set("olaz.systemWindowButtons", Value::Bool(p.system_window_buttons));
        store.save();
    }

    pub fn text_scale(&self) -> f32 {
        match self.prefs.text_size.as_str() {
            "small" => 0.92,
            "large" => 1.12,
            "extraLarge" => 1.26,
            _ => 1.0,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_keeps_other_keys() {
        let dir = tempfile::tempdir().unwrap();
        let paths = XdgPaths::rooted(dir.path());
        let mut s = Store::load(&paths);
        s.set("olaz.imei", Value::String("keep-me".into()));
        s.save();
        let mut p = AppPrefs::load(&paths);
        assert_eq!(p.prefs, Prefs::default());
        p.prefs.accent = "teal".into();
        p.prefs.setup_pending = true;
        p.save();
        let again = AppPrefs::load(&paths);
        assert_eq!(again.prefs.accent, "teal");
        assert!(again.prefs.setup_pending);
        assert_eq!(Store::load(&paths).get_str("olaz.imei").as_deref(), Some("keep-me"));
    }
}
