//! Ordering and filtering of library images, shared by the GUI and the CLI.

use std::cmp::Ordering;
use std::collections::HashSet;
use std::path::PathBuf;

use crate::image::{ImageFile, Orientation};

/// How images are ordered.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ImageOrder {
    /// Most recently added first.
    #[default]
    Added,
    /// Most recently modified first.
    Modified,
    /// By file name, A to Z, ignoring case.
    Name,
    /// By path relative to the library, A to Z.
    Path,
    /// Most pixels first.
    Resolution,
}

impl ImageOrder {
    /// Sorts `images`. Ties are broken by path, so the order is stable across
    /// rescans.
    pub fn sort(self, images: &mut [ImageFile]) {
        images.sort_by(|a, b| self.compare(a, b).then_with(|| a.path().cmp(b.path())));
    }

    fn compare(self, a: &ImageFile, b: &ImageFile) -> Ordering {
        match self {
            Self::Added => b.added().cmp(&a.added()),
            Self::Modified => b.modified().cmp(&a.modified()),
            Self::Name => name_key(a).cmp(&name_key(b)),
            Self::Path => Ordering::Equal,
            Self::Resolution => pixels(b).cmp(&pixels(a)),
        }
    }
}

fn name_key(image: &ImageFile) -> String {
    image
        .path()
        .file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .to_lowercase()
}

fn pixels(image: &ImageFile) -> u64 {
    u64::from(image.width()) * u64::from(image.height())
}

/// Which images to show. The default shows all of them.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ImageFilter {
    orientation: Option<Orientation>,
    favorites_only: bool,
}

impl ImageFilter {
    /// Only images with this orientation, or any orientation with `None`.
    pub fn with_orientation(mut self, orientation: Option<Orientation>) -> Self {
        self.orientation = orientation;
        self
    }

    pub fn with_favorites_only(mut self, favorites_only: bool) -> Self {
        self.favorites_only = favorites_only;
        self
    }

    pub fn orientation(&self) -> Option<Orientation> {
        self.orientation
    }

    pub fn is_favorites_only(&self) -> bool {
        self.favorites_only
    }

    /// Whether the filter shows every image.
    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }

    pub fn matches(&self, image: &ImageFile, favorites: &HashSet<PathBuf>) -> bool {
        self.orientation
            .is_none_or(|orientation| image.orientation() == orientation)
            && (!self.favorites_only || favorites.contains(image.path()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::image::ImageFormat;

    fn image(path: &str, added: i64, width: u32, height: u32) -> ImageFile {
        ImageFile::new(
            path.into(),
            1,
            added,
            added,
            width,
            height,
            ImageFormat::Png,
        )
    }

    fn paths(images: &[ImageFile]) -> Vec<String> {
        images
            .iter()
            .map(|image| image.path().display().to_string())
            .collect()
    }

    #[test]
    fn orders_sort_with_path_as_the_tie_breaker() {
        let mut images = vec![
            image("dark/b.png", 2, 100, 100),
            image("A.png", 1, 300, 200),
            image("c.png", 2, 10, 10),
        ];

        ImageOrder::Added.sort(&mut images);
        assert_eq!(paths(&images), ["c.png", "dark/b.png", "A.png"]);
        ImageOrder::Name.sort(&mut images);
        assert_eq!(paths(&images), ["A.png", "dark/b.png", "c.png"]);
        ImageOrder::Path.sort(&mut images);
        assert_eq!(paths(&images), ["A.png", "c.png", "dark/b.png"]);
        ImageOrder::Resolution.sort(&mut images);
        assert_eq!(paths(&images), ["A.png", "dark/b.png", "c.png"]);
    }

    #[test]
    fn filters_by_orientation_and_favorites() {
        let images = [
            image("wide.png", 0, 1920, 1080),
            image("tall.png", 0, 1080, 1920),
            image("square.png", 0, 500, 500),
        ];
        let favorites = HashSet::from([PathBuf::from("tall.png"), PathBuf::from("square.png")]);
        let shown = |filter: ImageFilter| -> Vec<String> {
            paths(
                &images
                    .iter()
                    .filter(|image| filter.matches(image, &favorites))
                    .cloned()
                    .collect::<Vec<_>>(),
            )
        };

        assert_eq!(shown(ImageFilter::default()).len(), 3);
        assert_eq!(
            shown(ImageFilter::default().with_orientation(Some(Orientation::Portrait))),
            ["tall.png"]
        );
        assert_eq!(
            shown(ImageFilter::default().with_favorites_only(true)),
            ["tall.png", "square.png"]
        );
        assert_eq!(
            shown(
                ImageFilter::default()
                    .with_orientation(Some(Orientation::Landscape))
                    .with_favorites_only(true)
            ),
            Vec::<String>::new()
        );
    }
}
