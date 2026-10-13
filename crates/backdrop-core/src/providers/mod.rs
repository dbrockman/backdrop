//! Online wallpaper providers, such as Wallhaven.
//!
//! Front ends search, describe, and download remote wallpapers through the
//! [`Provider`] trait and the types in this module, so nothing outside a
//! provider's own module depends on its API or data structures. Requests are
//! blocking; the GUI runs them off the UI thread.

pub mod wallhaven;

#[cfg(test)]
mod test_server;

use std::fmt;
use std::io::{self, Write};

use serde::{Deserialize, Serialize};
use toml::value::Datetime;

use crate::image::Orientation;

pub use wallhaven::Wallhaven;

#[derive(Debug, thiserror::Error)]
pub enum ProviderError {
    #[error("could not reach {provider}: {cause}")]
    Network {
        provider: &'static str,
        cause: Box<dyn std::error::Error + Send + Sync>,
    },
    #[error("{provider} has no wallpaper {id}")]
    NotFound { provider: &'static str, id: String },
    #[error("{provider} has no category {category}")]
    UnknownCategory {
        provider: &'static str,
        category: String,
    },
    #[error("{provider} needs an API key to show NSFW wallpapers")]
    ApiKeyRequired { provider: &'static str },
    #[error("{provider} rejected the API key")]
    ApiKeyRejected { provider: &'static str },
    #[error("{provider} is limiting requests; try again in a minute")]
    RateLimited { provider: &'static str },
    #[error("{provider} answered with HTTP status {status}")]
    Status { provider: &'static str, status: u16 },
    #[error("{provider} sent a response Backdrop doesn't understand: {message}")]
    InvalidResponse {
        provider: &'static str,
        message: String,
    },
    #[error("could not save the download: {0}")]
    Write(io::Error),
}

/// How explicit a wallpaper is, in the provider's own rating.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ContentRating {
    /// Safe for work.
    Sfw,
    /// Suggestive, but not explicit.
    Sketchy,
    /// Not safe for work.
    Nsfw,
}

impl ContentRating {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Sfw => "sfw",
            Self::Sketchy => "sketchy",
            Self::Nsfw => "nsfw",
        }
    }
}

/// A category a provider sorts its wallpapers into.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Category {
    /// Identifies the category in a [`SearchQuery`].
    pub id: &'static str,
    pub name: &'static str,
}

/// Search result orders. Each provider supports some of them
/// ([`Provider::sorts`]).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SearchSort {
    /// Most recently uploaded first.
    #[default]
    Newest,
    /// Best match for the search text first.
    Relevance,
    /// Most viewed first.
    Views,
    /// Most favorited first.
    Favorites,
    /// The provider's top list.
    Top,
    /// Random, but stable across the pages of one search ([`SearchPage::seed`]).
    Random,
}

/// What to search a provider for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SearchQuery {
    /// Words or tags, in the provider's search syntax. Empty browses everything.
    pub text: String,
    /// The content ratings to include. Empty means safe for work only.
    pub ratings: Vec<ContentRating>,
    /// The ids of the [`Category`]s to include. Empty means all of them.
    pub categories: Vec<String>,
    pub orientation: Option<Orientation>,
    /// Only wallpapers at least this wide and tall.
    pub at_least: Option<(u32, u32)>,
    pub sort: SearchSort,
    /// The page of results, starting at 1.
    pub page: u32,
    /// The seed of an earlier page of a random search, to continue it.
    pub seed: Option<String>,
}

impl Default for SearchQuery {
    fn default() -> Self {
        Self {
            text: String::new(),
            ratings: vec![ContentRating::Sfw],
            categories: Vec::new(),
            orientation: None,
            at_least: None,
            sort: SearchSort::default(),
            page: 1,
            seed: None,
        }
    }
}

impl SearchQuery {
    /// The ratings to include: [`ContentRating::Sfw`] when none are chosen.
    pub fn effective_ratings(&self) -> Vec<ContentRating> {
        let mut ratings = self.ratings.clone();
        if ratings.is_empty() {
            ratings.push(ContentRating::Sfw);
        }
        ratings.sort();
        ratings.dedup();
        ratings
    }
}

