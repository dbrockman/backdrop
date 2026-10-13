//! Thumbnails for the library grid, cached as JPEG files in the XDG cache
//! directory.
//!
//! A thumbnail is named after the image's absolute path, size, and
//! modification time, so a changed file gets a new thumbnail without reading
//! its contents. Stale thumbnails are left behind until the cache is cleaned.

use std::fs::File;
use std::io::{self, BufReader};
use std::os::unix::ffi::OsStrExt as _;
use std::path::{Path, PathBuf};

use image::codecs::jpeg::JpegEncoder;
use image::{DynamicImage, ImageDecoder as _, ImageReader};

use crate::fs_util;
use crate::image::ImageFile;

/// Thumbnails are scaled to cover this size, so a tile of up to half these
/// dimensions stays sharp on a display with a scale factor of 2.
pub const COVER_WIDTH: u32 = 512;
pub const COVER_HEIGHT: u32 = 320;
const JPEG_QUALITY: u8 = 85;

#[derive(Debug, thiserror::Error)]
pub enum ThumbnailError {
    #[error("could not read {}: {cause}", path.display())]
    Read { path: PathBuf, cause: io::Error },
    #[error("could not decode {}: {cause}", path.display())]
    Decode {
        path: PathBuf,
        cause: image::ImageError,
    },
    #[error("could not write {}: {cause}", path.display())]
    Write { path: PathBuf, cause: io::Error },
}

/// The thumbnail cache directory.
#[derive(Clone, Debug)]
pub struct Thumbnails {
    dir: PathBuf,
}

impl Thumbnails {
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        Self { dir: dir.into() }
    }

    /// Where the thumbnail of `image` in the library at `root` is stored.
    pub fn path(&self, root: &Path, image: &ImageFile) -> PathBuf {
        let mut hasher = blake3::Hasher::new();
        hasher.update(root.join(image.path()).as_os_str().as_bytes());
        hasher.update(&image.size().to_le_bytes());
        hasher.update(&image.modified().to_le_bytes());
        let key = hasher.finalize().to_hex();
        // Two-character subfolders keep any one folder small.
        self.dir.join(&key[..2]).join(format!("{}.jpg", &key[2..]))
    }

    /// Returns the thumbnail of `image`, creating it if it doesn't exist yet.
    pub fn ensure(&self, root: &Path, image: &ImageFile) -> Result<PathBuf, ThumbnailError> {
        let thumbnail = self.path(root, image);
        if !thumbnail.exists() {
            create(&root.join(image.path()), &thumbnail)?;
        }
        Ok(thumbnail)
    }
}

/// Decodes `source`, upright according to its EXIF orientation, scales it to
/// cover the thumbnail size, and saves it as a JPEG at `target`.
fn create(source: &Path, target: &Path) -> Result<(), ThumbnailError> {
    let decode_error = |cause| ThumbnailError::Decode {
        path: source.to_path_buf(),
        cause,
    };
    let file = File::open(source).map_err(|cause| ThumbnailError::Read {
        path: source.to_path_buf(),
        cause,
    })?;
    let mut decoder = ImageReader::new(BufReader::new(file))
        .with_guessed_format()
        .map_err(|cause| ThumbnailError::Read {
            path: source.to_path_buf(),
            cause,
        })?
        .into_decoder()
        .map_err(decode_error)?;
    let orientation = decoder.orientation().map_err(decode_error)?;
    let mut image = DynamicImage::from_decoder(decoder).map_err(decode_error)?;
    image.apply_orientation(orientation);

    let (width, height) = cover_size(image.width(), image.height());
    let thumbnail = if (width, height) == (image.width(), image.height()) {
        image
    } else {
        image.thumbnail_exact(width, height)
    };

    let mut bytes = Vec::new();
    thumbnail
        .to_rgb8()
        .write_with_encoder(JpegEncoder::new_with_quality(&mut bytes, JPEG_QUALITY))
        .map_err(decode_error)?;
    fs_util::write_atomically(target, &bytes).map_err(|cause| ThumbnailError::Write {
        path: target.to_path_buf(),
        cause,
    })
}

