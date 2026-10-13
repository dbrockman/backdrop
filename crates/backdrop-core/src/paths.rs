//! Locations of Backdrop's own files, following the XDG base directory spec.
//!
//! Everything Backdrop writes lives outside the wallpaper library: settings in
//! the config directory, the SQLite index in the data directory, and generated
//! thumbnails in the cache directory.

use std::path::{Path, PathBuf};

use directories::ProjectDirs;

const APPLICATION: &str = "backdrop";
const DATABASE_FILE: &str = "library.sqlite3";
const SETTINGS_FILE: &str = "settings.toml";
const THUMBNAILS_DIR: &str = "thumbnails";
const REMOTE_THUMBNAILS_DIR: &str = "remote-thumbnails";

#[derive(Debug, thiserror::Error)]
pub enum PathsError {
    #[error("could not determine the home directory")]
    NoHomeDirectory,
}

/// Resolved application directories. Resolving does not create them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AppPaths {
    config_dir: PathBuf,
    data_dir: PathBuf,
    cache_dir: PathBuf,
}

impl AppPaths {
    /// Resolves the directories for the current user from the environment.
    pub fn from_env() -> Result<Self, PathsError> {
        let dirs = ProjectDirs::from("", "", APPLICATION).ok_or(PathsError::NoHomeDirectory)?;
        Ok(Self {
            config_dir: dirs.config_dir().to_path_buf(),
            data_dir: dirs.data_dir().to_path_buf(),
            cache_dir: dirs.cache_dir().to_path_buf(),
        })
    }

    /// Uses the given directories instead of the user's XDG directories.
    pub fn new(
        config_dir: impl Into<PathBuf>,
        data_dir: impl Into<PathBuf>,
        cache_dir: impl Into<PathBuf>,
    ) -> Self {
        Self {
            config_dir: config_dir.into(),
            data_dir: data_dir.into(),
            cache_dir: cache_dir.into(),
        }
    }

    pub fn config_dir(&self) -> &Path {
        &self.config_dir
    }

    pub fn data_dir(&self) -> &Path {
        &self.data_dir
    }

    pub fn cache_dir(&self) -> &Path {
        &self.cache_dir
    }

    pub fn settings_file(&self) -> PathBuf {
        self.config_dir.join(SETTINGS_FILE)
    }

    pub fn database_file(&self) -> PathBuf {
        self.data_dir.join(DATABASE_FILE)
    }

    pub fn thumbnails_dir(&self) -> PathBuf {
        self.cache_dir.join(THUMBNAILS_DIR)
    }

    /// Thumbnails of provider wallpapers, downloaded while browsing.
    pub fn remote_thumbnails_dir(&self) -> PathBuf {
        self.cache_dir.join(REMOTE_THUMBNAILS_DIR)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn files_live_in_their_xdg_directories() {
        let paths = AppPaths::new("/config", "/data", "/cache");

        assert_eq!(paths.settings_file(), Path::new("/config/settings.toml"));
        assert_eq!(paths.database_file(), Path::new("/data/library.sqlite3"));
        assert_eq!(paths.thumbnails_dir(), Path::new("/cache/thumbnails"));
        assert_eq!(
            paths.remote_thumbnails_dir(),
            Path::new("/cache/remote-thumbnails")
        );
    }
}
