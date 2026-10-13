//! The library's portable metadata: `.backdrop/metadata.toml` in the library
//! folder.
//!
//! This file is the source of truth for everything about an image that
//! Backdrop cannot regenerate from the file itself: favorites, user tags, and
//! the provider an image came from. It is meant to be version-controlled with
//! the images, so output is deterministic (records sorted by path) and fields
//! this version doesn't know are kept as they are.

use std::collections::BTreeMap;
use std::io;
use std::path::{Component, Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::fs_util;

pub const METADATA_DIR: &str = ".backdrop";
const METADATA_FILE: &str = "metadata.toml";
const SOURCES_DIR: &str = "sources";
const FORMAT_VERSION: u32 = 1;

#[derive(Debug, thiserror::Error)]
pub enum MetadataError {
    #[error("could not read {}: {cause}", path.display())]
    Read { path: PathBuf, cause: io::Error },
    #[error("could not parse {}: {cause}", path.display())]
    Parse {
        path: PathBuf,
        cause: toml::de::Error,
    },
    #[error("could not write {}: {cause}", path.display())]
    Write { path: PathBuf, cause: io::Error },
    #[error("could not serialize library metadata: {0}")]
    Serialize(toml::ser::Error),
    #[error("{} was written by a newer version of Backdrop", path.display())]
    NewerVersion { path: PathBuf },
    #[error("{} can't have metadata: only paths inside the library with UTF-8 names can", path.display())]
    UnsupportedPath { path: PathBuf },
}

/// All image records of one library.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct LibraryMetadata {
    version: u32,
    /// Keyed by path relative to the library root, with `/` separators.
    #[serde(default)]
    images: BTreeMap<String, ImageMetadata>,
    #[serde(flatten)]
    unknown: toml::Table,
}

/// What the user has recorded about one image.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ImageMetadata {
    /// Content hash, used to find the image again after it is renamed or moved.
    hash: String,
    #[serde(default, skip_serializing_if = "is_false")]
    favorite: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    tags: Vec<String>,
    /// The provider item the image came from, as `provider/id`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    source: Option<String>,
    #[serde(flatten)]
    unknown: toml::Table,
}

fn is_false(value: &bool) -> bool {
    !value
}

impl Default for LibraryMetadata {
    fn default() -> Self {
        Self {
            version: FORMAT_VERSION,
            images: BTreeMap::new(),
            unknown: toml::Table::new(),
        }
    }
}

impl LibraryMetadata {
    pub fn file(root: &Path) -> PathBuf {
        root.join(METADATA_DIR).join(METADATA_FILE)
    }

    /// Reads the metadata of the library at `root`. A missing file yields no
    /// records.
    pub fn load(root: &Path) -> Result<Self, MetadataError> {
        let path = Self::file(root);
        let text = match std::fs::read_to_string(&path) {
            Ok(text) => text,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Self::default()),
            Err(cause) => return Err(MetadataError::Read { path, cause }),
        };
        let metadata: Self = toml::from_str(&text).map_err(|cause| MetadataError::Parse {
            path: path.clone(),
            cause,
        })?;
        if metadata.version > FORMAT_VERSION {
            return Err(MetadataError::NewerVersion { path });
        }
        Ok(metadata)
    }

    /// Writes the metadata. `.backdrop/` is not created just to store nothing.
    pub fn save(&self, root: &Path) -> Result<(), MetadataError> {
        let path = Self::file(root);
        if self.images.is_empty() && self.unknown.is_empty() && !path.exists() {
            return Ok(());
        }
        let text = toml::to_string_pretty(self).map_err(MetadataError::Serialize)?;
        fs_util::write_atomically(&path, text.as_bytes())
            .map_err(|cause| MetadataError::Write { path, cause })
    }

    /// Loads the current metadata, applies `change`, and saves it if it
    /// changed. Re-reading first keeps edits made by another process.
    pub fn update<T>(
        root: &Path,
        change: impl FnOnce(&mut Self) -> Result<T, MetadataError>,
    ) -> Result<(Self, T), MetadataError> {
        let mut metadata = Self::load(root)?;
        let before = metadata.clone();
        let value = change(&mut metadata)?;
        if metadata != before {
            metadata.save(root)?;
        }
        Ok((metadata, value))
    }

    pub fn len(&self) -> usize {
        self.images.len()
    }

    pub fn is_empty(&self) -> bool {
        self.images.is_empty()
    }

    pub fn get(&self, path: &Path) -> Option<&ImageMetadata> {
        self.images.get(key(path)?.as_str())
    }

    /// Records whose key is a valid path inside the library. Others (for
    /// example `../outside.jpg` from a hand-edited file) are kept in the file
    /// but never acted on.
    pub fn records(&self) -> impl Iterator<Item = (PathBuf, &ImageMetadata)> {
        self.images
            .iter()
            .filter_map(|(key, record)| Some((record_path(key)?, record)))
    }

    /// Whether any record refers to the provider item `source`.
    pub fn references_source(&self, source: &str) -> bool {
        self.images
            .values()
            .any(|record| record.source.as_deref() == Some(source))
    }

    /// Adds or replaces the record for `path`.
    pub(crate) fn insert(
        &mut self,
        path: &Path,
        record: ImageMetadata,
    ) -> Result<(), MetadataError> {
        let key = key(path).ok_or_else(|| MetadataError::UnsupportedPath {
            path: path.to_path_buf(),
        })?;
        self.images.insert(key, record);
        Ok(())
    }

    pub(crate) fn remove(&mut self, path: &Path) -> Option<ImageMetadata> {
        self.images.remove(key(path)?.as_str())
    }
}

