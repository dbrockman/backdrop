//! Downloading provider wallpapers into the library (spec §3.5).
//!
//! A download streams into a hidden temporary file in its destination folder,
//! which scans skip, so an interrupted download is never taken for an image.
//! Only a complete, readable image is given its final name, and never over an
//! existing file. Then the provider snapshot and the metadata record linking
//! the image to its source are written, and the library is scanned.

use std::ffi::OsStr;
use std::fs;
use std::io::{self, BufWriter, Write as _};
use std::path::{Component, Path, PathBuf};

use super::{Library, LibraryError};
use crate::discovery;
use crate::providers::{Progress, Provider, RemoteWallpaper};
use crate::snapshot::Snapshot;

/// The outcome of [`Library::download`]. Paths are relative to the library
/// root.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Downloaded {
    Saved(PathBuf),
    /// The wallpaper was already in the library, so nothing was downloaded.
    AlreadyInLibrary(PathBuf),
}

impl Library {
    /// The indexed image that came from wallpaper `id` of `provider`: one
    /// whose metadata names it as the source, or else one named by the
    /// provider's file name convention. Uses the index as of the last scan.
    pub fn find_source(
        &self,
        provider: &dyn Provider,
        id: &str,
    ) -> Result<Option<PathBuf>, LibraryError> {
        let indexed = self.indexed_sizes()?;
        let source = format!("{}/{id}", provider.id());
        let recorded = self.metadata()?.records().find_map(|(path, record)| {
            (record.source() == Some(source.as_str()) && indexed.contains_key(&path))
                .then_some(path)
        });
        if recorded.is_some() {
            return Ok(recorded);
        }
        let named = indexed.into_keys().filter(|path| {
            path.file_name()
                .and_then(OsStr::to_str)
                .and_then(|name| provider.id_from_file_name(name))
                .is_some_and(|found| found == id)
        });
        Ok(named.min())
    }

    /// Downloads the original of `wallpaper` into `folder`, a folder in the
    /// library given relative to its root (empty for the root itself), unless
    /// the library already has it. The library should have been scanned
    /// first, so an existing copy is found.
    pub fn download(
        &mut self,
        provider: &dyn Provider,
        wallpaper: &RemoteWallpaper,
        folder: &Path,
        progress: Progress,
    ) -> Result<Downloaded, LibraryError> {
        if let Some(path) = self.find_source(provider, wallpaper.id())? {
            return Ok(Downloaded::AlreadyInLibrary(path));
        }
        let dir = self.download_folder(folder)?;
        // Search results leave out the tags, which the snapshot should keep.
        let details;
        let wallpaper = match wallpaper.tags() {
            Some(_) => wallpaper,
            None => {
                details = provider.wallpaper(wallpaper.id())?;
                &details
            }
        };
        let fetched = chrono::Utc::now();

        let write_error = |cause: io::Error| LibraryError::Download {
            path: dir.clone(),
            cause,
        };
        let mut temp = tempfile::Builder::new()
            .prefix(".backdrop-download-")
            .tempfile_in(&dir)
            .map_err(write_error)?;
        let mut writer = BufWriter::new(temp.as_file_mut());
        provider.download(wallpaper, &mut writer, progress)?;
        writer.flush().map_err(write_error)?;
        drop(writer);
        temp.as_file().sync_all().map_err(write_error)?;

        let (format, _, _) =
            discovery::probe(temp.path()).map_err(|error| LibraryError::NotAnImage {
                wallpaper: wallpaper.source(),
                message: error.to_string(),
            })?;
        let name = provider.file_name(wallpaper.id(), format.extension());
        let path = folder.join(&name);
        let file = dir.join(&name);
        temp.persist_noclobber(&file).map_err(|error| {
            if error.error.kind() == io::ErrorKind::AlreadyExists {
                LibraryError::FileExists { path: file.clone() }
            } else {
                LibraryError::Download {
                    path: file.clone(),
                    cause: error.error,
                }
            }
        })?;

        // The image is saved; from here on, a failure leaves it in the library
        // without its snapshot, still recognized by its file name.
        self.scan()?;
        Snapshot::new(wallpaper, fetched).save_if_missing(&self.root)?;
        let source = wallpaper.source();
        self.update_image(&path, |record| record.set_source(Some(source)))?;
        Ok(Downloaded::Saved(path))
    }

