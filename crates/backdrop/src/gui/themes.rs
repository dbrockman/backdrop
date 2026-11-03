//! Color themes by name: JSON theme sets in the user's `themes` folder, then
//! the sets that ship with GPUI Kit (copied into `crates/backdrop/themes`), then
//! GPUI Kit's "Default Light" and "Default Dark".
//!
//! A theme file is a GPUI Kit `ThemeSet`: one file can hold several themes,
//! e.g. "Ayu Light" and "Ayu Dark", so themes are found by the name inside
//! the file rather than by file name. Each theme is light or dark.

use std::cell::OnceCell;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use gpui_kit::SharedString;
use gpui_kit::component::{ThemeConfig, ThemeMode, ThemeSet};

macro_rules! builtin {
    ($file:literal) => {
        ($file, include_str!(concat!("../../themes/", $file)))
    };
}

/// GPUI Kit's theme sets, from the `themes` folder of its repository.
const BUILTIN: &[(&str, &str)] = &[
    builtin!("adventure.json"),
    builtin!("alduin.json"),
    builtin!("asciinema.json"),
    builtin!("aurora.json"),
    builtin!("ayu.json"),
    builtin!("catppuccin.json"),
    builtin!("everforest.json"),
    builtin!("fahrenheit.json"),
    builtin!("flexoki.json"),
    builtin!("gruvbox.json"),
    builtin!("harper.json"),
    builtin!("hybrid.json"),
    builtin!("jellybeans.json"),
    builtin!("kibble.json"),
    builtin!("macos-classic.json"),
    builtin!("mellifluous.json"),
    builtin!("molokai.json"),
    builtin!("solarized.json"),
    builtin!("spaceduck.json"),
    builtin!("tokyonight.json"),
    builtin!("twilight.json"),
];

/// The themes available by name.
pub struct Themes {
    dir: PathBuf,
    /// Themes from the user's folder, which win over built-in ones.
    user: BTreeMap<SharedString, Rc<ThemeConfig>>,
    /// Built-in themes, parsed only when a name is not found in `user` or
    /// all names are listed, so the default themes cost nothing at startup.
    builtin: OnceCell<BTreeMap<SharedString, Rc<ThemeConfig>>>,
    /// GPUI Kit's "Default Light" and "Default Dark".
    defaults: [Rc<ThemeConfig>; 2],
    /// Why files in the folder were skipped or partly ignored.
    file_errors: Vec<String>,
}

impl Themes {
    /// Reads the theme files in `dir`. `defaults` are the themes used when
    /// none is configured, light first.
    pub fn load(dir: &Path, defaults: [Rc<ThemeConfig>; 2]) -> Self {
        let mut user = BTreeMap::new();
        let mut file_errors = Vec::new();
        let mut files: Vec<PathBuf> = std::fs::read_dir(dir)
            .into_iter()
            .flatten()
            .filter_map(|entry| Some(entry.ok()?.path()))
            .filter(|path| path.extension().is_some_and(|ext| ext == "json") && path.is_file())
            .collect();
        // Sorted, so which file wins a duplicate name does not depend on
        // the order the file system lists them in.
        files.sort();
        let mut sources: BTreeMap<SharedString, String> = BTreeMap::new();
        for path in files {
            let file_name = path.file_name().unwrap_or_default().to_string_lossy();
            let set = std::fs::read_to_string(&path)
                .map_err(|e| e.to_string())
                .and_then(|json| {
                    serde_json::from_str::<ThemeSet>(&json).map_err(|e| e.to_string())
                });
            match set {
                Ok(set) => {
                    for theme in set.themes {
                        if let Some(first) = sources.get(&theme.name) {
                            file_errors.push(format!(
                                "“{}” is in both {first} and {file_name}; using {first}.",
                                theme.name
                            ));
                            continue;
                        }
                        sources.insert(theme.name.clone(), file_name.to_string());
                        user.insert(theme.name.clone(), Rc::new(theme));
                    }
                }
                Err(e) => file_errors.push(format!("Couldn’t read {file_name}: {e}")),
            }
        }
        Self {
            dir: dir.to_owned(),
            user,
            builtin: OnceCell::new(),
            defaults,
            file_errors,
        }
    }

    /// The folder the user's themes are read from.
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    pub fn file_errors(&self) -> &[String] {
        &self.file_errors
    }

    /// The theme used for `mode` when none is configured.
    pub fn default_for(&self, mode: ThemeMode) -> &Rc<ThemeConfig> {
        &self.defaults[usize::from(mode.is_dark())]
    }

    /// Looks up `name` (or the default when `None`) for use as the `mode`
    /// theme. On failure the error says why, and the default should be used.
    pub fn resolve(&self, name: Option<&str>, mode: ThemeMode) -> Result<Rc<ThemeConfig>, String> {
        let Some(name) = name else {
            return Ok(self.default_for(mode).clone());
        };
        let theme = self
            .get(name)
            .ok_or_else(|| format!("There is no theme named “{name}”."))?;
        if theme.mode != mode {
            return Err(format!("“{name}” is a {} theme.", theme.mode.name()));
        }
        Ok(theme)
    }

