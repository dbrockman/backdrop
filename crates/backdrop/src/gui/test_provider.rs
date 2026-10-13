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
}

impl FakeProvider {
    pub fn new(pages: Vec<Vec<&'static str>>) -> Self {
        Self {
            pages,
            searches: Mutex::new(Vec::new()),
        }
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
        Ok(RemoteWallpaper::for_test("fake", id))
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
