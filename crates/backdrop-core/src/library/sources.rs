//! Which provider wallpaper each image came from (spec §3.4).
//!
//! An image's source is recorded in the library metadata when Backdrop
//! downloads it. Images saved some other way can often still be recognized by
//! the provider's file name convention, such as `wallhaven-94x38z.jpg`.
//! Recognizing reads nothing but the name and writes nothing. Linking makes it
//! permanent: it fetches the wallpaper's details, saves its snapshot, and
//! records the source, so it survives renames and keeps the provider's tags.

use std::collections::BTreeMap;
use std::ffi::OsStr;
use std::path::{Path, PathBuf};

use super::{Library, LibraryError};
use crate::providers::{Provider, RemoteWallpaper};
use crate::snapshot::Snapshot;

/// Where an image came from, as `provider/id`, and how Backdrop knows.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ImageSource {
    /// Recorded in the library metadata.
    Recorded(String),
    /// Recognized from the file name, but not linked yet.
    FileName(String),
}

impl ImageSource {
    /// `provider/id`.
    pub fn source(&self) -> &str {
        match self {
            Self::Recorded(source) | Self::FileName(source) => source,
        }
    }
}

impl Library {
    /// The source of each indexed image that has one: the recorded source, or
    /// else one recognized from the file name by `provider`. Uses the index as
    /// of the last scan.
    pub fn image_sources(
        &self,
        provider: &dyn Provider,
    ) -> Result<BTreeMap<PathBuf, ImageSource>, LibraryError> {
        let metadata = self.metadata()?;
        let mut sources = BTreeMap::new();
        for path in self.indexed_sizes()?.into_keys() {
            let recorded = metadata.get(&path).and_then(|record| record.source());
            let source = match recorded {
                Some(source) => ImageSource::Recorded(source.to_owned()),
                None => match id_from_path(provider, &path) {
                    Some(id) => ImageSource::FileName(format!("{}/{id}", provider.id())),
                    None => continue,
                },
            };
            sources.insert(path, source);
        }
        Ok(sources)
    }

    /// The source of the image at `path`: recorded, or recognized from its file
    /// name by `provider`. Reads only the metadata file, not the index.
    pub fn image_source(
        &self,
        provider: &dyn Provider,
        path: &Path,
    ) -> Result<Option<ImageSource>, LibraryError> {
        if let Some(source) = self
            .metadata()?
            .get(path)
            .and_then(|record| record.source())
        {
            return Ok(Some(ImageSource::Recorded(source.to_owned())));
        }
        Ok(id_from_path(provider, path)
            .map(|id| ImageSource::FileName(format!("{}/{id}", provider.id()))))
    }

    /// Records `wallpaper` as the source of the indexed image at `path`, and
    /// saves its snapshot unless the library has one. The image must have the
    /// wallpaper's dimensions, so a misnamed file isn't given another
    /// wallpaper's tags. An image already linked to another source is left
    /// alone.
    pub fn link_source(
        &mut self,
        path: &Path,
        wallpaper: &RemoteWallpaper,
    ) -> Result<(), LibraryError> {
        let images = self.index.images(crate::query::ImageOrder::Path)?;
        let Some(image) = images.iter().find(|image| image.path() == path) else {
            return Err(LibraryError::NotInLibrary {
                path: path.to_path_buf(),
            });
        };
        let source = wallpaper.source();
        let remote = (wallpaper.width(), wallpaper.height());
        // The index stores dimensions upright; a rotated copy is the same image.
        let same_size =
            remote == (image.width(), image.height()) || remote == (image.height(), image.width());
        if !same_size {
            return Err(LibraryError::DifferentImage {
                path: path.to_path_buf(),
                source_name: source,
            });
        }
        if let Some(linked) = self
            .metadata()?
            .get(path)
            .and_then(|record| record.source())
            && linked != source
        {
            return Err(LibraryError::AlreadyLinked {
                path: path.to_path_buf(),
                source_name: linked.to_owned(),
            });
        }
        // The snapshot first, so the record never points at nothing.
        Snapshot::new(wallpaper, chrono::Utc::now()).save_if_missing(&self.root)?;
        self.update_image(path, |record| record.set_source(Some(source)))
    }
}

