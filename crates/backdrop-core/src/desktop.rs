//! Reading and setting the desktop wallpaper through the user's desktop shell.
//!
//! The shell owns the wallpaper; Backdrop only asks it which image is shown and
//! tells it to show another. Backends talk to their shell through its own
//! command-line interface, so no Backdrop process stays running.

use std::ffi::OsString;
use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;

#[derive(Debug, thiserror::Error)]
pub enum DesktopError {
    #[error("could not run {program}: {cause}")]
    Spawn { program: String, cause: io::Error },
    #[error("{program} failed: {message}")]
    Failed { program: String, message: String },
    #[error("{} is not a file", path.display())]
    NotAFile { path: PathBuf },
}

/// A desktop shell that shows wallpapers.
pub trait WallpaperBackend: Send + Sync {
    /// The shell's name, for messages.
    fn name(&self) -> &'static str;

    /// The image shown as the wallpaper, if any. On several monitors this is
    /// the default wallpaper.
    fn current_wallpaper(&self) -> Result<Option<PathBuf>, DesktopError>;

    /// Shows `image` as the wallpaper on every monitor.
    fn set_wallpaper(&self, image: &Path) -> Result<(), DesktopError>;
}

/// The backend for the desktop shell running in this session, if Backdrop
/// supports it.
pub fn detect() -> Option<Box<dyn WallpaperBackend>> {
    let noctalia = Noctalia::new();
    noctalia
        .current_wallpaper()
        .is_ok()
        .then(|| Box::new(noctalia) as Box<dyn WallpaperBackend>)
}

/// The Noctalia shell, through `noctalia msg`.
#[derive(Clone, Debug)]
pub struct Noctalia {
    program: OsString,
    /// Arguments placed before `msg`.
    leading_args: Vec<OsString>,
}

impl Noctalia {
    pub fn new() -> Self {
        Self::with_program("noctalia", [] as [OsString; 0])
    }

    /// Runs `program leading_args… msg …` in place of `noctalia msg …`, for
    /// tests.
    pub fn with_program(
        program: impl Into<OsString>,
        leading_args: impl IntoIterator<Item = impl Into<OsString>>,
    ) -> Self {
        Self {
            program: program.into(),
            leading_args: leading_args.into_iter().map(Into::into).collect(),
        }
    }

    /// Runs `noctalia msg <args>` and returns its trimmed output.
    fn message(&self, args: &[&std::ffi::OsStr]) -> Result<String, DesktopError> {
        let program = self.program.to_string_lossy().into_owned();
        let output = Command::new(&self.program)
            .args(&self.leading_args)
            .arg("msg")
            .args(args)
            .output()
            .map_err(|cause| DesktopError::Spawn {
                program: program.clone(),
                cause,
            })?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            let message = stderr
                .trim()
                .strip_prefix("error: ")
                .unwrap_or(stderr.trim())
                .to_owned();
            return Err(DesktopError::Failed {
                program,
                message: if message.is_empty() {
                    output.status.to_string()
                } else {
                    message
                },
            });
        }
        Ok(String::from_utf8_lossy(&output.stdout).trim().to_owned())
    }
}

impl Default for Noctalia {
    fn default() -> Self {
        Self::new()
    }
}

impl WallpaperBackend for Noctalia {
    fn name(&self) -> &'static str {
        "Noctalia"
    }

    fn current_wallpaper(&self) -> Result<Option<PathBuf>, DesktopError> {
        let path = self.message(&["wallpaper-get".as_ref()])?;
        Ok((!path.is_empty()).then(|| PathBuf::from(path)))
    }

    fn set_wallpaper(&self, image: &Path) -> Result<(), DesktopError> {
        // Noctalia accepts any path and persists it, so check before asking.
        if !image.is_file() {
            return Err(DesktopError::NotAFile {
                path: image.to_path_buf(),
            });
        }
        let image = std::path::absolute(image).map_err(|_| DesktopError::NotAFile {
            path: image.to_path_buf(),
        })?;
        self.message(&["wallpaper-set".as_ref(), image.as_os_str()])?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;

    /// A stand-in for `noctalia` that remembers the last wallpaper set, so
    /// tests never touch the real desktop.
    fn fake_noctalia(dir: &Path) -> Noctalia {
        let state = dir.join("wallpaper");
        let script = dir.join("noctalia");
        fs::write(
            &script,
            format!(
                "#!/bin/sh\n\
                 case \"$2\" in\n\
                 wallpaper-get) cat '{state}' 2>/dev/null || true ;;\n\
                 wallpaper-set) printf '%s\\n' \"$3\" > '{state}' ;;\n\
                 *) echo \"error: unknown command $2\" >&2; exit 1 ;;\n\
                 esac\n",
                state = state.display()
            ),
        )
        .unwrap();

        // Run through `sh`: executing a file just written can fail with "text
        // file busy" while other test threads fork.
        Noctalia::with_program("sh", [script])
    }

    #[test]
    fn sets_and_reads_the_wallpaper() {
        let dir = tempfile::tempdir().unwrap();
        let noctalia = fake_noctalia(dir.path());
        let image = dir.path().join("a.jpg");
        fs::write(&image, b"").unwrap();

        assert_eq!(noctalia.current_wallpaper().unwrap(), None);
        noctalia.set_wallpaper(&image).unwrap();

        assert_eq!(noctalia.current_wallpaper().unwrap(), Some(image));
    }

    #[test]
    fn missing_images_are_not_sent_to_the_shell() {
        let dir = tempfile::tempdir().unwrap();
        let noctalia = fake_noctalia(dir.path());

        let result = noctalia.set_wallpaper(&dir.path().join("missing.jpg"));

        assert!(matches!(result, Err(DesktopError::NotAFile { .. })));
        assert_eq!(noctalia.current_wallpaper().unwrap(), None);
    }

    #[test]
    fn shell_errors_are_reported() {
        let dir = tempfile::tempdir().unwrap();
        let script = dir.path().join("noctalia");
        fs::write(
            &script,
            "#!/bin/sh\necho 'error: not running' >&2\nexit 1\n",
        )
        .unwrap();

        let error = Noctalia::with_program("sh", [script])
            .current_wallpaper()
            .unwrap_err();

        assert!(
            error.to_string().ends_with("failed: not running"),
            "{error}"
        );
    }

    #[test]
    fn a_missing_program_is_reported() {
        let error = Noctalia::with_program("/nonexistent/noctalia", [] as [OsString; 0])
            .current_wallpaper()
            .unwrap_err();

        assert!(matches!(error, DesktopError::Spawn { .. }));
    }
}
