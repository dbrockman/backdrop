//! [Wallhaven](https://wallhaven.cc), through its API (v1).
//!
//! Without an API key, Wallhaven serves safe-for-work and sketchy wallpapers;
//! NSFW wallpapers need the key of an account that allows them. The key goes
//! only to the API, never to the image servers.

use std::collections::VecDeque;
use std::io::{Read as _, Write};
use std::sync::{Mutex, PoisonError, RwLock};
use std::time::{Duration, Instant};

use serde::Deserialize;
use ureq::http::Response;

use super::{
    Category, ContentRating, Progress, Provider, ProviderError, RemoteWallpaper, SearchPage,
    SearchQuery, SearchSort, Tag,
};
use crate::image::Orientation;

const ID: &str = "wallhaven";
const NAME: &str = "Wallhaven";
const API_URL: &str = "https://wallhaven.cc/api/v1";
/// File names of downloaded originals start with this, e.g.
/// `wallhaven-94x38z.jpg`.
const FILE_PREFIX: &str = "wallhaven-";
/// API responses are a few kilobytes; anything near this is not one.
const MAX_RESPONSE_BYTES: u64 = 8 * 1024 * 1024;
/// Wallhaven allows this many API requests a minute.
pub const RATE_LIMIT: usize = 45;
const RATE_WINDOW: Duration = Duration::from_secs(60);

const CATEGORIES: &[Category] = &[
    Category {
        id: "general",
        name: "General",
    },
    Category {
        id: "anime",
        name: "Anime",
    },
    Category {
        id: "people",
        name: "People",
    },
];

const SORTS: &[SearchSort] = &[
    SearchSort::Newest,
    SearchSort::Relevance,
    SearchSort::Views,
    SearchSort::Favorites,
    SearchSort::Top,
    SearchSort::Random,
];

const RATINGS: [ContentRating; 3] = [
    ContentRating::Sfw,
    ContentRating::Sketchy,
    ContentRating::Nsfw,
];

pub struct Wallhaven {
    agent: ureq::Agent,
    api_url: String,
    /// Changeable while the client is in use, so one client and its rate
    /// limit serve the whole application.
    api_key: RwLock<Option<String>>,
    /// When recent API requests were sent, or are due to be sent.
    sent: Mutex<VecDeque<Instant>>,
}

impl Wallhaven {
    /// A client for wallhaven.cc, authenticated with `api_key` if given.
    pub fn new(api_key: Option<String>) -> Self {
        Self::with_api_url(API_URL, api_key)
    }

    /// Sends API requests to `api_url` in place of wallhaven.cc, for tests.
    pub fn with_api_url(api_url: impl Into<String>, api_key: Option<String>) -> Self {
        let config = ureq::Agent::config_builder()
            .user_agent(concat!("backdrop/", env!("CARGO_PKG_VERSION")))
            .http_status_as_error(false)
            .timeout_connect(Some(Duration::from_secs(15)))
            .timeout_recv_response(Some(Duration::from_secs(30)))
            .build();
        Self {
            agent: config.into(),
            api_url: api_url.into(),
            api_key: RwLock::new(api_key.filter(|key| !key.is_empty())),
            sent: Mutex::new(VecDeque::new()),
        }
    }

