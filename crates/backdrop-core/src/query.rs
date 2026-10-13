//! Ordering and filtering of library images, shared by the GUI and the CLI.

use std::cmp::Ordering;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use crate::image::{ImageFile, Orientation};
use crate::metadata::same_tag;

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

/// What the user and providers recorded about the images of a library: what
/// filters look at besides the files themselves.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ImageMarks {
    favorites: HashSet<PathBuf>,
    /// The user's tags, in the order they were added.
    tags: HashMap<PathBuf, Vec<String>>,
    /// Tags from the provider snapshot of each image's source.
    provider_tags: HashMap<PathBuf, Vec<String>>,
}

impl ImageMarks {
    pub fn new(
        favorites: HashSet<PathBuf>,
        tags: HashMap<PathBuf, Vec<String>>,
        provider_tags: HashMap<PathBuf, Vec<String>>,
    ) -> Self {
        Self {
            favorites,
            tags,
            provider_tags,
        }
    }

    pub fn is_favorite(&self, path: &Path) -> bool {
        self.favorites.contains(path)
    }

    pub fn set_favorite(&mut self, path: &Path, favorite: bool) {
        if favorite {
            self.favorites.insert(path.to_path_buf());
        } else {
            self.favorites.remove(path);
        }
    }

    /// The user's tags of the image at `path`.
    pub fn tags(&self, path: &Path) -> &[String] {
        self.tags.get(path).map_or(&[], Vec::as_slice)
    }

    pub fn set_tags(&mut self, path: &Path, tags: Vec<String>) {
        if tags.is_empty() {
            self.tags.remove(path);
        } else {
            self.tags.insert(path.to_path_buf(), tags);
        }
    }

    /// The provider's tags of the image at `path`.
    pub fn provider_tags(&self, path: &Path) -> &[String] {
        self.provider_tags.get(path).map_or(&[], Vec::as_slice)
    }

    /// Whether the image at `path` has `tag`, from the user or its provider.
    pub fn has_tag(&self, path: &Path, tag: &str) -> bool {
        self.tags(path)
            .iter()
            .chain(self.provider_tags(path))
            .any(|candidate| same_tag(candidate, tag))
    }

    /// The tags of the images at `paths`, user and provider, most used first.
    /// Each image counts once per tag, whoever gave it the tag.
    pub fn tag_counts<'a>(&self, paths: impl IntoIterator<Item = &'a Path>) -> Vec<TagCount> {
        let mut counts: HashMap<String, TagCount> = HashMap::new();
        for path in paths {
            let mut seen = HashSet::new();
            for (tags, from_user) in [(self.tags(path), true), (self.provider_tags(path), false)] {
                for tag in tags {
                    let key = tag.to_lowercase();
                    let count = counts.entry(key.clone()).or_insert_with(|| TagCount {
                        name: tag.clone(),
                        images: 0,
                        user: 0,
                        provider: 0,
                    });
                    if from_user {
                        if count.user == 0 {
                            // The user's spelling wins over a provider's.
                            count.name = tag.clone();
                        }
                        count.user += 1;
                    } else {
                        count.provider += 1;
                    }
                    if seen.insert(key) {
                        count.images += 1;
                    }
                }
            }
        }
        let mut counts: Vec<TagCount> = counts.into_values().collect();
        counts.sort_by(|a, b| {
            b.images
                .cmp(&a.images)
                .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
        });
        counts
    }

    /// Forgets an image that left the library.
    pub fn remove(&mut self, path: &Path) {
        self.favorites.remove(path);
        self.tags.remove(path);
        self.provider_tags.remove(path);
    }
}

/// A tag used in the library, and how many images have it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TagCount {
    name: String,
    images: usize,
    user: usize,
    provider: usize,
}

impl TagCount {
    /// The tag as first spelled, preferring the user's spelling.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Images with this tag, from the user or a provider.
    pub fn images(&self) -> usize {
        self.images
    }

    /// Images the user gave this tag.
    pub fn user(&self) -> usize {
        self.user
    }

    /// Images whose provider gave them this tag.
    pub fn provider(&self) -> usize {
        self.provider
    }
}

/// Which images to show. The default shows all of them.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ImageFilter {
    orientation: Option<Orientation>,
    favorites_only: bool,
    /// Images must have every one of these tags.
    tags: Vec<String>,
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

    /// Only images with all of these tags, from the user or a provider.
    pub fn with_tags(mut self, tags: Vec<String>) -> Self {
        self.tags = tags;
        self
    }

    pub fn tags(&self) -> &[String] {
        &self.tags
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

    pub fn matches(&self, image: &ImageFile, marks: &ImageMarks) -> bool {
        let path = image.path();
        self.orientation
            .is_none_or(|orientation| image.orientation() == orientation)
            && (!self.favorites_only || marks.is_favorite(path))
            && self.tags.iter().all(|tag| marks.has_tag(path, tag))
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
        let marks = ImageMarks::new(favorites, HashMap::new(), HashMap::new());
        let shown = |filter: ImageFilter| -> Vec<String> {
            paths(
                &images
                    .iter()
                    .filter(|image| filter.matches(image, &marks))
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

    #[test]
    fn an_image_counts_once_per_tag_whoever_gave_it() {
        let tags = |list: &[&str]| list.iter().map(|tag| (*tag).to_owned()).collect();
        let marks = ImageMarks::new(
            HashSet::new(),
            HashMap::from([
                (PathBuf::from("a.png"), tags(&["Mountains"])),
                (PathBuf::from("b.png"), tags(&["night"])),
            ]),
            HashMap::from([(PathBuf::from("a.png"), tags(&["mountains", "lake"]))]),
        );
        let paths = [PathBuf::from("a.png"), PathBuf::from("b.png")];

        let counts = marks.tag_counts(paths.iter().map(PathBuf::as_path));

        let summary: Vec<_> = counts
            .iter()
            .map(|count| (count.name(), count.images(), count.user(), count.provider()))
            .collect();
        assert_eq!(
            summary,
            [
                ("lake", 1, 0, 1),
                ("Mountains", 1, 1, 1),
                ("night", 1, 1, 0)
            ]
        );
    }

    #[test]
    fn tags_match_all_of_them_from_the_user_or_a_provider_ignoring_case() {
        let images = [
            image("a.png", 0, 10, 10),
            image("b.png", 0, 10, 10),
            image("c.png", 0, 10, 10),
        ];
        let tags = |list: &[&str]| list.iter().map(|tag| (*tag).to_owned()).collect();
        let marks = ImageMarks::new(
            HashSet::new(),
            HashMap::from([
                (PathBuf::from("a.png"), tags(&["Night", "lake"])),
                (PathBuf::from("b.png"), tags(&["night"])),
            ]),
            HashMap::from([(PathBuf::from("b.png"), tags(&["Lake"]))]),
        );
        let shown = |wanted: &[&str]| -> Vec<String> {
            let filter = ImageFilter::default().with_tags(tags(wanted));
            paths(
                &images
                    .iter()
                    .filter(|image| filter.matches(image, &marks))
                    .cloned()
                    .collect::<Vec<_>>(),
            )
        };

        assert_eq!(shown(&["NIGHT"]), ["a.png", "b.png"]);
        assert_eq!(shown(&["night", "lake"]), ["a.png", "b.png"]);
        assert_eq!(shown(&["night", "forest"]), Vec::<String>::new());
        assert_eq!(shown(&[]).len(), 3);
    }
}
