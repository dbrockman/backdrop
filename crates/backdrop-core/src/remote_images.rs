//! Thumbnails and previews of provider wallpapers, cached in the XDG cache
//! directory.
//!
//! Browsing a provider shows many thumbnails, and the same ones again on the
//! next visit, so each is downloaded once and kept as a file named after its
//! provider and wallpaper id. Previews are the full original images shown in
//! the detail view; they are large, so only the most recent few are kept. A
//! file only appears once its download is complete and readable as an image.

use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

use crate::discovery;
use crate::providers::{Progress, Provider, ProviderError, RemoteWallpaper};

#[derive(Debug, thiserror::Error)]
pub enum RemoteImageError {
    #[error(transparent)]
    Provider(#[from] ProviderError),
    #[error("could not save {}: {cause}", path.display())]
    Write { path: PathBuf, cause: io::Error },
    #[error("the image of {wallpaper} is not an image: {message}")]
    NotAnImage { wallpaper: String, message: String },
    #[error("{wallpaper} can't be cached: its id isn't a plain name")]
    InvalidId { wallpaper: String },
    #[error("the download was cancelled")]
    Cancelled,
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
    ) -> Result<PathBuf, RemoteImageError> {
        let file = cache_file(&self.dir, wallpaper, "thumbnail")?;
        if !file.is_file() {
            fetch_into(&file, wallpaper, |to| {
                provider.download_thumbnail(wallpaper, to)
            })?;
        }
        Ok(file)
    }
}

/// Full original images, for looking at a wallpaper before downloading it.
#[derive(Clone, Debug)]
pub struct RemotePreviews {
    dir: PathBuf,
    /// How many previews to keep; older ones are deleted.
    keep: usize,
}

impl RemotePreviews {
    pub fn new(dir: impl Into<PathBuf>, keep: usize) -> Self {
        Self {
            dir: dir.into(),
            keep,
        }
    }

    /// The cached original of `wallpaper`, downloaded through `provider` if it
    /// isn't cached yet, reporting progress. Setting `cancelled` stops the
    /// download, which then fails with [`RemoteImageError::Cancelled`].
    pub fn ensure(
        &self,
        provider: &dyn Provider,
        wallpaper: &RemoteWallpaper,
        progress: Progress,
        cancelled: &AtomicBool,
    ) -> Result<PathBuf, RemoteImageError> {
        let file = cache_file(&self.dir, wallpaper, "preview")?;
        if file.is_file() {
            return Ok(file);
        }
        let result = fetch_into(&file, wallpaper, |to| {
            let mut to = Cancellable { to, cancelled };
            provider.download(wallpaper, &mut to, progress)
        });
        if cancelled.load(Ordering::Relaxed) {
            return Err(RemoteImageError::Cancelled);
        }
        result?;
        self.prune(&file);
        Ok(file)
    }

    /// Deletes all but the newest `keep` previews, never `just_saved`.
    fn prune(&self, just_saved: &Path) {
        let Ok(providers) = fs::read_dir(&self.dir) else {
            return;
        };
        let mut previews: Vec<(std::time::SystemTime, PathBuf)> = providers
            .flatten()
            .filter_map(|provider| fs::read_dir(provider.path()).ok())
            .flatten()
            .flatten()
            .filter(|entry| entry.path().extension().is_some_and(|ext| ext == "preview"))
            .filter_map(|entry| Some((entry.metadata().ok()?.modified().ok()?, entry.path())))
            .collect();
        previews.sort_by_key(|(modified, _)| std::cmp::Reverse(*modified));
        for (_, path) in previews.into_iter().skip(self.keep) {
            if path != just_saved {
                let _ = fs::remove_file(path);
            }
        }
    }
}

/// Fails writes once `cancelled` is set, which ends a download in progress.
struct Cancellable<'a> {
    to: &'a mut dyn Write,
    cancelled: &'a AtomicBool,
}

impl Write for Cancellable<'_> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        if self.cancelled.load(Ordering::Relaxed) {
            // Not `Interrupted`, which `write_all` retries forever.
            return Err(io::Error::other("cancelled"));
        }
        self.to.write(buf)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.to.flush()
    }
}