    /// Creates `folder` in the library if needed and returns its full path.
    /// It must be a plain folder of the library: not hidden, since scans skip
    /// hidden folders, and not reached through a symbolic link, which scans
    /// don't follow and which could lead outside the library.
    fn download_folder(&self, folder: &Path) -> Result<PathBuf, LibraryError> {
        let invalid = || LibraryError::InvalidDownloadFolder {
            path: folder.to_path_buf(),
        };
        let plain = folder.components().all(|component| match component {
            Component::Normal(name) => !name.as_encoded_bytes().starts_with(b"."),
            _ => false,
        });
        if !plain {
            return Err(invalid());
        }
        let dir = self.root.join(folder);
        fs::create_dir_all(&dir).map_err(|cause| LibraryError::Download {
            path: dir.clone(),
            cause,
        })?;
        let resolved = |path: &Path| {
            fs::canonicalize(path).map_err(|cause| LibraryError::NotFound {
                path: path.to_path_buf(),
                cause,
            })
        };
        if resolved(&dir)? != resolved(&self.root)?.join(folder) {
            return Err(invalid());
        }
        Ok(dir)
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::sync::Mutex;

    use super::*;
    use crate::index::Index;
    use crate::metadata::LibraryMetadata;
    use crate::providers::{Category, ProviderError, SearchPage, SearchQuery, SearchSort, Tag};
    use crate::test_images::{jpeg, png};

    /// A provider serving images from memory, named `fake-{id}.{extension}`.
    #[derive(Default)]
    struct FakeProvider {
        images: HashMap<String, Vec<u8>>,
        /// Ids whose download fails halfway.
        failing: Vec<String>,
        detail_requests: Mutex<usize>,
    }

    impl FakeProvider {
        fn with(images: &[(&str, Vec<u8>)]) -> Self {
            Self {
                images: images
                    .iter()
                    .map(|(id, bytes)| (id.to_string(), bytes.clone()))
                    .collect(),
                ..Self::default()
            }
        }
    }

    impl Provider for FakeProvider {
        fn id(&self) -> &'static str {
            "fake"
        }

        fn name(&self) -> &'static str {
            "Fake"
        }