/// One page of search results.
#[derive(Clone, Debug, PartialEq)]
pub struct SearchPage {
    /// Without tags; [`Provider::wallpaper`] fetches them.
    pub wallpapers: Vec<RemoteWallpaper>,
    pub page: u32,
    pub last_page: u32,
    /// Matching wallpapers on all pages.
    pub total: u64,
    /// Pass in [`SearchQuery::seed`] to fetch more pages of a random search.
    pub seed: Option<String>,
}

/// A tag a provider attached to a wallpaper.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Tag {
    pub name: String,
    pub category: Option<String>,
}

/// A wallpaper offered by a provider.
#[derive(Clone, Debug, PartialEq)]
pub struct RemoteWallpaper {
    pub(crate) provider: &'static str,
    pub(crate) id: String,
    pub(crate) page_url: String,
    pub(crate) image_url: String,
    pub(crate) thumbnail_url: String,
    pub(crate) preview_url: String,
    pub(crate) width: u32,
    pub(crate) height: u32,
    pub(crate) file_size: Option<u64>,
    pub(crate) media_type: Option<String>,
    pub(crate) rating: ContentRating,
    pub(crate) category: Option<String>,
    pub(crate) uploaded: Option<Datetime>,
    pub(crate) uploader: Option<String>,
    pub(crate) source_url: Option<String>,
    pub(crate) views: Option<u64>,
    pub(crate) favorites: Option<u64>,
    pub(crate) colors: Vec<String>,
    pub(crate) tags: Option<Vec<Tag>>,
}

impl RemoteWallpaper {
    /// The id of the provider, e.g. `wallhaven`.
    pub fn provider(&self) -> &'static str {
        self.provider
    }

    /// The provider's id for this wallpaper.
    pub fn id(&self) -> &str {
        &self.id
    }

    /// The wallpaper as library metadata refers to it: `provider/id`.
    pub fn source(&self) -> String {
        format!("{}/{}", self.provider, self.id)
    }

    /// The wallpaper's page on the provider's website.
    pub fn page_url(&self) -> &str {
        &self.page_url
    }

    /// The original, full-resolution image.
    pub fn image_url(&self) -> &str {
        &self.image_url
    }

    /// A small image cropped to the provider's tile shape, for grids.
    pub fn thumbnail_url(&self) -> &str {
        &self.thumbnail_url
    }

    /// A small image in the wallpaper's own aspect ratio.
    pub fn preview_url(&self) -> &str {
        &self.preview_url
    }

    pub fn width(&self) -> u32 {
        self.width
    }

    pub fn height(&self) -> u32 {
        self.height
    }

    pub fn orientation(&self) -> Orientation {
        Orientation::of(self.width, self.height)
    }

    /// The size of the original image in bytes.
    pub fn file_size(&self) -> Option<u64> {
        self.file_size
    }

    /// The original image's media type, e.g. `image/jpeg`.
    pub fn media_type(&self) -> Option<&str> {
        self.media_type.as_deref()
    }

    pub fn rating(&self) -> ContentRating {
        self.rating
    }

    /// The name of the provider's [`Category`].
    pub fn category(&self) -> Option<&str> {
        self.category.as_deref()
    }

    /// When the wallpaper was published, as the provider reports it.
    pub fn uploaded(&self) -> Option<&Datetime> {
        self.uploaded.as_ref()
    }

    pub fn uploader(&self) -> Option<&str> {
        self.uploader.as_deref()
    }

    /// Where the image was originally published, if the provider knows.
    pub fn source_url(&self) -> Option<&str> {
        self.source_url.as_deref()
    }

    pub fn views(&self) -> Option<u64> {
        self.views
    }

    pub fn favorites(&self) -> Option<u64> {
        self.favorites
    }

    /// The image's main colors, as `#rrggbb`.
    pub fn colors(&self) -> &[String] {
        &self.colors
    }

    /// The wallpaper's tags, or `None` when they weren't fetched, as in
    /// search results.
    pub fn tags(&self) -> Option<&[Tag]> {
        self.tags.as_deref()
    }
}

/// The outcome of [`check_api_key`] for a key worth saving.
#[derive(Debug)]
pub enum KeyCheck {
    Accepted,
    /// The provider couldn't be asked, for example while offline. The key
    /// may still be right, so it is worth keeping.
    Unchecked(ProviderError),
}