/// The wallpaper id in the file name of `path`, by `provider`'s convention.
pub(super) fn id_from_path(provider: &dyn Provider, path: &Path) -> Option<String> {
    let name = path.file_name().and_then(OsStr::to_str)?;
    provider.id_from_file_name(name)
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;
    use crate::index::Index;
    use crate::providers::{Tag, Wallhaven};
    use crate::test_images::png;

    struct Fixture {
        _dir: tempfile::TempDir,
        library: Library,
    }

    impl Fixture {
        /// A library with the given 20 × 10 images, scanned.
        fn with(names: &[&str]) -> Self {
            let dir = tempfile::tempdir().unwrap();
            for (seed, name) in names.iter().enumerate() {
                let path = dir.path().join(name);
                fs::create_dir_all(path.parent().unwrap()).unwrap();
                fs::write(path, png(20, 10, seed as u8)).unwrap();
            }
            let mut library = Library::with_index(dir.path(), Index::in_memory().unwrap());
            library.scan().unwrap();
            Self { _dir: dir, library }
        }
    }

    fn wallpaper(id: &str, width: u32, height: u32) -> RemoteWallpaper {
        let mut wallpaper = RemoteWallpaper::for_test("wallhaven", id);
        wallpaper.width = width;
        wallpaper.height = height;
        wallpaper.tags = Some(vec![Tag {
            name: "mountains".to_owned(),
            category: None,
        }]);
        wallpaper
    }

    #[test]
    fn sources_are_recognized_from_file_names_without_writing() {
        let fixture = Fixture::with(&[
            "wallhaven-94x38z.png",
            "dark/wallhaven-lygg6r.png",
            "mountain.png",
        ]);

        let sources = fixture
            .library
            .image_sources(&Wallhaven::new(None))
            .unwrap();

        assert_eq!(
            sources.into_iter().collect::<Vec<_>>(),
            [
                (
                    PathBuf::from("dark/wallhaven-lygg6r.png"),
                    ImageSource::FileName("wallhaven/lygg6r".to_owned())
                ),
                (
                    PathBuf::from("wallhaven-94x38z.png"),
                    ImageSource::FileName("wallhaven/94x38z".to_owned())
                ),
            ]
        );
        assert!(!fixture.library.root().join(".backdrop").exists());
    }

    #[test]
    fn linking_records_the_source_and_saves_a_snapshot() {
        let mut fixture = Fixture::with(&["wallhaven-94x38z.png"]);
        let path = Path::new("wallhaven-94x38z.png");

        fixture
            .library
            .link_source(path, &wallpaper("94x38z", 20, 10))
            .unwrap();

        let metadata = fixture.library.metadata().unwrap();
        assert_eq!(
            metadata.get(path).unwrap().source(),
            Some("wallhaven/94x38z")
        );
        let snapshot = Snapshot::load(fixture.library.root(), "wallhaven/94x38z")
            .unwrap()
            .unwrap();
        assert_eq!(snapshot.tags()[0].name, "mountains");
        let sources = fixture
            .library
            .image_sources(&Wallhaven::new(None))
            .unwrap();
        assert_eq!(
            sources[path],
            ImageSource::Recorded("wallhaven/94x38z".to_owned())
        );
    }

    #[test]
    fn a_linked_image_keeps_its_source_when_renamed() {
        let mut fixture = Fixture::with(&["wallhaven-94x38z.png"]);
        fixture
            .library
            .link_source(
                Path::new("wallhaven-94x38z.png"),
                &wallpaper("94x38z", 20, 10),
            )
            .unwrap();
        let root = fixture.library.root().to_path_buf();

        fs::rename(root.join("wallhaven-94x38z.png"), root.join("lake.png")).unwrap();
        fixture.library.scan().unwrap();

        let sources = fixture
            .library
            .image_sources(&Wallhaven::new(None))
            .unwrap();
        assert_eq!(
            sources[Path::new("lake.png")],
            ImageSource::Recorded("wallhaven/94x38z".to_owned())
        );
    }

    #[test]
    fn a_wallpaper_of_other_dimensions_is_not_linked() {
        let mut fixture = Fixture::with(&["wallhaven-94x38z.png"]);
        let path = Path::new("wallhaven-94x38z.png");

        let result = fixture
            .library
            .link_source(path, &wallpaper("94x38z", 3840, 2160));

        assert!(matches!(result, Err(LibraryError::DifferentImage { .. })));
        assert!(!fixture.library.root().join(".backdrop").exists());
        // Rotated a quarter turn, it is the same image.
        fixture
            .library
            .link_source(path, &wallpaper("94x38z", 10, 20))
            .unwrap();
    }

    #[test]
    fn an_image_linked_elsewhere_is_left_alone() {
        let mut fixture = Fixture::with(&["wallhaven-94x38z.png"]);
        let path = Path::new("wallhaven-94x38z.png");
        fixture
            .library
            .link_source(path, &wallpaper("94x38z", 20, 10))
            .unwrap();

        let result = fixture
            .library
            .link_source(path, &wallpaper("lygg6r", 20, 10));

        assert!(matches!(result, Err(LibraryError::AlreadyLinked { .. })));
        // Linking again to the same source is fine.
        fixture
            .library
            .link_source(path, &wallpaper("94x38z", 20, 10))
            .unwrap();
    }

    #[test]
    fn only_indexed_images_are_linked() {
        let mut fixture = Fixture::with(&[]);

        let result = fixture.library.link_source(
            Path::new("wallhaven-94x38z.png"),
            &wallpaper("94x38z", 20, 10),
        );

        assert!(matches!(result, Err(LibraryError::NotInLibrary { .. })));
    }
}