    fn current_api_key(&self) -> Option<String> {
        self.api_key
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    /// Sends an API request with the current API key and checks its status.
    fn get(
        &self,
        path: &str,
        query: &[(&str, String)],
    ) -> Result<Response<ureq::Body>, ProviderError> {
        self.get_with_key(path, query, self.current_api_key().as_deref())
    }

    /// Sends an API request authenticated with `api_key` and checks its status.
    /// Requests beyond Wallhaven's rate limit wait until they are allowed, so a
    /// batch, such as linking a whole library, slows down rather than fails.
    fn get_with_key(
        &self,
        path: &str,
        query: &[(&str, String)],
        api_key: Option<&str>,
    ) -> Result<Response<ureq::Body>, ProviderError> {
        let delay = {
            let mut sent = self.sent.lock().unwrap_or_else(PoisonError::into_inner);
            let now = Instant::now();
            let delay = rate_limit_delay(&mut sent, now, RATE_LIMIT, RATE_WINDOW);
            sent.push_back(now + delay);
            delay
        };
        std::thread::sleep(delay);
        let mut request = self
            .agent
            .get(format!("{}{path}", self.api_url))
            .query_pairs(query.iter().map(|(key, value)| (*key, value.as_str())));
        if let Some(key) = api_key {
            request = request.header("X-API-Key", key);
        }
        let response = request.call().map_err(network_error)?;
        match response.status().as_u16() {
            200..=299 => Ok(response),
            401 if api_key.is_some() => Err(ProviderError::ApiKeyRejected { provider: NAME }),
            401 => Err(ProviderError::ApiKeyRequired { provider: NAME }),
            429 => Err(ProviderError::RateLimited { provider: NAME }),
            status => Err(ProviderError::Status {
                provider: NAME,
                status,
            }),
        }
    }

    /// Writes the file at `url`, an image of wallpaper `id`, to `to`.
    fn fetch_file(
        &self,
        url: &str,
        id: &str,
        to: &mut dyn Write,
        progress: Progress,
    ) -> Result<u64, ProviderError> {
        // Image servers don't need the API key, so it isn't sent to them.
        let mut response = self.agent.get(url).call().map_err(network_error)?;
        match response.status().as_u16() {
            200..=299 => {}
            404 => {
                return Err(ProviderError::NotFound {
                    provider: NAME,
                    id: id.to_owned(),
                });
            }
            status => {
                return Err(ProviderError::Status {
                    provider: NAME,
                    status,
                });
            }
        }
        let total = response.body().content_length();
        let mut reader = response.body_mut().as_reader();
        let mut buffer = vec![0; 64 * 1024];
        let mut received = 0;
        progress(received, total);
        loop {
            let read = reader
                .read(&mut buffer)
                .map_err(|error| network_error(ureq::Error::Io(error)))?;
            if read == 0 {
                break;
            }
            to.write_all(&buffer[..read])
                .map_err(ProviderError::Write)?;
            received += read as u64;
            progress(received, total);
        }
        Ok(received)
    }

    fn get_json<T: for<'de> Deserialize<'de>>(
        &self,
        path: &str,
        query: &[(&str, String)],
    ) -> Result<T, ProviderError> {
        let mut response = self.get(path, query)?;
        let text = response
            .body_mut()
            .with_config()
            .limit(MAX_RESPONSE_BYTES)
            .read_to_string()
            .map_err(network_error)?;
        serde_json::from_str(&text).map_err(|error| invalid_response(error.to_string()))
    }
}

impl Provider for Wallhaven {
    fn id(&self) -> &'static str {
        ID
    }

