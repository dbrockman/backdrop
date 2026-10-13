//! Provider snapshots: `.backdrop/sources/<provider>/<id>.toml`.
//!
//! A snapshot keeps what a provider said about a wallpaper when it was
//! downloaded, because the provider may later remove it (spec §5.2). It holds
//! normalized fields, not the raw API response, and is written once; fields
//! this version doesn't know are kept if it is rewritten.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use toml::value::Datetime;

use crate::fs_util;
use crate::metadata;
use crate::providers::{ContentRating, RemoteWallpaper};

const FORMAT_VERSION: u32 = 1;

#[derive(Debug, thiserror::Error)]
pub enum SnapshotError {
    #[error("{name} is not a valid provider source")]
    InvalidSource { name: String },
    #[error("could not read {}: {cause}", path.display())]
    Read { path: PathBuf, cause: io::Error },
    #[error("could not parse {}: {cause}", path.display())]
    Parse {
        path: PathBuf,
        cause: toml::de::Error,
    },
    #[error("could not write {}: {cause}", path.display())]
    Write { path: PathBuf, cause: io::Error },
    #[error("could not serialize a provider snapshot: {0}")]
    Serialize(toml::ser::Error),
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Snapshot {
    version: u32,
    provider: String,
    id: String,
    /// When Backdrop fetched this from the provider, in UTC.
    fetched: Datetime,
    page_url: String,
    image_url: String,
    width: u32,
    height: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    file_size: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    media_type: Option<String>,
    rating: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    category: Option<String>,
    /// As the provider reports it, which may have no time zone.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    uploaded: Option<Datetime>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    uploader: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    source_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    views: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    favorites: Option<u64>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    colors: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    tags: Vec<SnapshotTag>,
    #[serde(flatten)]
    unknown: toml::Table,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SnapshotTag {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub category: Option<String>,
    /// The provider's id for the tag, to search for exactly this tag.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
}

impl Snapshot {
    /// A snapshot of `wallpaper`, fetched at `fetched`.
    pub fn new(wallpaper: &RemoteWallpaper, fetched: chrono::DateTime<chrono::Utc>) -> Self {
        let fetched = fetched
            .format("%Y-%m-%dT%H:%M:%SZ")
            .to_string()
            .parse()
            .expect("an RFC 3339 time is a TOML datetime");
        Self {
            version: FORMAT_VERSION,
            provider: wallpaper.provider().to_owned(),
            id: wallpaper.id().to_owned(),
            fetched,
            page_url: wallpaper.page_url().to_owned(),
            image_url: wallpaper.image_url().to_owned(),
            width: wallpaper.width(),
            height: wallpaper.height(),
            file_size: wallpaper.file_size(),
            media_type: wallpaper.media_type().map(str::to_owned),
            rating: wallpaper.rating().as_str().to_owned(),
            category: wallpaper.category().map(str::to_owned),
            uploaded: wallpaper.uploaded().cloned(),
            uploader: wallpaper.uploader().map(str::to_owned),
            source_url: wallpaper.source_url().map(str::to_owned),
            views: wallpaper.views(),
            favorites: wallpaper.favorites(),
            colors: wallpaper.colors().to_vec(),
            tags: wallpaper
                .tags()
                .unwrap_or_default()
                .iter()
                .map(|tag| SnapshotTag {
                    name: tag.name.clone(),
                    category: tag.category.clone(),
                    id: tag.id.clone(),
                })
                .collect(),
            unknown: toml::Table::new(),
        }
    }

    /// The snapshot of `source` (`provider/id`) in the library at `root`, if
    /// there is one.
    pub fn load(root: &Path, source: &str) -> Result<Option<Self>, SnapshotError> {
        let path = file(root, source)?;
        let text = match fs::read_to_string(&path) {
            Ok(text) => text,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(cause) => return Err(SnapshotError::Read { path, cause }),
        };
        toml::from_str(&text)
            .map(Some)
            .map_err(|cause| SnapshotError::Parse { path, cause })
    }

    /// Writes the snapshot unless the library already has one for this
    /// wallpaper, which is kept as the record of what was first downloaded.
    pub(crate) fn save_if_missing(&self, root: &Path) -> Result<(), SnapshotError> {
        let path = file(root, &self.source())?;
        if path.exists() {
            return Ok(());
        }
        let text = toml::to_string_pretty(self).map_err(SnapshotError::Serialize)?;
        fs_util::write_atomically(&path, text.as_bytes())
            .map_err(|cause| SnapshotError::Write { path, cause })
    }

    /// `provider/id`, as library metadata refers to the wallpaper.
    pub fn source(&self) -> String {
        format!("{}/{}", self.provider, self.id)
    }

    pub fn fetched(&self) -> &Datetime {
        &self.fetched
    }

    pub fn page_url(&self) -> &str {
        &self.page_url
    }

    /// The content rating, or `None` if this version doesn't know it.
    pub fn rating(&self) -> Option<ContentRating> {
        match self.rating.as_str() {
            "sfw" => Some(ContentRating::Sfw),
            "sketchy" => Some(ContentRating::Sketchy),
            "nsfw" => Some(ContentRating::Nsfw),
            _ => None,
        }
    }

    pub fn uploaded(&self) -> Option<&Datetime> {
        self.uploaded.as_ref()
    }

    pub fn tags(&self) -> &[SnapshotTag] {
        &self.tags
    }
}

fn file(root: &Path, source: &str) -> Result<PathBuf, SnapshotError> {
    metadata::snapshot_file(root, source).ok_or_else(|| SnapshotError::InvalidSource {
        name: source.to_owned(),
    })
}

#[cfg(test)]
mod tests {
    use chrono::TimeZone as _;

    use super::*;
    use crate::providers::Tag;

    #[test]
    fn snapshots_round_trip_as_readable_toml() {
        let library = tempfile::tempdir().unwrap();
        let mut wallpaper = RemoteWallpaper::for_test("wallhaven", "94x38z");
        wallpaper.uploaded = Some("2026-10-02 13:39:14".parse().unwrap());
        wallpaper.tags = Some(vec![Tag {
            name: "mountains".to_owned(),
            category: Some("Nature".to_owned()),
            id: Some("328".to_owned()),
        }]);
        let fetched = chrono::Utc
            .with_ymd_and_hms(2026, 10, 13, 15, 31, 25)
            .unwrap();

        let snapshot = Snapshot::new(&wallpaper, fetched);
        snapshot.save_if_missing(library.path()).unwrap();

        let file = library
            .path()
            .join(".backdrop/sources/wallhaven/94x38z.toml");
        let text = fs::read_to_string(&file).unwrap();
        assert!(text.starts_with("version = 1\nprovider = \"wallhaven\"\nid = \"94x38z\"\n"));
        assert!(text.contains("fetched = 2026-10-13T15:31:25Z\n"));
        assert!(text.contains("uploaded = 2026-10-02T13:39:14\n"));
        assert!(
            text.contains("[[tags]]\nname = \"mountains\"\ncategory = \"Nature\"\nid = \"328\"\n")
        );
        let loaded = Snapshot::load(library.path(), "wallhaven/94x38z")
            .unwrap()
            .unwrap();
        assert_eq!(loaded, snapshot);
        assert_eq!(loaded.rating(), Some(ContentRating::Sfw));
    }

    #[test]
    fn snapshots_from_before_tag_ids_still_load() {
        let library = tempfile::tempdir().unwrap();
        let file = library
            .path()
            .join(".backdrop/sources/wallhaven/94x38z.toml");
        fs::create_dir_all(file.parent().unwrap()).unwrap();
        fs::write(
            &file,
            "version = 1\nprovider = \"wallhaven\"\nid = \"94x38z\"\n\
             fetched = 2026-10-13T15:31:25Z\npage_url = \"https://wallhaven.cc/w/94x38z\"\n\
             image_url = \"https://w.wallhaven.cc/full/94/wallhaven-94x38z.jpg\"\n\
             width = 20\nheight = 10\nrating = \"sfw\"\n\n[[tags]]\nname = \"mountains\"\n",
        )
        .unwrap();

        let snapshot = Snapshot::load(library.path(), "wallhaven/94x38z")
            .unwrap()
            .unwrap();

        assert_eq!(snapshot.tags()[0].name, "mountains");
        assert_eq!(snapshot.tags()[0].id, None);
    }

    #[test]
    fn an_existing_snapshot_is_kept() {
        let library = tempfile::tempdir().unwrap();
        let file = library
            .path()
            .join(".backdrop/sources/wallhaven/94x38z.toml");
        fs::create_dir_all(file.parent().unwrap()).unwrap();
        fs::write(&file, "kept").unwrap();

        Snapshot::new(
            &RemoteWallpaper::for_test("wallhaven", "94x38z"),
            chrono::Utc::now(),
        )
        .save_if_missing(library.path())
        .unwrap();

        assert_eq!(fs::read_to_string(&file).unwrap(), "kept");
    }

    #[test]
    fn missing_snapshots_load_as_none() {
        let library = tempfile::tempdir().unwrap();
        assert_eq!(
            Snapshot::load(library.path(), "wallhaven/94x38z").unwrap(),
            None
        );
        assert!(matches!(
            Snapshot::load(library.path(), "../x"),
            Err(SnapshotError::InvalidSource { .. })
        ));
    }
}