/// The smallest size with the image's aspect ratio that covers
/// `COVER_WIDTH` × `COVER_HEIGHT`. Images are never enlarged.
fn cover_size(width: u32, height: u32) -> (u32, u32) {
    let scale = (COVER_WIDTH as f64 / width as f64)
        .max(COVER_HEIGHT as f64 / height as f64)
        .min(1.0);
    let scaled = |n: u32| ((n as f64 * scale).round() as u32).max(1);
    (scaled(width), scaled(height))
}

#[cfg(test)]
mod tests {
    use std::fs;

    use image::{ImageBuffer, Rgb};

    use super::*;
    use crate::index::{ImageOrder, Index};

    #[test]
    fn cover_size_fills_the_tile_without_enlarging() {
        assert_eq!(cover_size(3840, 2160), (569, 320));
        assert_eq!(cover_size(1080, 1920), (512, 910));
        assert_eq!(cover_size(2560, 1600), (512, 320));
        assert_eq!(cover_size(300, 200), (300, 200));
    }

    #[test]
    fn creates_a_cached_thumbnail_once() {
        let library = tempfile::tempdir().unwrap();
        let cache = tempfile::tempdir().unwrap();
        ImageBuffer::from_pixel(1920, 1080, Rgb([200u8, 40, 40]))
            .save(library.path().join("red.png"))
            .unwrap();
        let mut index = Index::in_memory().unwrap();
        index.scan(library.path()).unwrap();
        let image = index.images(ImageOrder::Path).unwrap().remove(0);
        let thumbnails = Thumbnails::new(cache.path());

        let path = thumbnails.ensure(library.path(), &image).unwrap();
        let modified = fs::metadata(&path).unwrap().modified().unwrap();
        let again = thumbnails.ensure(library.path(), &image).unwrap();

        assert_eq!(path, again);
        assert_eq!(fs::metadata(&again).unwrap().modified().unwrap(), modified);
        let thumbnail = image::open(&path).unwrap();
        assert_eq!((thumbnail.width(), thumbnail.height()), (569, 320));
    }

    #[test]
    fn a_changed_image_gets_a_new_thumbnail() {
        let library = tempfile::tempdir().unwrap();
        let file = library.path().join("a.png");
        ImageBuffer::from_pixel(64, 64, Rgb([0u8, 0, 0]))
            .save(&file)
            .unwrap();
        let mut index = Index::in_memory().unwrap();
        index.scan(library.path()).unwrap();
        let before = index.images(ImageOrder::Path).unwrap().remove(0);

        ImageBuffer::from_pixel(32, 32, Rgb([0u8, 0, 0]))
            .save(&file)
            .unwrap();
        index.scan(library.path()).unwrap();
        let after = index.images(ImageOrder::Path).unwrap().remove(0);
        let thumbnails = Thumbnails::new("/cache");

        assert_ne!(
            thumbnails.path(library.path(), &before),
            thumbnails.path(library.path(), &after)
        );
    }

    #[test]
    fn undecodable_images_are_an_error() {
        let library = tempfile::tempdir().unwrap();
        let cache = tempfile::tempdir().unwrap();
        // A valid PNG header with no image data.
        let mut header = b"\x89PNG\r\n\x1a\n\0\0\0\x0dIHDR".to_vec();
        header.extend(16u32.to_be_bytes());
        header.extend(16u32.to_be_bytes());
        header.extend([8, 6, 0, 0, 0, 0, 0, 0, 0]);
        fs::write(library.path().join("broken.png"), header).unwrap();
        let mut index = Index::in_memory().unwrap();
        index.scan(library.path()).unwrap();
        let image = index.images(ImageOrder::Path).unwrap().remove(0);
        let thumbnails = Thumbnails::new(cache.path());

        assert!(matches!(
            thumbnails.ensure(library.path(), &image),
            Err(ThumbnailError::Decode { .. })
        ));
        assert!(!thumbnails.path(library.path(), &image).exists());
    }
}