    fn name(&self) -> &'static str {
        NAME
    }

    fn categories(&self) -> &'static [Category] {
        CATEGORIES
    }

    fn sorts(&self) -> &'static [SearchSort] {
        SORTS
    }

    fn search(&self, query: &SearchQuery) -> Result<SearchPage, ProviderError> {
        let ratings = query.effective_ratings();
        if ratings.contains(&ContentRating::Nsfw) && !self.has_api_key() {
            return Err(ProviderError::ApiKeyRequired { provider: NAME });
        }
        let response: SearchResponse = self.get_json("/search", &search_params(query)?)?;
        let wallpapers = response
            .data
            .into_iter()
            .map(ApiWallpaper::into_wallpaper)
            .collect::<Result<Vec<_>, _>>()?;
        Ok(SearchPage {
            // Never show a rating that wasn't asked for, whatever the API sends.
            wallpapers: wallpapers
                .into_iter()
                .filter(|wallpaper| ratings.contains(&wallpaper.rating))
                .collect(),
            page: response.meta.current_page,
            last_page: response.meta.last_page,
            total: response.meta.total,
            seed: response.meta.seed,
        })
    }

    fn wallpaper(&self, id: &str) -> Result<RemoteWallpaper, ProviderError> {
        let not_found = || ProviderError::NotFound {
            provider: NAME,
            id: id.to_owned(),
        };
        // The id becomes part of the URL, so only well-formed ids are sent.
        if !is_valid_id(id) {
            return Err(not_found());
        }
        let response: WallpaperResponse = match self.get_json(&format!("/w/{id}"), &[]) {
            Err(ProviderError::Status { status: 404, .. }) => return Err(not_found()),
            response => response?,
        };
        response.data.into_wallpaper()
    }

    fn download(
        &self,
        wallpaper: &RemoteWallpaper,
        to: &mut dyn Write,
        progress: Progress,
    ) -> Result<u64, ProviderError> {
        self.fetch_file(&wallpaper.image_url, &wallpaper.id, to, progress)
    }

    fn download_thumbnail(
        &self,
        wallpaper: &RemoteWallpaper,
        to: &mut dyn Write,
    ) -> Result<u64, ProviderError> {
        self.fetch_file(&wallpaper.thumbnail_url, &wallpaper.id, to, &mut |_, _| {})
    }

    fn has_api_key(&self) -> bool {
        self.current_api_key().is_some()
    }

    fn set_api_key(&self, api_key: Option<String>) {
        *self.api_key.write().unwrap_or_else(PoisonError::into_inner) =
            api_key.filter(|key| !key.is_empty());
    }

    /// Asks for the account settings, which only a valid key can read.
    /// Wallhaven answers an unknown key with 404 rather than 401.
    fn verify_api_key(&self, api_key: &str) -> Result<(), ProviderError> {
        match self.get_with_key("/settings", &[], Some(api_key)) {
            Ok(_) => Ok(()),
            Err(ProviderError::Status { status: 404, .. }) => {
                Err(ProviderError::ApiKeyRejected { provider: NAME })
            }
            Err(error) => Err(error),
        }
    }

    fn file_name(&self, id: &str, extension: &str) -> String {
        format!("{FILE_PREFIX}{id}.{extension}")
    }

    fn id_from_file_name(&self, file_name: &str) -> Option<String> {
        let (id, extension) = file_name.strip_prefix(FILE_PREFIX)?.split_once('.')?;
        let extension_ok =
            !extension.is_empty() && extension.chars().all(|c| c.is_ascii_alphanumeric());
        (is_valid_id(id) && extension_ok).then(|| id.to_owned())
    }

    fn parse_id(&self, text: &str) -> Option<String> {
        let text = text.trim();
        if is_valid_id(text) {
            return Some(text.to_owned());
        }
        let path = text.split(['?', '#']).next()?;
        let path = path
            .strip_prefix("https://")
            .or_else(|| path.strip_prefix("http://"))
            .unwrap_or(path);
        let path = path
            .strip_prefix("www.")
            .unwrap_or(path)
            .trim_end_matches('/');
        // Wallpaper pages and short links.
        for prefix in ["wallhaven.cc/w/", "whvn.cc/"] {
            if let Some(id) = path.strip_prefix(prefix)
                && is_valid_id(id)
            {
                return Some(id.to_owned());
            }
        }
        // Image links and downloaded files.
        self.id_from_file_name(path.rsplit('/').next()?)
    }
}

/// How long to wait before sending a request so that no `window` holds more
/// than `limit` of them, given when earlier ones were sent. Forgets requests
/// that have left the window.
fn rate_limit_delay(
    sent: &mut VecDeque<Instant>,
    now: Instant,
    limit: usize,
    window: Duration,
) -> Duration {
    while sent
        .front()
        .is_some_and(|&time| now.saturating_duration_since(time) >= window)
    {
        sent.pop_front();
    }
    match sent.len().checked_sub(limit) {
        None => Duration::ZERO,
        // The request `limit` back must leave the window first.
        Some(index) => (sent[index] + window).saturating_duration_since(now),
    }
}

/// Wallhaven ids are short strings of lowercase letters and digits.
fn is_valid_id(id: &str) -> bool {
    (1..=16).contains(&id.len())
        && id
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
}