        fn categories(&self) -> &'static [Category] {
            &[]
        }

        fn sorts(&self) -> &'static [SearchSort] {
            &[]
        }

        fn search(&self, _: &SearchQuery) -> Result<SearchPage, ProviderError> {
            unimplemented!()
        }

        fn wallpaper(&self, id: &str) -> Result<RemoteWallpaper, ProviderError> {
            *self.detail_requests.lock().unwrap() += 1;
            let mut wallpaper = RemoteWallpaper::for_test("fake", id);
            wallpaper.tags = Some(vec![Tag {
                name: "mountains".to_owned(),
                category: Some("Nature".to_owned()),
            }]);
            Ok(wallpaper)
        }

        fn download(
            &self,
            wallpaper: &RemoteWallpaper,
            to: &mut dyn io::Write,
            progress: Progress,
        ) -> Result<u64, ProviderError> {
            let bytes = &self.images[wallpaper.id()];
            if self.failing.iter().any(|id| id == wallpaper.id()) {
                to.write_all(&bytes[..bytes.len() / 2]).unwrap();
                return Err(ProviderError::Status {
                    provider: "Fake",
                    status: 500,
                });
            }
            to.write_all(bytes).map_err(ProviderError::Write)?;
            progress(bytes.len() as u64, Some(bytes.len() as u64));
            Ok(bytes.len() as u64)
        }

        fn file_name(&self, id: &str, extension: &str) -> String {
            format!("fake-{id}.{extension}")
        }

        fn id_from_file_name(&self, file_name: &str) -> Option<String> {
            let (id, _) = file_name.strip_prefix("fake-")?.split_once('.')?;
            Some(id.to_owned())
        }

        fn parse_id(&self, text: &str) -> Option<String> {
            Some(text.to_owned())
        }
    }

    struct Fixture {
        dir: tempfile::TempDir,
        library: Library,
    }

    impl Fixture {
        fn new() -> Self {
            let dir = tempfile::tempdir().unwrap();
            let root = dir.path().join("library");
            fs::create_dir(&root).unwrap();
            let mut library = Library::with_index(&root, Index::in_memory().unwrap());
            library.scan().unwrap();
            Self { dir, library }
        }

        fn root(&self) -> &Path {
            self.library.root()
        }

        fn download(
            &mut self,
            provider: &FakeProvider,
            id: &str,
            folder: &str,
        ) -> Result<Downloaded, LibraryError> {
            let wallpaper = RemoteWallpaper::for_test("fake", id);
            self.library
                .download(provider, &wallpaper, Path::new(folder), &mut |_, _| {})
        }

        /// Every file under the library root, hidden ones included.
        fn files(&self) -> Vec<String> {
            let mut files: Vec<_> = walkdir::WalkDir::new(self.root())
                .into_iter()
                .map(Result::unwrap)
                .filter(|entry| entry.file_type().is_file())
                .map(|entry| {
                    let path = entry.path().strip_prefix(self.root()).unwrap();
                    path.display().to_string()
                })
                .collect();
            files.sort();
            files
        }
    }

    #[test]
    fn download_saves_the_image_with_its_source_and_snapshot() {
        let mut fixture = Fixture::new();
        let provider = FakeProvider::with(&[("abc123", png(20, 10, 1))]);

        let downloaded = fixture.download(&provider, "abc123", "").unwrap();

        assert_eq!(
            downloaded,
            Downloaded::Saved(PathBuf::from("fake-abc123.png"))
        );
        assert_eq!(
            fs::read(fixture.root().join("fake-abc123.png")).unwrap(),
            png(20, 10, 1)
        );
        let metadata = LibraryMetadata::load(fixture.root()).unwrap();
        let record = metadata.get(Path::new("fake-abc123.png")).unwrap();
        assert_eq!(record.source(), Some("fake/abc123"));
        assert!(record.hash().starts_with("blake3:"));
        let snapshot = Snapshot::load(fixture.root(), "fake/abc123")
            .unwrap()
            .unwrap();
        assert_eq!(snapshot.page_url(), "https://example.com/w/abc123");
        assert_eq!(snapshot.tags()[0].name, "mountains");
        // The image is indexed.
        let images = fixture
            .library
            .images(crate::query::ImageOrder::Path)
            .unwrap();
        assert_eq!(images.len(), 1);
        assert_eq!(images[0].path(), Path::new("fake-abc123.png"));
    }

    #[test]
    fn the_file_name_follows_the_downloaded_format() {
        let mut fixture = Fixture::new();
        let provider = FakeProvider::with(&[("abc123", jpeg(20, 10))]);

        let downloaded = fixture.download(&provider, "abc123", "").unwrap();

        assert_eq!(
            downloaded,
            Downloaded::Saved(PathBuf::from("fake-abc123.jpg"))
        );
    }

    #[test]
    fn download_into_a_new_folder() {
        let mut fixture = Fixture::new();
        let provider = FakeProvider::with(&[("abc123", png(20, 10, 1))]);

        let downloaded = fixture.download(&provider, "abc123", "dark/new").unwrap();

        assert_eq!(
            downloaded,
            Downloaded::Saved(PathBuf::from("dark/new/fake-abc123.png"))
        );
        assert!(fixture.root().join("dark/new/fake-abc123.png").is_file());
    }

    #[test]
    fn an_image_already_in_the_library_is_not_downloaded_again() {
        let mut fixture = Fixture::new();
        let provider = FakeProvider::with(&[("abc123", png(20, 10, 1))]);
        fixture.download(&provider, "abc123", "").unwrap();
        // Renamed, it is still found through its metadata.
        fs::create_dir(fixture.root().join("dark")).unwrap();
        fs::rename(
            fixture.root().join("fake-abc123.png"),
            fixture.root().join("dark/mountain.png"),
        )
        .unwrap();
        fixture.library.scan().unwrap();

        let downloaded = fixture.download(&provider, "abc123", "").unwrap();

        assert_eq!(
            downloaded,
            Downloaded::AlreadyInLibrary(PathBuf::from("dark/mountain.png"))
        );
    }

    #[test]
    fn an_image_named_by_the_convention_is_already_in_the_library() {
        let mut fixture = Fixture::new();
        fs::write(fixture.root().join("fake-abc123.png"), png(20, 10, 1)).unwrap();
        fixture.library.scan().unwrap();
        let provider = FakeProvider::with(&[("abc123", png(20, 10, 2))]);

        let downloaded = fixture.download(&provider, "abc123", "other").unwrap();

        assert_eq!(
            downloaded,
            Downloaded::AlreadyInLibrary(PathBuf::from("fake-abc123.png"))
        );
        assert_eq!(*provider.detail_requests.lock().unwrap(), 0);
    }

    #[test]
    fn an_existing_file_is_never_overwritten() {
        let mut fixture = Fixture::new();
        // Not an image, so the library doesn't know it as one.
        fs::write(fixture.root().join("fake-abc123.png"), "notes").unwrap();
        fixture.library.scan().unwrap();
        let provider = FakeProvider::with(&[("abc123", png(20, 10, 1))]);

        let result = fixture.download(&provider, "abc123", "");

        assert!(matches!(result, Err(LibraryError::FileExists { .. })));
        assert_eq!(
            fs::read_to_string(fixture.root().join("fake-abc123.png")).unwrap(),
            "notes"
        );
        assert_eq!(fixture.files(), ["fake-abc123.png"]);
    }

    #[test]
    fn a_failed_download_leaves_nothing_behind() {
        let mut fixture = Fixture::new();
        let mut provider = FakeProvider::with(&[("abc123", png(20, 10, 1))]);
        provider.failing.push("abc123".to_owned());

        let result = fixture.download(&provider, "abc123", "");

        assert!(matches!(result, Err(LibraryError::Provider(_))));
        assert!(fixture.files().is_empty(), "{:?}", fixture.files());
    }

    #[test]
    fn a_download_that_is_not_an_image_is_discarded() {
        let mut fixture = Fixture::new();
        let provider = FakeProvider::with(&[("abc123", b"<html>Not found</html>".to_vec())]);

        let result = fixture.download(&provider, "abc123", "");

        assert!(matches!(result, Err(LibraryError::NotAnImage { .. })));
        assert!(fixture.files().is_empty(), "{:?}", fixture.files());
    }

    #[test]
    fn downloads_stay_in_plain_library_folders() {
        let mut fixture = Fixture::new();
        let provider = FakeProvider::with(&[("abc123", png(20, 10, 1))]);
        let outside = fixture.dir.path().join("outside");
        fs::create_dir(&outside).unwrap();
        std::os::unix::fs::symlink(&outside, fixture.root().join("link")).unwrap();

        for folder in ["../outside", "/tmp", ".backdrop", "dark/.hidden", "link"] {
            let result = fixture.download(&provider, "abc123", folder);
            assert!(
                matches!(result, Err(LibraryError::InvalidDownloadFolder { .. })),
                "{folder}: {result:?}"
            );
        }
        assert_eq!(fs::read_dir(&outside).unwrap().count(), 0);
    }
}