    fn get(&self, name: &str) -> Option<Rc<ThemeConfig>> {
        self.user
            .get(name)
            .or_else(|| self.builtin().get(name))
            .or_else(|| self.defaults.iter().find(|theme| theme.name == name))
            .cloned()
    }

    /// Names of the `mode` themes: the default first, then the rest by name.
    pub fn names(&self, mode: ThemeMode) -> Vec<SharedString> {
        let default = &self.default_for(mode).name;
        let mut names: Vec<SharedString> = self
            .user
            .values()
            .chain(self.builtin().values())
            .filter(|theme| theme.mode == mode && &theme.name != default)
            .map(|theme| theme.name.clone())
            .collect();
        names.sort_by_key(|name| name.to_lowercase());
        names.dedup();
        names.insert(0, default.clone());
        names
    }

    fn builtin(&self) -> &BTreeMap<SharedString, Rc<ThemeConfig>> {
        self.builtin.get_or_init(|| {
            BUILTIN
                .iter()
                .flat_map(|(file, json)| {
                    serde_json::from_str::<ThemeSet>(json)
                        .unwrap_or_else(|e| panic!("built-in theme {file} is invalid: {e}"))
                        .themes
                })
                .map(|theme| (theme.name.clone(), Rc::new(theme)))
                .collect()
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn theme(name: &str, mode: ThemeMode) -> Rc<ThemeConfig> {
        let mut theme = ThemeConfig::default();
        theme.name = name.to_owned().into();
        theme.mode = mode;
        Rc::new(theme)
    }

    fn defaults() -> [Rc<ThemeConfig>; 2] {
        [
            theme("Default Light", ThemeMode::Light),
            theme("Default Dark", ThemeMode::Dark),
        ]
    }

    fn set(themes: &[(&str, &str)]) -> String {
        let themes: Vec<String> = themes
            .iter()
            .map(|(name, mode)| {
                format!(r#"{{"name": "{name}", "mode": "{mode}", "colors": {{}}}}"#)
            })
            .collect();
        format!(r#"{{"name": "Set", "themes": [{}]}}"#, themes.join(","))
    }

    #[test]
    fn builtin_themes_parse() {
        let themes = Themes::load(Path::new("/nonexistent"), defaults());
        assert!(themes.file_errors().is_empty());
        let light = themes.names(ThemeMode::Light);
        let dark = themes.names(ThemeMode::Dark);
        assert_eq!(light[0], "Default Light");
        assert_eq!(dark[0], "Default Dark");
        assert!(light.iter().any(|name| name == "Ayu Light"));
        assert!(dark.iter().any(|name| name == "Catppuccin Mocha"));
        assert_eq!(light.len() + dark.len(), 36 + 2);
    }

    #[test]
    fn resolves_user_themes_before_builtin_ones() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(
            tmp.path().join("mine.json"),
            set(&[("Ayu Light", "light"), ("Mine", "dark")]),
        )
        .unwrap();
        let themes = Themes::load(tmp.path(), defaults());
        assert!(themes.file_errors().is_empty());

        // The user's "Ayu Light" has no colors; the built-in one does.
        let ayu = themes.resolve(Some("Ayu Light"), ThemeMode::Light).unwrap();
        assert!(ayu.colors.background.is_none());
        assert_eq!(
            themes.resolve(Some("Mine"), ThemeMode::Dark).unwrap().name,
            "Mine"
        );
        assert_eq!(
            themes
                .resolve(Some("Ayu Dark"), ThemeMode::Dark)
                .unwrap()
                .name,
            "Ayu Dark"
        );
        assert_eq!(
            themes.resolve(None, ThemeMode::Dark).unwrap().name,
            "Default Dark"
        );
        assert!(themes.names(ThemeMode::Dark).contains(&"Mine".into()));
    }

    #[test]
    fn explains_unknown_names_and_wrong_modes() {
        let themes = Themes::load(Path::new("/nonexistent"), defaults());
        assert_eq!(
            themes.resolve(Some("Nope"), ThemeMode::Light).unwrap_err(),
            "There is no theme named “Nope”."
        );
        assert_eq!(
            themes
                .resolve(Some("Ayu Dark"), ThemeMode::Light)
                .unwrap_err(),
            "“Ayu Dark” is a dark theme."
        );
    }

    #[test]
    fn reports_bad_files_and_duplicate_names() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("a.json"), set(&[("Same", "light")])).unwrap();
        std::fs::write(tmp.path().join("b.json"), set(&[("Same", "light")])).unwrap();
        std::fs::write(tmp.path().join("broken.json"), "{ not json").unwrap();
        std::fs::write(tmp.path().join("notes.txt"), "ignored").unwrap();
        let themes = Themes::load(tmp.path(), defaults());
        let errors = themes.file_errors();
        assert_eq!(errors.len(), 2, "{errors:?}");
        assert_eq!(
            errors[0],
            "“Same” is in both a.json and b.json; using a.json."
        );
        assert!(errors[1].starts_with("Couldn’t read broken.json: "));
        assert!(themes.resolve(Some("Same"), ThemeMode::Light).is_ok());
    }
}