impl ImageMetadata {
    pub(crate) fn new(hash: String) -> Self {
        Self {
            hash,
            favorite: false,
            tags: Vec::new(),
            source: None,
            unknown: toml::Table::new(),
        }
    }

    pub fn hash(&self) -> &str {
        &self.hash
    }

    pub(crate) fn set_hash(&mut self, hash: String) {
        self.hash = hash;
    }

    pub fn is_favorite(&self) -> bool {
        self.favorite
    }

    pub fn set_favorite(&mut self, favorite: bool) {
        self.favorite = favorite;
    }

    pub fn tags(&self) -> &[String] {
        &self.tags
    }

    pub fn set_tags(&mut self, tags: Vec<String>) {
        self.tags = tags;
    }

    pub fn source(&self) -> Option<&str> {
        self.source.as_deref()
    }

    pub fn set_source(&mut self, source: Option<String>) {
        self.source = source;
    }

    /// Whether the record holds nothing but the hash, so it can be dropped.
    pub(crate) fn is_empty(&self) -> bool {
        !self.favorite && self.tags.is_empty() && self.source.is_none() && self.unknown.is_empty()
    }
}

/// The longest tag, in characters.
pub const MAX_TAG_LENGTH: usize = 64;

/// A user tag as it is stored: trimmed, with runs of whitespace made single
/// spaces. `None` for an empty tag or one longer than [`MAX_TAG_LENGTH`].
pub fn normalize_tag(text: &str) -> Option<String> {
    let tag = text.split_whitespace().collect::<Vec<_>>().join(" ");
    (!tag.is_empty() && tag.chars().count() <= MAX_TAG_LENGTH).then_some(tag)
}

/// Whether two tags are the same tag: tags ignore case.
pub fn same_tag(a: &str, b: &str) -> bool {
    a.to_lowercase() == b.to_lowercase()
}

/// Where the provider snapshot for `source` (`provider/id`) is stored, or
/// `None` if `source` is not a valid source ID. Validation keeps a crafted
/// metadata file from pointing outside `.backdrop/sources/`.
pub(crate) fn snapshot_file(root: &Path, source: &str) -> Option<PathBuf> {
    let (provider, id) = source.split_once('/')?;
    let valid = |part: &str| {
        !part.is_empty()
            && part
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
    };
    (valid(provider) && valid(id)).then(|| {
        root.join(METADATA_DIR)
            .join(SOURCES_DIR)
            .join(provider)
            .join(format!("{id}.toml"))
    })
}

/// The key for `path`, if it can have one: relative, inside the library, and
/// UTF-8.
fn key(path: &Path) -> Option<String> {
    let mut parts = Vec::new();
    for component in path.components() {
        match component {
            Component::Normal(part) => parts.push(part.to_str()?),
            _ => return None,
        }
    }
    (!parts.is_empty()).then(|| parts.join("/"))
}

