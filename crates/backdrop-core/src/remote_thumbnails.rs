//! Thumbnails of provider wallpapers, cached in the XDG cache directory.
//!
//! Browsing a provider shows many thumbnails, and the same ones again on the
//! next visit, so each is downloaded once and kept as a file named after its
//! provider and wallpaper id. A file only appears once its download is
//! complete and readable as an image.

use std::io::{self, Write as _};
use std::path::PathBuf;

use crate::discovery;
use crate::providers::{Provider, ProviderError, RemoteWallpaper};

#[derive(Debug, thiserror::Error)]
pub enum RemoteThumbnailError {
    #[error(transparent)]
    Provider(#[from] ProviderError),
    #[error("could not save the thumbnail to {}: {cause}", path.display())]
    Write { path: PathBuf, cause: io::Error },
    #[error("the thumbnail of {wallpaper} is not an image: {message}")]
    NotAnImage { wallpaper: String, message: String },
    #[error("{wallpaper} can't be cached: its id isn't a plain name")]
    InvalidId { wallpaper: String },
}

#[derive(Clone, Debug)]
pub struct RemoteThumbnails {
    dir: PathBuf,
}

impl RemoteThumbnails {
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        Self { dir: dir.into() }
    }

    /// The cached thumbnail of `wallpaper`, downloaded through `provider` if
    /// it isn't cached yet.
    pub fn ensure(
        &self,
        provider: &dyn Provider,
        wallpaper: &RemoteWallpaper,
    ) -> Result<PathBuf, RemoteThumbnailError> {
        let file = self.file(wallpaper)?;
        if file.is_file() {
            return Ok(file);
        }
        let dir = file
            .parent()
            .expect("thumbnails are inside a provider folder");
        let write_error = |cause| RemoteThumbnailError::Write {
            path: file.clone(),
            cause,
        };
        std::fs::create_dir_all(dir).map_err(write_error)?;
        let mut temp = tempfile::NamedTempFile::new_in(dir).map_err(write_error)?;
        provider.download_thumbnail(wallpaper, temp.as_file_mut())?;
        temp.as_file_mut().flush().map_err(write_error)?;
        discovery::probe(temp.path()).map_err(|error| RemoteThumbnailError::NotAnImage {
            wallpaper: wallpaper.source(),
            message: error.to_string(),
        })?;
        // Another process may have cached it meanwhile; either copy will do.
        temp.persist(&file)
            .map_err(|error| write_error(error.error))?;
        Ok(file)
    }

    fn file(&self, wallpaper: &RemoteWallpaper) -> Result<PathBuf, RemoteThumbnailError> {
        let plain = |part: &str| {
            !part.is_empty()
                && part
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
        };
        if !plain(wallpaper.provider()) || !plain(wallpaper.id()) {
            return Err(RemoteThumbnailError::InvalidId {
                wallpaper: wallpaper.source(),
            });
        }
        Ok(self
            .dir
            .join(wallpaper.provider())
            .join(format!("{}.thumbnail", wallpaper.id())))
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use super::*;
    use crate::providers::{Category, Progress, SearchPage, SearchQuery, SearchSort};
    use crate::test_images::png;

    /// Serves `thumbnail` for every wallpaper and counts the requests.
    struct ThumbnailProvider {
        thumbnail: Vec<u8>,
        requests: Mutex<usize>,
    }

    impl Provider for ThumbnailProvider {
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
        fn wallpaper(&self, _: &str) -> Result<RemoteWallpaper, ProviderError> {
            unimplemented!()
        }
        fn download(
            &self,
            _: &RemoteWallpaper,
            _: &mut dyn io::Write,
            _: Progress,
        ) -> Result<u64, ProviderError> {
            unimplemented!()
        }
        fn download_thumbnail(
            &self,
            _: &RemoteWallpaper,
            to: &mut dyn io::Write,
        ) -> Result<u64, ProviderError> {
            *self.requests.lock().unwrap() += 1;
            to.write_all(&self.thumbnail)
                .map_err(ProviderError::Write)?;
            Ok(self.thumbnail.len() as u64)
        }
        fn file_name(&self, id: &str, extension: &str) -> String {
            format!("{id}.{extension}")
        }
        fn id_from_file_name(&self, _: &str) -> Option<String> {
            None
        }
        fn parse_id(&self, _: &str) -> Option<String> {
            None
        }
    }

    fn provider(thumbnail: Vec<u8>) -> ThumbnailProvider {
        ThumbnailProvider {
            thumbnail,
            requests: Mutex::new(0),
        }
    }

    #[test]
    fn thumbnails_are_downloaded_once() {
        let cache = tempfile::tempdir().unwrap();
        let thumbnails = RemoteThumbnails::new(cache.path());
        let provider = provider(png(30, 20, 1));
        let wallpaper = RemoteWallpaper::for_test("fake", "abc123");

        let first = thumbnails.ensure(&provider, &wallpaper).unwrap();
        let second = thumbnails.ensure(&provider, &wallpaper).unwrap();

        assert_eq!(first, cache.path().join("fake/abc123.thumbnail"));
        assert_eq!(first, second);
        assert_eq!(std::fs::read(&first).unwrap(), png(30, 20, 1));
        assert_eq!(*provider.requests.lock().unwrap(), 1);
    }

    #[test]
    fn a_thumbnail_that_is_not_an_image_is_not_cached() {
        let cache = tempfile::tempdir().unwrap();
        let thumbnails = RemoteThumbnails::new(cache.path());
        let provider = provider(b"<html>error</html>".to_vec());

        let result = thumbnails.ensure(&provider, &RemoteWallpaper::for_test("fake", "abc123"));

        assert!(matches!(
            result,
            Err(RemoteThumbnailError::NotAnImage { .. })
        ));
        let files = std::fs::read_dir(cache.path().join("fake"))
            .unwrap()
            .count();
        assert_eq!(files, 0);
    }

    #[test]
    fn ids_that_are_not_plain_names_are_refused() {
        let cache = tempfile::tempdir().unwrap();
        let thumbnails = RemoteThumbnails::new(cache.path());

        let result = thumbnails.ensure(
            &provider(png(30, 20, 1)),
            &RemoteWallpaper::for_test("fake", "../x"),
        );

        assert!(matches!(
            result,
            Err(RemoteThumbnailError::InvalidId { .. })
        ));
    }
}