/// The query parameters of a search, as the API names them.
fn search_params(query: &SearchQuery) -> Result<Vec<(&'static str, String)>, ProviderError> {
    let mut params = Vec::new();
    let text = query.text.trim();
    if !text.is_empty() {
        params.push(("q", text.to_owned()));
    }
    if let Some(unknown) = query
        .categories
        .iter()
        .find(|id| !CATEGORIES.iter().any(|category| category.id == id.as_str()))
    {
        return Err(ProviderError::UnknownCategory {
            provider: NAME,
            category: unknown.clone(),
        });
    }
    let categories = CATEGORIES.iter().map(|category| {
        query.categories.is_empty() || query.categories.iter().any(|id| id == category.id)
    });
    params.push(("categories", flags(categories)));
    let ratings = query.effective_ratings();
    params.push((
        "purity",
        flags(RATINGS.iter().map(|rating| ratings.contains(rating))),
    ));
    let sorting = match query.sort {
        SearchSort::Newest => "date_added",
        SearchSort::Relevance => "relevance",
        SearchSort::Views => "views",
        SearchSort::Favorites => "favorites",
        SearchSort::Top => "toplist",
        SearchSort::Random => "random",
    };
    params.push(("sorting", sorting.to_owned()));
    if let Some(orientation) = query.orientation {
        let ratios = match orientation {
            Orientation::Landscape => "landscape",
            Orientation::Portrait => "portrait",
            Orientation::Square => "1x1",
        };
        params.push(("ratios", ratios.to_owned()));
    }
    if let Some((width, height)) = query.at_least {
        params.push(("atleast", format!("{width}x{height}")));
    }
    if query.page > 1 {
        params.push(("page", query.page.to_string()));
    }
    if let Some(seed) = &query.seed {
        params.push(("seed", seed.clone()));
    }
    Ok(params)
}

/// Wallhaven's on/off flags, e.g. `110`.
fn flags(values: impl Iterator<Item = bool>) -> String {
    values.map(|on| if on { '1' } else { '0' }).collect()
}

fn network_error(error: ureq::Error) -> ProviderError {
    ProviderError::Network {
        provider: NAME,
        cause: Box::new(error),
    }
}

fn invalid_response(message: impl Into<String>) -> ProviderError {
    ProviderError::InvalidResponse {
        provider: NAME,
        message: message.into(),
    }
}

#[derive(Deserialize)]
struct SearchResponse {
    data: Vec<ApiWallpaper>,
    meta: SearchMeta,
}

#[derive(Deserialize)]
struct SearchMeta {
    current_page: u32,
    last_page: u32,
    total: u64,
    seed: Option<String>,
}

#[derive(Deserialize)]
struct WallpaperResponse {
    data: ApiWallpaper,
}

/// A wallpaper as the API describes it. Search results leave out `uploader`
/// and `tags`.
#[derive(Deserialize)]
struct ApiWallpaper {
    id: String,
    url: String,
    path: String,
    thumbs: Thumbs,
    dimension_x: u32,
    dimension_y: u32,
    file_size: Option<u64>,
    file_type: Option<String>,
    purity: String,
    category: Option<String>,
    created_at: Option<String>,
    source: Option<String>,
    views: Option<u64>,
    favorites: Option<u64>,
    #[serde(default)]
    colors: Vec<String>,
    uploader: Option<Uploader>,
    tags: Option<Vec<ApiTag>>,
}

#[derive(Deserialize)]
struct Thumbs {
    large: String,
    original: String,
}

#[derive(Deserialize)]
struct Uploader {
    username: String,
}

#[derive(Deserialize)]
struct ApiTag {
    name: String,
    category: Option<String>,
}

