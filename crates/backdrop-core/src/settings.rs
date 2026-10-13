//! User settings, persisted as TOML in the config directory.
//!
//! The GUI and CLI may run at the same time, so writers go through
//! [`Settings::update`], which re-reads the file before changing it, and every
//! save replaces the file atomically so a reader never sees a partial write.
//! The file can hold API keys, so only the user can read it.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

#[derive(Debug, thiserror::Error)]
pub enum SettingsError {
    #[error("could not read {}: {cause}", path.display())]
    Read { path: PathBuf, cause: io::Error },
    #[error("could not parse {}: {cause}", path.display())]
    Parse {
        path: PathBuf,
        cause: toml::de::Error,
    },
    #[error("could not write {}: {cause}", path.display())]
    Write { path: PathBuf, cause: io::Error },
    #[error("could not serialize settings: {0}")]
    Serialize(toml::ser::Error),
    #[error("{} is not a folder", path.display())]
    NotADirectory { path: PathBuf },
    #[error("could not open {}: {cause}", path.display())]
    Inaccessible { path: PathBuf, cause: io::Error },
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    library: LibrarySettings,
    providers: ProviderSettings,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
struct LibrarySettings {
    directory: Option<PathBuf>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
struct ProviderSettings {
    wallhaven: WallhavenSettings,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
struct WallhavenSettings {
    #[serde(skip_serializing_if = "Option::is_none")]
    api_key: Option<Secret>,
    /// Leave wallpapers the library already has out of the results.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    hide_in_library: bool,
}

/// A credential, left out of `Debug` output so it doesn't end up in logs.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
struct Secret(String);

impl std::fmt::Debug for Secret {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("<secret>")
    }
}

/// Whether the configured library folder can be used.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LibraryStatus {
    NotChosen,
    /// Configured, but missing or no longer a folder.
    Missing(PathBuf),
    Available(PathBuf),
}

impl Settings {
    /// Reads settings from `path`. A missing file yields the defaults.
    pub fn load(path: &Path) -> Result<Self, SettingsError> {
        let text = match fs::read_to_string(path) {
            Ok(text) => text,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Self::default()),
            Err(cause) => {
                return Err(SettingsError::Read {
                    path: path.to_path_buf(),
                    cause,
                });
            }
        };
        toml::from_str(&text).map_err(|cause| SettingsError::Parse {
            path: path.to_path_buf(),
            cause,
        })
    }

    /// Writes settings to `path`, creating its directory if needed.
    pub fn save(&self, path: &Path) -> Result<(), SettingsError> {
        let text = toml::to_string_pretty(self).map_err(SettingsError::Serialize)?;
        crate::fs_util::write_atomically(path, text.as_bytes()).map_err(|cause| {
            SettingsError::Write {
                path: path.to_path_buf(),
                cause,
            }
        })
    }

    /// Loads the current settings from `path`, applies `change`, and saves
    /// them. Nothing is written if `change` fails.
    pub fn update<T>(
        path: &Path,
        change: impl FnOnce(&mut Self) -> Result<T, SettingsError>,
    ) -> Result<(Self, T), SettingsError> {
        let mut settings = Self::load(path)?;
        let value = change(&mut settings)?;
        settings.save(path)?;
        Ok((settings, value))
    }

    pub fn library_dir(&self) -> Option<&Path> {
        self.library.directory.as_deref()
    }

    /// Sets the library folder, stored as an absolute path with symlinks
    /// resolved. Returns the stored path.
    pub fn set_library_dir(&mut self, dir: &Path) -> Result<PathBuf, SettingsError> {
        let dir = fs::canonicalize(dir).map_err(|cause| SettingsError::Inaccessible {
            path: dir.to_path_buf(),
            cause,
        })?;
        if !dir.is_dir() {
            return Err(SettingsError::NotADirectory { path: dir });
        }
        self.library.directory = Some(dir.clone());
        Ok(dir)
    }

    /// The Wallhaven API key, needed for NSFW wallpapers.
    pub fn wallhaven_api_key(&self) -> Option<&str> {
        self.providers
            .wallhaven
            .api_key
            .as_ref()
            .map(|key| key.0.as_str())
    }

    /// Sets or, with `None` or an empty key, removes the Wallhaven API key.
    pub fn set_wallhaven_api_key(&mut self, key: Option<&str>) {
        self.providers.wallhaven.api_key = key
            .map(str::trim)
            .filter(|key| !key.is_empty())
            .map(|key| Secret(key.to_owned()));
    }

    /// Whether Wallhaven results leave out wallpapers the library has.
    pub fn wallhaven_hide_in_library(&self) -> bool {
        self.providers.wallhaven.hide_in_library
    }

    pub fn set_wallhaven_hide_in_library(&mut self, hide: bool) {
        self.providers.wallhaven.hide_in_library = hide;
    }

