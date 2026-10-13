//! Finding image files in the library folder.
//!
//! Discovery walks the library recursively and reads the header of each new or
//! changed image. It skips hidden files and folders (including `.backdrop/` and
//! `.git/`) and does not follow symbolic links.

use std::collections::{HashMap, HashSet};
use std::fs::File;
use std::io::{self, BufReader};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use image::metadata::Orientation;
use image::{ImageDecoder as _, ImageError, ImageReader};
use walkdir::WalkDir;

use crate::image::{ImageFile, ImageFormat};

const EXTENSIONS: &[&str] = &["jpg", "jpeg", "png", "webp"];

#[derive(Debug, thiserror::Error)]
#[error("could not read the library folder {}: {cause}", path.display())]
pub struct DiscoveryError {
    path: PathBuf,
    cause: walkdir::Error,
}

/// A file or folder that could not be indexed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ScanIssue {
    path: PathBuf,
    message: String,
}

impl ScanIssue {
    /// Path relative to the library root.
    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn message(&self) -> &str {
        &self.message
    }
}

/// The size and modification time an indexed file had when it was read. A file
/// whose stamp is unchanged is not read again.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct FileStamp {
    pub(crate) size: u64,
    pub(crate) modified: i64,
}

#[derive(Debug, Default)]
pub(crate) struct Discovery {
    /// New or changed images, read from disk.
    pub(crate) changed: Vec<ImageFile>,
    /// Every image found, changed or not.
    pub(crate) found: HashSet<PathBuf>,
    /// Paths that could not be read. Indexed images at or below these paths
    /// may still exist, so they must not be treated as removed.
    pub(crate) unreadable: Vec<PathBuf>,
    pub(crate) issues: Vec<ScanIssue>,
}

impl Discovery {
    /// Whether an indexed image that was not found may still exist.
    pub(crate) fn may_exist(&self, path: &Path) -> bool {
        self.unreadable
            .iter()
            .any(|prefix| path.starts_with(prefix))
    }

    fn report_unreadable(&mut self, path: PathBuf, message: String) {
        self.issues.push(ScanIssue {
            path: path.clone(),
            message,
        });
        self.unreadable.push(path);
    }
}

/// Walks `root`, reading images whose stamps differ from `known`.
pub(crate) fn discover(
    root: &Path,
    known: &HashMap<PathBuf, FileStamp>,
) -> Result<Discovery, DiscoveryError> {
    let mut discovery = Discovery::default();
    let walker = WalkDir::new(root)
        .follow_links(false)
        .into_iter()
        .filter_entry(|entry| entry.depth() == 0 || !is_hidden(entry.file_name()));

    for entry in walker {
        let entry = match entry {
            Ok(entry) => entry,
            Err(cause) if cause.depth() == 0 => {
                return Err(DiscoveryError {
                    path: root.to_path_buf(),
                    cause,
                });
            }
            Err(cause) => {
                let path = cause
                    .path()
                    .map(|path| relative(root, path))
                    .unwrap_or_default();
                discovery.report_unreadable(path, cause.to_string());
                continue;
            }
        };
        if !entry.file_type().is_file() || !has_image_extension(entry.path()) {
            continue;
        }

        let path = relative(root, entry.path());
        let metadata = match entry.metadata() {
            Ok(metadata) => metadata,
            Err(cause) => {
                discovery.report_unreadable(path, cause.to_string());
                continue;
            }
        };
        let modified = metadata.modified().map(unix_nanos).unwrap_or_default();
        let stamp = FileStamp {
            size: metadata.len(),
            modified,
        };
        if known.get(&path) == Some(&stamp) {
            discovery.found.insert(path);
            continue;
        }

        match probe(entry.path()) {
            Ok((format, width, height)) => {
                let added = metadata.created().map(unix_nanos).unwrap_or(modified);
                discovery.found.insert(path.clone());
                discovery.changed.push(ImageFile::new(
                    path, stamp.size, modified, added, width, height, format,
                ));
            }
            Err(ProbeError::Io(cause)) => discovery.report_unreadable(path, cause.to_string()),
            // Not found, so an earlier index entry for it is removed.
            Err(ProbeError::Invalid(message)) => discovery.issues.push(ScanIssue {
                path,
                message: message.into(),
            }),
        }
    }
    Ok(discovery)
}

/// Why a file could not be read as an image.
pub(crate) enum ProbeError {
    Io(io::Error),
    Invalid(&'static str),
}

const UNSUPPORTED: &str = "not a JPEG, PNG, or WebP image";

/// Reads the format and the upright dimensions from the file header: an image
/// whose EXIF orientation rotates it a quarter turn has its width and height
/// swapped, so orientation filters match what the user sees.
pub(crate) fn probe(path: &Path) -> Result<(ImageFormat, u32, u32), ProbeError> {
    let file = File::open(path).map_err(ProbeError::Io)?;
    let reader = ImageReader::new(BufReader::new(file))
        .with_guessed_format()
        .map_err(ProbeError::Io)?;
    let format = match reader.format() {
        Some(image::ImageFormat::Jpeg) => ImageFormat::Jpeg,
        Some(image::ImageFormat::Png) => ImageFormat::Png,
        Some(image::ImageFormat::WebP) => ImageFormat::Webp,
        _ => return Err(ProbeError::Invalid(UNSUPPORTED)),
    };
    let mut decoder = reader.into_decoder().map_err(probe_error)?;
    let (width, height) = decoder.dimensions();
    if width == 0 || height == 0 {
        return Err(ProbeError::Invalid("image has no pixels"));
    }
    // A missing or unreadable orientation means the image is stored upright.
    let orientation = decoder.orientation().unwrap_or(Orientation::NoTransforms);
    let quarter_turn = matches!(
        orientation,
        Orientation::Rotate90
            | Orientation::Rotate270
            | Orientation::Rotate90FlipH
            | Orientation::Rotate270FlipH
    );
    Ok(if quarter_turn {
        (format, height, width)
    } else {
        (format, width, height)
    })
}

impl std::fmt::Display for ProbeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(cause) => cause.fmt(f),
            Self::Invalid(message) => f.write_str(message),
        }
    }
}

fn probe_error(error: ImageError) -> ProbeError {
    match error {
        ImageError::IoError(cause) if cause.kind() != io::ErrorKind::UnexpectedEof => {
            ProbeError::Io(cause)
        }
        ImageError::Unsupported(_) => ProbeError::Invalid(UNSUPPORTED),
        _ => ProbeError::Invalid("the image file is damaged or incomplete"),
    }
}

fn is_hidden(name: &std::ffi::OsStr) -> bool {
    name.as_encoded_bytes().starts_with(b".")
}

fn has_image_extension(path: &Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| {
            EXTENSIONS
                .iter()
                .any(|supported| extension.eq_ignore_ascii_case(supported))
        })
}

fn relative(root: &Path, path: &Path) -> PathBuf {
    path.strip_prefix(root).unwrap_or(path).to_path_buf()
}

pub(crate) fn unix_nanos(time: SystemTime) -> i64 {
    match time.duration_since(UNIX_EPOCH) {
        Ok(after) => i64::try_from(after.as_nanos()).unwrap_or(i64::MAX),
        Err(before) => i64::try_from(before.duration().as_nanos()).map_or(i64::MIN, |n| -n),
    }
}