impl ApiWallpaper {
    fn into_wallpaper(self) -> Result<RemoteWallpaper, ProviderError> {
        if !is_valid_id(&self.id) {
            return Err(invalid_response(format!(
                "invalid wallpaper id {:?}",
                self.id
            )));
        }
        let rating = match self.purity.as_str() {
            "sfw" => ContentRating::Sfw,
            "sketchy" => ContentRating::Sketchy,
            // Anything unexpected is treated as the most explicit rating.
            _ => ContentRating::Nsfw,
        };
        let non_empty = |text: Option<String>| text.filter(|text| !text.trim().is_empty());
        Ok(RemoteWallpaper {
            provider: ID,
            id: self.id,
            page_url: self.url,
            image_url: self.path,
            thumbnail_url: self.thumbs.large,
            preview_url: self.thumbs.original,
            width: self.dimension_x,
            height: self.dimension_y,
            file_size: self.file_size,
            media_type: non_empty(self.file_type),
            rating,
            category: non_empty(self.category),
            // "2026-10-02 13:39:14", with no time zone.
            uploaded: self.created_at.and_then(|date| date.parse().ok()),
            uploader: non_empty(self.uploader.map(|uploader| uploader.username)),
            source_url: non_empty(self.source),
            views: self.views,
            favorites: self.favorites,
            colors: self.colors,
            tags: self.tags.map(|tags| {
                tags.into_iter()
                    .map(|tag| Tag {
                        name: tag.name,
                        category: non_empty(tag.category),
                    })
                    .collect()
            }),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::super::test_server::TestServer;
    use super::*;

    /// A search result, as the API sends it.
    fn api_wallpaper(id: &str, purity: &str) -> String {
        format!(
            r##"{{
                "id": "{id}",
                "url": "https://wallhaven.cc/w/{id}",
                "short_url": "https://whvn.cc/{id}",
                "views": 14135,
                "favorites": 177,
                "source": "",
                "purity": "{purity}",
                "category": "general",
                "dimension_x": 4928,
                "dimension_y": 3280,
                "resolution": "4928x3280",
                "ratio": "1.5",
                "file_size": 1854551,
                "file_type": "image/jpeg",
                "created_at": "2026-10-02 13:39:14",
                "colors": ["#000000", "#999999"],
                "path": "https://w.wallhaven.cc/full/ly/wallhaven-{id}.jpg",
                "thumbs": {{
                    "large": "https://th.wallhaven.cc/lg/ly/{id}.jpg",
                    "original": "https://th.wallhaven.cc/orig/ly/{id}.jpg",
                    "small": "https://th.wallhaven.cc/small/ly/{id}.jpg"
                }}
            }}"##
        )
    }

    fn search_response(wallpapers: &[String]) -> Vec<u8> {
        format!(
            r#"{{
                "data": [{}],
                "meta": {{
                    "current_page": 2, "last_page": 6, "per_page": 24, "total": 135,
                    "query": {{"id": 328, "tag": "mountains"}}, "seed": null
                }}
            }}"#,
            wallpapers.join(",")
        )
        .into_bytes()
    }

    /// A wallpaper's details, which add the uploader and tags.
    fn wallpaper_response(id: &str) -> Vec<u8> {
        let wallpaper = api_wallpaper(id, "sfw");
        let details = r#"
            "uploader": {"username": "Tempest06123", "group": "User", "avatar": {}},
            "tags": [
                {"id": 328, "name": "mountains", "alias": "mount", "category_id": 5,
                 "category": "Nature", "purity": "sfw", "created_at": "2014-02-11 16:20:39"},
                {"id": 32, "name": "space", "alias": "", "category_id": 45,
                 "category": "", "purity": "sfw", "created_at": "2014-02-02 17:54:50"}
            ],"#;
        format!(r#"{{"data": {{{details}{}}}"#, &wallpaper.trim()[1..]).into_bytes()
    }

    fn search(server: &TestServer, api_key: Option<&str>, query: &SearchQuery) -> SearchPage {
        Wallhaven::with_api_url(server.url(), api_key.map(str::to_owned))
            .search(query)
            .unwrap()
    }

    #[test]
    fn search_reads_a_page_of_wallpapers() {
        let server = TestServer::start(vec![(
            200,
            search_response(&[api_wallpaper("lygg6r", "sfw")]),
        )]);

        let page = search(&server, None, &SearchQuery::default());

        assert_eq!((page.page, page.last_page, page.total), (2, 6, 135));
        assert_eq!(page.seed, None);
        let [wallpaper] = page.wallpapers.as_slice() else {
            panic!("expected one wallpaper: {:?}", page.wallpapers);
        };
        assert_eq!(wallpaper.source(), "wallhaven/lygg6r");
        assert_eq!(wallpaper.page_url(), "https://wallhaven.cc/w/lygg6r");
        assert_eq!(
            wallpaper.image_url(),
            "https://w.wallhaven.cc/full/ly/wallhaven-lygg6r.jpg"
        );
        assert_eq!(
            wallpaper.thumbnail_url(),
            "https://th.wallhaven.cc/lg/ly/lygg6r.jpg"
        );
        assert_eq!((wallpaper.width(), wallpaper.height()), (4928, 3280));
        assert_eq!(wallpaper.orientation(), Orientation::Landscape);
        assert_eq!(wallpaper.file_size(), Some(1854551));
        assert_eq!(wallpaper.media_type(), Some("image/jpeg"));
        assert_eq!(wallpaper.rating(), ContentRating::Sfw);
        assert_eq!(wallpaper.category(), Some("general"));
        assert_eq!(
            wallpaper.uploaded().map(ToString::to_string).as_deref(),
            Some("2026-10-02T13:39:14")
        );
        assert_eq!(wallpaper.source_url(), None);
        assert_eq!(wallpaper.colors(), ["#000000", "#999999"]);
        assert_eq!(wallpaper.tags(), None);

        let [request] = server.requests().try_into().unwrap();
        assert_eq!(
            request.target,
            "/search?categories=111&purity=100&sorting=date_added"
        );
        assert_eq!(request.header("x-api-key"), None);
    }

    #[test]
    fn search_sends_the_query_and_api_key() {
        let server = TestServer::start(vec![(200, search_response(&[]))]);
        let query = SearchQuery {
            text: " mountain lake ".to_owned(),
            ratings: vec![ContentRating::Nsfw, ContentRating::Sfw],
            categories: vec!["general".to_owned(), "people".to_owned()],
            orientation: Some(Orientation::Portrait),
            at_least: Some((2560, 1440)),
            sort: SearchSort::Random,
            page: 3,
            seed: Some("abc123".to_owned()),
        };

        search(&server, Some("secret"), &query);

        let [request] = server.requests().try_into().unwrap();
        assert_eq!(
            request.target,
            "/search?q=mountain%20lake&categories=101&purity=101&sorting=random\
             &ratios=portrait&atleast=2560x1440&page=3&seed=abc123"
        );
        assert_eq!(request.header("x-api-key"), Some("secret"));
    }

    #[test]
    fn search_never_returns_ratings_that_were_not_asked_for() {
        let server = TestServer::start(vec![(
            200,
            search_response(&[
                api_wallpaper("aaaaaa", "sfw"),
                api_wallpaper("bbbbbb", "sketchy"),
                api_wallpaper("cccccc", "nsfw"),
                api_wallpaper("dddddd", "unknown"),
            ]),
        )]);
        let query = SearchQuery {
            ratings: vec![ContentRating::Sfw, ContentRating::Sketchy],
            ..SearchQuery::default()
        };

        let page = search(&server, None, &query);

        let ids: Vec<_> = page.wallpapers.iter().map(RemoteWallpaper::id).collect();
        assert_eq!(ids, ["aaaaaa", "bbbbbb"]);
    }

    #[test]
    fn nsfw_search_needs_an_api_key() {
        let server = TestServer::start(vec![]);
        let query = SearchQuery {
            ratings: vec![ContentRating::Nsfw],
            ..SearchQuery::default()
        };

        let result = Wallhaven::with_api_url(server.url(), None).search(&query);

        assert!(matches!(result, Err(ProviderError::ApiKeyRequired { .. })));
        assert!(server.requests().is_empty());
    }

    #[test]
    fn unknown_categories_are_rejected() {
        let query = SearchQuery {
            categories: vec!["landscapes".to_owned()],
            ..SearchQuery::default()
        };
        assert!(matches!(
            search_params(&query),
            Err(ProviderError::UnknownCategory { .. })
        ));
    }

    #[test]
    fn search_sorts_map_to_the_api() {
        let sorting = |sort| {
            let query = SearchQuery {
                sort,
                ..SearchQuery::default()
            };
            search_params(&query)
                .unwrap()
                .into_iter()
                .find(|(key, _)| *key == "sorting")
                .unwrap()
                .1
        };
        assert_eq!(sorting(SearchSort::Newest), "date_added");
        assert_eq!(sorting(SearchSort::Relevance), "relevance");
        assert_eq!(sorting(SearchSort::Views), "views");
        assert_eq!(sorting(SearchSort::Favorites), "favorites");
        assert_eq!(sorting(SearchSort::Top), "toplist");
        assert_eq!(sorting(SearchSort::Random), "random");
    }

    #[test]
    fn wallpaper_reads_tags_and_uploader() {
        let server = TestServer::start(vec![(200, wallpaper_response("lygg6r"))]);

        let wallpaper = Wallhaven::with_api_url(server.url(), Some("secret".to_owned()))
            .wallpaper("lygg6r")
            .unwrap();

        assert_eq!(wallpaper.id(), "lygg6r");
        assert_eq!(wallpaper.uploader(), Some("Tempest06123"));
        assert_eq!(
            wallpaper.tags().unwrap(),
            [
                Tag {
                    name: "mountains".to_owned(),
                    category: Some("Nature".to_owned()),
                },
                Tag {
                    name: "space".to_owned(),
                    category: None,
                },
            ]
        );
        let [request] = server.requests().try_into().unwrap();
        assert_eq!(request.target, "/w/lygg6r");
        assert_eq!(request.header("x-api-key"), Some("secret"));
    }

    #[test]
    fn malformed_ids_are_never_requested() {
        let server = TestServer::start(vec![]);
        let wallhaven = Wallhaven::with_api_url(server.url(), None);

        for id in ["", "../search", "LYGG6R", "lygg6r?x=1"] {
            assert!(matches!(
                wallhaven.wallpaper(id),
                Err(ProviderError::NotFound { .. })
            ));
        }
        assert!(server.requests().is_empty());
    }

    #[test]
    fn malformed_ids_in_responses_are_rejected() {
        let server = TestServer::start(vec![(
            200,
            search_response(&[api_wallpaper("../../etc", "sfw")]),
        )]);

        let result = Wallhaven::with_api_url(server.url(), None).search(&SearchQuery::default());

        assert!(matches!(result, Err(ProviderError::InvalidResponse { .. })));
    }

    #[test]
    fn error_statuses_are_explained() {
        let server = TestServer::start(vec![
            (404, br#"{"error":"Nothing here"}"#.to_vec()),
            (401, br#"{"error":"Unauthorized"}"#.to_vec()),
            (429, Vec::new()),
            (503, Vec::new()),
        ]);
        let anonymous = Wallhaven::with_api_url(server.url(), None);
        let with_key = Wallhaven::with_api_url(server.url(), Some("wrong".to_owned()));

        assert!(matches!(
            anonymous.wallpaper("aaaaaa"),
            Err(ProviderError::NotFound { id, .. }) if id == "aaaaaa"
        ));
        assert!(matches!(
            with_key.wallpaper("aaaaaa"),
            Err(ProviderError::ApiKeyRejected { .. })
        ));
        assert!(matches!(
            anonymous.search(&SearchQuery::default()),
            Err(ProviderError::RateLimited { .. })
        ));
        assert!(matches!(
            anonymous.search(&SearchQuery::default()),
            Err(ProviderError::Status { status: 503, .. })
        ));
    }

    #[test]
    fn unauthorized_without_a_key_asks_for_one() {
        let server = TestServer::start(vec![(401, br#"{"error":"Unauthorized"}"#.to_vec())]);

        let result = Wallhaven::with_api_url(server.url(), None).wallpaper("aaaaaa");

        assert!(matches!(result, Err(ProviderError::ApiKeyRequired { .. })));
    }

    #[test]
    fn download_writes_the_original_without_the_api_key() {
        let image = vec![7u8; 200_000];
        let server = TestServer::start(vec![
            (200, wallpaper_response("lygg6r")),
            (200, image.clone()),
        ]);
        let wallhaven = Wallhaven::with_api_url(server.url(), Some("secret".to_owned()));
        let mut wallpaper = wallhaven.wallpaper("lygg6r").unwrap();
        wallpaper.image_url = format!("{}/full/ly/wallhaven-lygg6r.jpg", server.url());

        let mut file = Vec::new();
        let mut reports = Vec::new();
        let written = wallhaven
            .download(&wallpaper, &mut file, &mut |received, total| {
                reports.push((received, total))
            })
            .unwrap();

        assert_eq!(written, 200_000);
        assert_eq!(file, image);
        assert_eq!(reports.first(), Some(&(0, Some(200_000))));
        assert_eq!(reports.last(), Some(&(200_000, Some(200_000))));
        let request = &server.requests()[1];
        assert_eq!(request.target, "/full/ly/wallhaven-lygg6r.jpg");
        assert_eq!(request.header("x-api-key"), None);
    }

    #[test]
    fn api_keys_are_verified_without_being_used() {
        let server = TestServer::start(vec![
            (200, br#"{"data":{"purity":["sfw"]}}"#.to_vec()),
            (404, br#"{"error":"Nothing here"}"#.to_vec()),
            (401, br#"{"error":"Unauthorized"}"#.to_vec()),
            (503, Vec::new()),
        ]);
        let wallhaven = Wallhaven::with_api_url(server.url(), None);

        assert!(wallhaven.verify_api_key("good").is_ok());
        assert!(matches!(
            wallhaven.verify_api_key("unknown"),
            Err(ProviderError::ApiKeyRejected { .. })
        ));
        assert!(matches!(
            wallhaven.verify_api_key("revoked"),
            Err(ProviderError::ApiKeyRejected { .. })
        ));
        assert!(matches!(
            wallhaven.verify_api_key("good"),
            Err(ProviderError::Status { status: 503, .. })
        ));
        assert!(!wallhaven.has_api_key());
        let requests = server.requests();
        assert_eq!(requests[0].target, "/settings");
        assert_eq!(requests[0].header("x-api-key"), Some("good"));
    }

    #[test]
    fn the_api_key_can_change_while_the_client_is_in_use() {
        let server = TestServer::start(vec![
            (200, search_response(&[])),
            (200, search_response(&[])),
        ]);
        let wallhaven = Wallhaven::with_api_url(server.url(), None);
        let nsfw = SearchQuery {
            ratings: vec![ContentRating::Nsfw],
            ..SearchQuery::default()
        };
        assert!(wallhaven.search(&nsfw).is_err());

        wallhaven.set_api_key(Some("secret".to_owned()));
        wallhaven.search(&nsfw).unwrap();
        wallhaven.set_api_key(Some(String::new()));
        assert!(!wallhaven.has_api_key());
        wallhaven.search(&SearchQuery::default()).unwrap();

        let requests = server.requests();
        assert_eq!(requests[0].header("x-api-key"), Some("secret"));
        assert_eq!(requests[1].header("x-api-key"), None);
    }

    #[test]
    fn requests_beyond_the_rate_limit_wait() {
        let start = Instant::now();
        let window = Duration::from_secs(60);
        let mut sent = VecDeque::new();
        let mut send = |at: Duration| {
            let now = start + at;
            let delay = rate_limit_delay(&mut sent, now, 3, window);
            sent.push_back(now + delay);
            delay
        };

        assert_eq!(send(Duration::ZERO), Duration::ZERO);
        assert_eq!(send(Duration::from_secs(1)), Duration::ZERO);
        assert_eq!(send(Duration::from_secs(2)), Duration::ZERO);
        // The fourth waits for the first to leave the window, the fifth for
        // the second.
        assert_eq!(send(Duration::from_secs(10)), Duration::from_secs(50));
        assert_eq!(send(Duration::from_secs(10)), Duration::from_secs(51));
        // Long after, the window is empty again.
        assert_eq!(send(Duration::from_secs(200)), Duration::ZERO);
    }

    #[test]
    fn file_names_follow_the_download_convention() {
        let wallhaven = Wallhaven::new(None);
        let id = |name| wallhaven.id_from_file_name(name);

        assert_eq!(id("wallhaven-94x38z.jpg").as_deref(), Some("94x38z"));
        assert_eq!(id("wallhaven-94x38z.png").as_deref(), Some("94x38z"));
        assert_eq!(id("wallhaven-94x38z"), None);
        assert_eq!(id("wallhaven-94x38z."), None);
        assert_eq!(id("wallhaven-.jpg"), None);
        assert_eq!(id("wallhaven-94X38Z.jpg"), None);
        assert_eq!(id("wallhaven-94x38z (1).jpg"), None);
        assert_eq!(id("94x38z.jpg"), None);
        assert_eq!(id("my-wallhaven-94x38z.jpg"), None);
    }

    #[test]
    fn ids_are_found_in_links_and_file_names() {
        let wallhaven = Wallhaven::new(None);
        let id = |text| wallhaven.parse_id(text);

        assert_eq!(id("94x38z").as_deref(), Some("94x38z"));
        assert_eq!(id(" 94x38z\n").as_deref(), Some("94x38z"));
        assert_eq!(
            id("https://wallhaven.cc/w/94x38z").as_deref(),
            Some("94x38z")
        );
        assert_eq!(
            id("wallhaven.cc/w/94x38z/?ref=x").as_deref(),
            Some("94x38z")
        );
        assert_eq!(id("https://whvn.cc/94x38z").as_deref(), Some("94x38z"));
        assert_eq!(
            id("https://w.wallhaven.cc/full/94/wallhaven-94x38z.jpg").as_deref(),
            Some("94x38z")
        );
        assert_eq!(
            id("/home/me/Pictures/wallhaven-94x38z.jpg").as_deref(),
            Some("94x38z")
        );
        assert_eq!(id("https://example.com/w/94x38z"), None);
        assert_eq!(id("mountain.jpg"), None);
        assert_eq!(id(""), None);
    }
}