    /// Checks the configured library folder against the filesystem.
    pub fn library_status(&self) -> LibraryStatus {
        match self.library_dir() {
            None => LibraryStatus::NotChosen,
            Some(dir) if dir.is_dir() => LibraryStatus::Available(dir.to_path_buf()),
            Some(dir) => LibraryStatus::Missing(dir.to_path_buf()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_file_loads_defaults() {
        let temp = tempfile::tempdir().unwrap();
        let settings = Settings::load(&temp.path().join("settings.toml")).unwrap();

        assert_eq!(settings, Settings::default());
        assert_eq!(settings.library_status(), LibraryStatus::NotChosen);
    }

    #[test]
    fn library_dir_round_trips_through_the_file() {
        let temp = tempfile::tempdir().unwrap();
        let file = temp.path().join("config/settings.toml");
        let library = temp.path().join("Wallpapers");
        fs::create_dir(&library).unwrap();

        let (_, stored) = Settings::update(&file, |s| s.set_library_dir(&library)).unwrap();
        let loaded = Settings::load(&file).unwrap();

        assert_eq!(stored, fs::canonicalize(&library).unwrap());
        assert_eq!(loaded.library_dir(), Some(stored.as_path()));
        assert_eq!(loaded.library_status(), LibraryStatus::Available(stored));
    }

    #[test]
    fn library_dir_is_stored_resolved() {
        let temp = tempfile::tempdir().unwrap();
        fs::create_dir(temp.path().join("Wallpapers")).unwrap();
        let mut settings = Settings::default();

        let stored = settings
            .set_library_dir(&temp.path().join("Wallpapers/../Wallpapers"))
            .unwrap();

        assert_eq!(
            stored,
            fs::canonicalize(temp.path().join("Wallpapers")).unwrap()
        );
    }

    #[test]
    fn library_dir_must_be_an_existing_folder() {
        let temp = tempfile::tempdir().unwrap();
        let file = temp.path().join("image.jpg");
        fs::write(&file, b"").unwrap();
        let mut settings = Settings::default();

        assert!(matches!(
            settings.set_library_dir(&temp.path().join("missing")),
            Err(SettingsError::Inaccessible { .. })
        ));
        assert!(matches!(
            settings.set_library_dir(&file),
            Err(SettingsError::NotADirectory { .. })
        ));
        assert_eq!(settings.library_dir(), None);
    }

    #[test]
    fn removed_library_dir_is_reported_missing() {
        let temp = tempfile::tempdir().unwrap();
        let library = temp.path().join("Wallpapers");
        fs::create_dir(&library).unwrap();
        let mut settings = Settings::default();
        let stored = settings.set_library_dir(&library).unwrap();

        fs::remove_dir(&library).unwrap();

        assert_eq!(settings.library_status(), LibraryStatus::Missing(stored));
    }

    #[test]
    fn failed_change_leaves_the_file_alone() {
        let temp = tempfile::tempdir().unwrap();
        let file = temp.path().join("settings.toml");

        let result = Settings::update(&file, |s| s.set_library_dir(&temp.path().join("missing")));

        assert!(result.is_err());
        assert!(!file.exists());
    }

    #[test]
    fn malformed_file_is_an_error() {
        let temp = tempfile::tempdir().unwrap();
        let file = temp.path().join("settings.toml");
        fs::write(&file, "[library\n").unwrap();

        assert!(matches!(
            Settings::load(&file),
            Err(SettingsError::Parse { .. })
        ));
    }

    #[test]
    fn api_key_is_stored_privately_and_never_debug_printed() {
        use std::os::unix::fs::PermissionsExt as _;

        let temp = tempfile::tempdir().unwrap();
        let file = temp.path().join("settings.toml");

        Settings::update(&file, |s| {
            s.set_wallhaven_api_key(Some(" abc123 "));
            Ok(())
        })
        .unwrap();
        let settings = Settings::load(&file).unwrap();

        assert_eq!(settings.wallhaven_api_key(), Some("abc123"));
        assert!(!format!("{settings:?}").contains("abc123"));
        // Only the user can read the file holding the key.
        let mode = fs::metadata(&file).unwrap().permissions().mode();
        assert_eq!(mode & 0o077, 0);
    }

    #[test]
    fn empty_api_key_removes_it() {
        let mut settings = Settings::default();
        settings.set_wallhaven_api_key(Some("abc123"));

        settings.set_wallhaven_api_key(Some("  "));

        assert_eq!(settings.wallhaven_api_key(), None);
        assert_eq!(settings, Settings::default());
    }

    #[test]
    fn hiding_wallpapers_in_the_library_is_remembered() {
        let temp = tempfile::tempdir().unwrap();
        let file = temp.path().join("settings.toml");
        assert!(!Settings::load(&file).unwrap().wallhaven_hide_in_library());

        Settings::update(&file, |s| {
            s.set_wallhaven_hide_in_library(true);
            Ok(())
        })
        .unwrap();

        assert!(Settings::load(&file).unwrap().wallhaven_hide_in_library());
        let text = fs::read_to_string(&file).unwrap();
        assert!(text.contains("hide_in_library = true"), "{text}");
    }

    #[test]
    fn unknown_keys_are_ignored() {
        let temp = tempfile::tempdir().unwrap();
        let file = temp.path().join("settings.toml");
        fs::write(&file, "[appearance]\nthumbnail_size = 200\n").unwrap();

        assert_eq!(Settings::load(&file).unwrap(), Settings::default());
    }
}