/// Where the image of `wallpaper` with this `kind` is cached in `dir`.
fn cache_file(
    dir: &Path,
    wallpaper: &RemoteWallpaper,
    kind: &str,
) -> Result<PathBuf, RemoteImageError> {
    let plain = |part: &str| {
        !part.is_empty()
            && part
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
    };
    if !plain(wallpaper.provider()) || !plain(wallpaper.id()) {
        return Err(RemoteImageError::InvalidId {
            wallpaper: wallpaper.source(),
        });
    }
    Ok(dir
        .join(wallpaper.provider())
        .join(format!("{}.{kind}", wallpaper.id())))
}

/// Writes what `fetch` downloads to a temporary file beside `file`, and gives
/// it the name `file` only if it is a complete, readable image.
fn fetch_into(
    file: &Path,
    wallpaper: &RemoteWallpaper,
    fetch: impl FnOnce(&mut dyn Write) -> Result<u64, ProviderError>,
) -> Result<(), RemoteImageError> {
    let dir = file
        .parent()
        .expect("cached images are inside a provider folder");
    let write_error = |cause| RemoteImageError::Write {
        path: file.to_path_buf(),
        cause,
    };
    fs::create_dir_all(dir).map_err(write_error)?;
    let mut temp = tempfile::NamedTempFile::new_in(dir).map_err(write_error)?;
    {
        let mut writer = io::BufWriter::new(temp.as_file_mut());
        fetch(&mut writer)?;
        writer.flush().map_err(write_error)?;
    }
    discovery::probe(temp.path()).map_err(|error| RemoteImageError::NotAnImage {
        wallpaper: wallpaper.source(),
        message: error.to_string(),
    })?;
    // Another process may have cached it meanwhile; either copy will do.
    temp.persist(file)
        .map_err(|error| write_error(error.error))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;
    use std::sync::atomic::AtomicBool;

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
            to: &mut dyn io::Write,
            progress: Progress,
        ) -> Result<u64, ProviderError> {
            *self.requests.lock().unwrap() += 1;
            let total = self.thumbnail.len() as u64;
            to.write_all(&self.thumbnail)
                .map_err(ProviderError::Write)?;
            progress(total, Some(total));
            Ok(total)
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

        assert!(matches!(result, Err(RemoteImageError::NotAnImage { .. })));
        let files = std::fs::read_dir(cache.path().join("fake"))
            .unwrap()
            .count();
        assert_eq!(files, 0);
    }

    #[test]
    fn previews_are_downloaded_once_and_only_the_newest_kept() {
        let cache = tempfile::tempdir().unwrap();
        let previews = RemotePreviews::new(cache.path(), 2);
        let provider = provider(png(30, 20, 1));
        let not_cancelled = AtomicBool::new(false);
        let ensure = |id: &str| {
            previews
                .ensure(
                    &provider,
                    &RemoteWallpaper::for_test("fake", id),
                    &mut |_, _| {},
                    &not_cancelled,
                )
                .unwrap()
        };

        let first = ensure("aaa111");
        assert_eq!(ensure("aaa111"), first);
        assert_eq!(*provider.requests.lock().unwrap(), 1);
        // Make the first the oldest, whatever the clock's resolution.
        let old = std::time::SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1);
        std::fs::File::options()
            .write(true)
            .open(&first)
            .unwrap()
            .set_modified(old)
            .unwrap();
        let second = ensure("bbb222");
        let third = ensure("ccc333");

        assert!(!first.exists(), "the oldest preview is deleted");
        assert!(second.exists() && third.exists());
    }

    #[test]
    fn a_cancelled_preview_leaves_nothing_behind() {
        let cache = tempfile::tempdir().unwrap();
        let previews = RemotePreviews::new(cache.path(), 2);
        let cancelled = AtomicBool::new(true);

        let result = previews.ensure(
            &provider(png(30, 20, 1)),
            &RemoteWallpaper::for_test("fake", "aaa111"),
            &mut |_, _| {},
            &cancelled,
        );

        assert!(matches!(result, Err(RemoteImageError::Cancelled)));
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

        assert!(matches!(result, Err(RemoteImageError::InvalidId { .. })));
    }
}