/// Checks `api_key` with `provider` before it is saved. Only a key the
/// provider rejects is an error; one that couldn't be checked is kept, so
/// setting a key works offline.
pub fn check_api_key(provider: &dyn Provider, api_key: &str) -> Result<KeyCheck, ProviderError> {
    match provider.verify_api_key(api_key) {
        Ok(()) => Ok(KeyCheck::Accepted),
        Err(error @ ProviderError::ApiKeyRejected { .. }) => Err(error),
        Err(error) => Ok(KeyCheck::Unchecked(error)),
    }
}

/// Download progress: bytes received so far, and the total if known.
pub type Progress<'a> = &'a mut dyn FnMut(u64, Option<u64>);

/// An online source of wallpapers.
pub trait Provider: Send + Sync {
    /// Identifies the provider in library metadata, e.g. `wallhaven`.
    fn id(&self) -> &'static str;

    /// The provider's name, for display.
    fn name(&self) -> &'static str;

    /// The categories a search can be limited to.
    fn categories(&self) -> &'static [Category];

    /// The orders search results can be sorted in.
    fn sorts(&self) -> &'static [SearchSort];

    fn search(&self, query: &SearchQuery) -> Result<SearchPage, ProviderError>;

    /// The wallpaper with the provider id `id`, with its tags.
    fn wallpaper(&self, id: &str) -> Result<RemoteWallpaper, ProviderError>;

    /// Writes the original image to `to`, reporting progress as it arrives.
    /// Returns the number of bytes written.
    fn download(
        &self,
        wallpaper: &RemoteWallpaper,
        to: &mut dyn Write,
        progress: Progress,
    ) -> Result<u64, ProviderError>;

    /// Whether requests are authenticated with an API key.
    fn has_api_key(&self) -> bool {
        false
    }

    /// Authenticates later requests with `api_key`, or with none. Providers
    /// without keys ignore it.
    fn set_api_key(&self, _api_key: Option<String>) {}

    /// Checks with the provider that it accepts `api_key`, without using it
    /// for other requests. [`ProviderError::ApiKeyRejected`] means it doesn't;
    /// any other error means the key couldn't be checked.
    fn verify_api_key(&self, _api_key: &str) -> Result<(), ProviderError> {
        Ok(())
    }

    /// The file name a download of wallpaper `id` is saved as, with the
    /// extension of its format, such as `jpg`.
    fn file_name(&self, id: &str, extension: &str) -> String;

    /// Writes the wallpaper's thumbnail ([`RemoteWallpaper::thumbnail_url`]) to
    /// `to`. Returns the number of bytes written.
    fn download_thumbnail(
        &self,
        wallpaper: &RemoteWallpaper,
        to: &mut dyn Write,
    ) -> Result<u64, ProviderError>;

    /// The wallpaper id in a file name following the provider's naming
    /// convention, such as `wallhaven-94x38z.jpg`.
    fn id_from_file_name(&self, file_name: &str) -> Option<String>;

    /// The wallpaper id in text a user typed or pasted: an id, a link to the
    /// wallpaper, or a file name.
    fn parse_id(&self, text: &str) -> Option<String>;
}

impl fmt::Debug for dyn Provider {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

#[cfg(any(test, feature = "test-support"))]
impl RemoteWallpaper {
    /// A 20 × 10 safe-for-work wallpaper of `provider` with id `id`, without
    /// tags, as in search results.
    pub fn for_test(provider: &'static str, id: &str) -> Self {
        Self {
            provider,
            id: id.to_owned(),
            page_url: format!("https://example.com/w/{id}"),
            image_url: format!("https://example.com/full/{id}"),
            thumbnail_url: format!("https://example.com/thumb/{id}"),
            preview_url: format!("https://example.com/preview/{id}"),
            width: 20,
            height: 10,
            file_size: None,
            media_type: None,
            rating: ContentRating::Sfw,
            category: None,
            uploaded: None,
            uploader: None,
            source_url: None,
            views: None,
            favorites: None,
            colors: Vec::new(),
            tags: None,
        }
    }

    /// The wallpaper with these tags, as its details have them.
    pub fn with_tags(mut self, names: &[&str]) -> Self {
        self.tags = Some(
            names
                .iter()
                .map(|name| Tag {
                    name: (*name).to_owned(),
                    category: None,
                })
                .collect(),
        );
        self
    }
}
