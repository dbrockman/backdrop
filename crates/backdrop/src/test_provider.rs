//! A provider for UI tests: canned search pages and small generated images,
//! so tests never reach a real service.

use std::io::{Cursor, Write};
use std::sync::Mutex;

use backdrop_core::providers::{
    Category, Progress, Provider, ProviderError, RemoteWallpaper, SearchPage, SearchQuery,
    SearchSort,
};
use image::{ImageFormat, Rgb, RgbImage};

/// Serves `pages` of wallpaper ids and records every search. Its file names
/// are `fake-{id}.{extension}`.
pub struct FakeProvider {
    pages: Vec<Vec<&'static str>>,
    searches: Mutex<Vec<SearchQuery>>,
    api_key: Mutex<Option<String>>,
    /// Keys `verify_api_key` rejects.
    rejected_keys: Vec<&'static str>,
    /// Whether `verify_api_key` fails as if the network were down.
    offline: bool,
}

impl FakeProvider {
    pub fn new(pages: Vec<Vec<&'static str>>) -> Self {
        Self {
            pages,
            searches: Mutex::new(Vec::new()),
            api_key: Mutex::new(None),
            rejected_keys: Vec::new(),
            offline: false,
        }
    }

    pub fn rejecting(mut self, key: &'static str) -> Self {
        self.rejected_keys.push(key);
        self
    }

    pub fn offline(mut self) -> Self {
        self.offline = true;
        self
    }

    pub fn api_key(&self) -> Option<String> {
        self.api_key.lock().unwrap().clone()
    }

    pub fn searches(&self) -> Vec<SearchQuery> {
        self.searches.lock().unwrap().clone()
    }
}

/// A 20 × 10 PNG, the size `RemoteWallpaper::for_test` reports.
fn png() -> Vec<u8> {
    let mut bytes = Cursor::new(Vec::new());
    RgbImage::from_pixel(20, 10, Rgb([40, 80, 120]))
        .write_to(&mut bytes, ImageFormat::Png)
        .unwrap();
    bytes.into_inner()
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
        &[SearchSort::Newest]
    }

    fn search(&self, query: &SearchQuery) -> Result<SearchPage, ProviderError> {
        self.searches.lock().unwrap().push(query.clone());
        let ids = self
            .pages
            .get(query.page as usize - 1)
            .cloned()
            .unwrap_or_default();
        Ok(SearchPage {
            wallpapers: ids
                .into_iter()
                .map(|id| RemoteWallpaper::for_test("fake", id))
                .collect(),
            page: query.page,
            last_page: self.pages.len() as u32,
            total: self.pages.iter().map(Vec::len).sum::<usize>() as u64,
            seed: None,
        })
    }

    fn wallpaper(&self, id: &str) -> Result<RemoteWallpaper, ProviderError> {
        Ok(RemoteWallpaper::for_test("fake", id).with_tags(&["mountains", "lake"]))
    }

    fn download(
        &self,
        _: &RemoteWallpaper,
        to: &mut dyn Write,
        progress: Progress,
    ) -> Result<u64, ProviderError> {
        let image = png();
        to.write_all(&image).map_err(ProviderError::Write)?;
        progress(image.len() as u64, Some(image.len() as u64));
        Ok(image.len() as u64)
    }

    fn download_thumbnail(
        &self,
        wallpaper: &RemoteWallpaper,
        to: &mut dyn Write,
    ) -> Result<u64, ProviderError> {
        self.download(wallpaper, to, &mut |_, _| {})
    }

    fn has_api_key(&self) -> bool {
        self.api_key().is_some()
    }

    fn set_api_key(&self, api_key: Option<String>) {
        *self.api_key.lock().unwrap() = api_key;
    }

    fn verify_api_key(&self, api_key: &str) -> Result<(), ProviderError> {
        if self.offline {
            return Err(ProviderError::Status {
                provider: "Fake",
                status: 503,
            });
        }
        if self.rejected_keys.contains(&api_key) {
            return Err(ProviderError::ApiKeyRejected { provider: "Fake" });
        }
        Ok(())
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
