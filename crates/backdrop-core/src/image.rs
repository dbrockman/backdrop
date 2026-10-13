//! What the index knows about one image file.

use std::path::{Path, PathBuf};

/// Supported image formats, detected from file contents rather than the
/// extension.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ImageFormat {
    Jpeg,
    Png,
    Webp,
}

impl ImageFormat {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Jpeg => "jpeg",
            Self::Png => "png",
            Self::Webp => "webp",
        }
    }

    pub fn from_name(name: &str) -> Option<Self> {
        match name {
            "jpeg" => Some(Self::Jpeg),
            "png" => Some(Self::Png),
            "webp" => Some(Self::Webp),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Orientation {
    Landscape,
    Portrait,
    Square,
}

impl Orientation {
    /// The orientation of an image `width` wide and `height` tall.
    pub fn of(width: u32, height: u32) -> Self {
        match width.cmp(&height) {
            std::cmp::Ordering::Greater => Self::Landscape,
            std::cmp::Ordering::Less => Self::Portrait,
            std::cmp::Ordering::Equal => Self::Square,
        }
    }
}

/// An image in the library. Times are nanoseconds since the Unix epoch.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ImageFile {
    path: PathBuf,
    size: u64,
    modified: i64,
    added: i64,
    width: u32,
    height: u32,
    format: ImageFormat,
}

impl ImageFile {
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new(
        path: PathBuf,
        size: u64,
        modified: i64,
        added: i64,
        width: u32,
        height: u32,
        format: ImageFormat,
    ) -> Self {
        Self {
            path,
            size,
            modified,
            added,
            width,
            height,
            format,
        }
    }

    /// Path relative to the library root.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// File size in bytes.
    pub fn size(&self) -> u64 {
        self.size
    }

    /// File modification time.
    pub fn modified(&self) -> i64 {
        self.modified
    }

    /// When the file appeared in the library: its creation time where the
    /// filesystem records one, otherwise its modification time. Taken from the
    /// filesystem so it survives rebuilding the index.
    pub fn added(&self) -> i64 {
        self.added
    }

    pub fn width(&self) -> u32 {
        self.width
    }

    pub fn height(&self) -> u32 {
        self.height
    }

    pub fn format(&self) -> ImageFormat {
        self.format
    }

    pub fn orientation(&self) -> Orientation {
        Orientation::of(self.width, self.height)
    }
}