fn record_path(key: &str) -> Option<PathBuf> {
    let path = PathBuf::from(key);
    path.components()
        .all(|component| matches!(component, Component::Normal(_)))
        .then_some(path)
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;

    fn record(hash: &str) -> ImageMetadata {
        let mut record = ImageMetadata::new(hash.into());
        record.set_favorite(true);
        record
    }

    #[test]
    fn missing_file_has_no_records_and_saving_nothing_creates_nothing() {
        let library = tempfile::tempdir().unwrap();

        let metadata = LibraryMetadata::load(library.path()).unwrap();
        metadata.save(library.path()).unwrap();

        assert!(metadata.is_empty());
        assert!(!library.path().join(METADATA_DIR).exists());
    }

    #[test]
    fn records_round_trip_sorted_by_path() {
        let library = tempfile::tempdir().unwrap();
        LibraryMetadata::update(library.path(), |metadata| {
            metadata.insert(Path::new("dark/b.jpg"), record("blake3:b"))?;
            metadata.insert(Path::new("a.png"), record("blake3:a"))
        })
        .unwrap();

        let text = fs::read_to_string(LibraryMetadata::file(library.path())).unwrap();
        let loaded = LibraryMetadata::load(library.path()).unwrap();

        assert!(text.find("a.png").unwrap() < text.find("dark/b.jpg").unwrap());
        assert_eq!(loaded.len(), 2);
        assert!(loaded.get(Path::new("dark/b.jpg")).unwrap().is_favorite());
    }

    #[test]
    fn unknown_fields_survive_a_rewrite() {
        let library = tempfile::tempdir().unwrap();
        let file = LibraryMetadata::file(library.path());
        fs::create_dir_all(file.parent().unwrap()).unwrap();
        fs::write(
            &file,
            "version = 1\nfuture = \"kept\"\n\n[images.\"a.png\"]\nhash = \"blake3:a\"\nrating = 5\n",
        )
        .unwrap();

        LibraryMetadata::update(library.path(), |metadata| {
            metadata.insert(Path::new("b.png"), record("blake3:b"))
        })
        .unwrap();

        let text = fs::read_to_string(&file).unwrap();
        assert!(text.contains("future = \"kept\""));
        assert!(text.contains("rating = 5"));
    }

    #[test]
    fn newer_format_is_refused() {
        let library = tempfile::tempdir().unwrap();
        let file = LibraryMetadata::file(library.path());
        fs::create_dir_all(file.parent().unwrap()).unwrap();
        fs::write(&file, "version = 2\n").unwrap();

        assert!(matches!(
            LibraryMetadata::load(library.path()),
            Err(MetadataError::NewerVersion { .. })
        ));
    }

    #[test]
    fn paths_outside_the_library_are_ignored() {
        let library = tempfile::tempdir().unwrap();
        let file = LibraryMetadata::file(library.path());
        fs::create_dir_all(file.parent().unwrap()).unwrap();
        fs::write(
            &file,
            "version = 1\n[images.\"../outside.jpg\"]\nhash = \"x\"\n[images.\"/etc/x.jpg\"]\nhash = \"x\"\n[images.\"ok.jpg\"]\nhash = \"x\"\n",
        )
        .unwrap();

        let metadata = LibraryMetadata::load(library.path()).unwrap();
        let paths: Vec<_> = metadata.records().map(|(path, _)| path).collect();

        assert_eq!(paths, [PathBuf::from("ok.jpg")]);
        assert!(
            metadata
                .clone()
                .insert(Path::new("../x.jpg"), record("x"))
                .is_err()
        );
    }

    #[test]
    fn tags_are_trimmed_and_limited() {
        assert_eq!(
            normalize_tag("  snowy \t peaks "),
            Some("snowy peaks".to_owned())
        );
        assert_eq!(normalize_tag(" \n "), None);
        assert_eq!(normalize_tag(&"a".repeat(64)), Some("a".repeat(64)));
        assert_eq!(normalize_tag(&"a".repeat(65)), None);
        assert!(same_tag("Night", "night"));
        assert!(same_tag("Ärger", "ärger"));
    }

    #[test]
    fn snapshot_files_stay_inside_sources() {
        let root = Path::new("/library");

        assert_eq!(
            snapshot_file(root, "wallhaven/94x38z"),
            Some(PathBuf::from(
                "/library/.backdrop/sources/wallhaven/94x38z.toml"
            ))
        );
        assert_eq!(snapshot_file(root, "wallhaven/../../x"), None);
        assert_eq!(snapshot_file(root, "wallhaven"), None);
        assert_eq!(snapshot_file(root, "/etc/passwd"), None);
    }
}
