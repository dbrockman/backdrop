//! `backdrop wallhaven`: searching, describing, and downloading Wallhaven
//! wallpapers, and its API key.

use std::io::{self, BufRead as _, Write as _};
use std::path::{Path, PathBuf};

use anyhow::{anyhow, bail};
use backdrop_core::library::Downloaded;
use backdrop_core::paths::AppPaths;
use backdrop_core::providers::{
    ContentRating, Provider, ProviderError, RemoteWallpaper, SearchQuery, SearchSort, Wallhaven,
};
use backdrop_core::settings::Settings;
use clap::{Args, Subcommand, ValueEnum};

use super::OrientationArg;
use crate::format;

#[derive(Debug, Subcommand)]
pub enum WallhavenCommand {
    /// Search Wallhaven and print one wallpaper per line.
    Search(SearchArgs),
    /// Print the details and tags of a Wallhaven wallpaper.
    Info {
        /// A wallpaper id, a link to it, or a file name like wallhaven-94x38z.jpg.
        wallpaper: String,
    },
    /// Download wallpapers into the library, skipping any it already has.
    Download {
        /// Wallpaper ids, links to them, or file names like wallhaven-94x38z.jpg.
        #[arg(required = true)]
        wallpapers: Vec<String>,
        /// The library folder to save into, such as `dark`, instead of the one
        /// the download rules choose (see `backdrop rules`). Missing folders
        /// are created.
        #[arg(long, value_name = "FOLDER")]
        to: Option<PathBuf>,
    },
    /// Show whether an API key is set, or change it. NSFW wallpapers need one.
    ApiKey {
        #[command(subcommand)]
        command: Option<ApiKeyCommand>,
    },
}

#[derive(Debug, Subcommand)]
pub enum ApiKeyCommand {
    /// Store an API key, from your Wallhaven account settings.
    Set {
        /// The key; read from standard input if left out, which keeps it out
        /// of shell history.
        key: Option<String>,
    },
    /// Remove the stored API key.
    Clear,
}

#[derive(Debug, Args)]
pub struct SearchArgs {
    /// Words or tags to search for; Wallhaven's syntax, such as +tag and
    /// -tag, works. Without any, browse all wallpapers.
    query: Vec<String>,
    /// Content ratings to include. Safe for work only by default; nsfw needs
    /// an API key.
    #[arg(long = "rating", value_enum, value_delimiter = ',')]
    ratings: Vec<RatingArg>,
    /// Categories to include. All by default.
    #[arg(long = "category", value_enum, value_delimiter = ',')]
    categories: Vec<CategoryArg>,
    #[arg(long, value_enum)]
    orientation: Option<OrientationArg>,
    /// Minimum resolution, e.g. 2560x1440.
    #[arg(long, value_name = "WIDTHxHEIGHT", value_parser = parse_resolution)]
    at_least: Option<(u32, u32)>,
    /// Order of the results.
    #[arg(long, value_enum, default_value_t = SortArg::Newest)]
    sort: SortArg,
    /// The page of results, 24 wallpapers each.
    #[arg(long, default_value_t = 1, value_parser = clap::value_parser!(u32).range(1..))]
    page: u32,
    /// Continue a random search; the seed is printed with its first page.
    #[arg(long)]
    seed: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub enum RatingArg {
    Sfw,
    Sketchy,
    Nsfw,
}

impl From<RatingArg> for ContentRating {
    fn from(rating: RatingArg) -> Self {
        match rating {
            RatingArg::Sfw => Self::Sfw,
            RatingArg::Sketchy => Self::Sketchy,
            RatingArg::Nsfw => Self::Nsfw,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub enum CategoryArg {
    General,
    Anime,
    People,
}

impl CategoryArg {
    /// The provider's id for the category.
    fn id(self) -> &'static str {
        match self {
            Self::General => "general",
            Self::Anime => "anime",
            Self::People => "people",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub enum SortArg {
    /// Most recently uploaded first.
    Newest,
    /// Best match first.
    Relevance,
    /// Most viewed first.
    Views,
    /// Most favorited first.
    Favorites,
    /// Wallhaven's top list of the last month.
    Top,
    /// Random order.
    Random,
}

impl From<SortArg> for SearchSort {
    fn from(sort: SortArg) -> Self {
        match sort {
            SortArg::Newest => Self::Newest,
            SortArg::Relevance => Self::Relevance,
            SortArg::Views => Self::Views,
            SortArg::Favorites => Self::Favorites,
            SortArg::Top => Self::Top,
            SortArg::Random => Self::Random,
        }
    }
}

fn parse_resolution(text: &str) -> Result<(u32, u32), String> {
    let invalid = || format!("expected WIDTHxHEIGHT, such as 2560x1440, not {text:?}");
    let (width, height) = text.split_once(['x', 'X', '×']).ok_or_else(invalid)?;
    let width = width.trim().parse().map_err(|_| invalid())?;
    let height = height.trim().parse().map_err(|_| invalid())?;
    Ok((width, height))
}

impl SearchArgs {
    fn query(&self) -> SearchQuery {
        SearchQuery {
            text: self.query.join(" "),
            ratings: self.ratings.iter().map(|&rating| rating.into()).collect(),
            categories: self
                .categories
                .iter()
                .map(|category| category.id().to_owned())
                .collect(),
            orientation: self.orientation.map(Into::into),
            at_least: self.at_least,
            sort: self.sort.into(),
            page: self.page,
            seed: self.seed.clone(),
        }
    }
}

pub fn run(paths: &AppPaths, command: WallhavenCommand) -> anyhow::Result<()> {
    let settings_file = &paths.settings_file();
    match command {
        WallhavenCommand::Search(args) => search(&client(settings_file)?, &args.query()),
        WallhavenCommand::Info { wallpaper } => info(&client(settings_file)?, &wallpaper),
        WallhavenCommand::Download { wallpapers, to } => {
            download(paths, &wallpapers, to.as_deref())
        }
        WallhavenCommand::ApiKey { command: None } => {
            let set = Settings::load(settings_file)?.wallhaven_api_key().is_some();
            println!("{}", if set { "set" } else { "not set" });
            Ok(())
        }
        WallhavenCommand::ApiKey {
            command: Some(ApiKeyCommand::Set { key }),
        } => {
            let key = match key {
                Some(key) => key,
                None => read_key()?,
            };
            if key.trim().is_empty() {
                bail!("the API key is empty");
            }
            Settings::update(settings_file, |settings| {
                settings.set_wallhaven_api_key(Some(&key));
                Ok(())
            })?;
            Ok(())
        }
        WallhavenCommand::ApiKey {
            command: Some(ApiKeyCommand::Clear),
        } => {
            Settings::update(settings_file, |settings| {
                settings.set_wallhaven_api_key(None);
                Ok(())
            })?;
            Ok(())
        }
    }
}

fn read_key() -> anyhow::Result<String> {
    if io::IsTerminal::is_terminal(&io::stdin()) {
        eprint!("Wallhaven API key: ");
    }
    let mut key = String::new();
    io::stdin().lock().read_line(&mut key)?;
    Ok(key.trim().to_owned())
}

fn client(settings_file: &Path) -> anyhow::Result<Wallhaven> {
    let settings = Settings::load(settings_file)?;
    Ok(Wallhaven::new(
        settings.wallhaven_api_key().map(str::to_owned),
    ))
}

/// Adds how to fix an error to its message, where the CLI can help.
fn explain(error: ProviderError) -> anyhow::Error {
    match error {
        ProviderError::ApiKeyRequired { .. } | ProviderError::ApiKeyRejected { .. } => {
            anyhow::anyhow!("{error}; set one with `backdrop wallhaven api-key set`")
        }
        error => error.into(),
    }
}

/// Prints one line per wallpaper, then which page this is on standard error.
fn search(wallhaven: &Wallhaven, query: &SearchQuery) -> anyhow::Result<()> {
    let page = wallhaven.search(query).map_err(explain)?;
    let mut out = io::stdout().lock();
    for wallpaper in &page.wallpapers {
        let line = writeln!(out, "{}", search_line(wallpaper));
        if let Err(error) = line {
            return match error.kind() {
                io::ErrorKind::BrokenPipe => Ok(()),
                _ => Err(error.into()),
            };
        }
    }
    let mut footer = format!(
        "page {} of {}, {}",
        page.page,
        page.last_page.max(1),
        format::count(page.total as usize, "wallpaper", "wallpapers")
    );
    if let Some(seed) = &page.seed {
        footer.push_str(&format!("; continue with --seed {seed}"));
    }
    eprintln!("{footer}");
    Ok(())
}

/// `lygg6r  4928x3280  sfw      https://wallhaven.cc/w/lygg6r`
fn search_line(wallpaper: &RemoteWallpaper) -> String {
    format!(
        "{}  {:<9}  {:<7}  {}",
        wallpaper.id(),
        format!("{}x{}", wallpaper.width(), wallpaper.height()),
        wallpaper.rating().as_str(),
        wallpaper.page_url()
    )
}

/// Downloads each wallpaper in turn and prints where it is in the library.
/// All are looked up even if some fail; the command fails if any did.
fn download(paths: &AppPaths, wallpapers: &[String], to: Option<&Path>) -> anyhow::Result<()> {
    let wallhaven = client(&paths.settings_file())?;
    let ids = wallpapers
        .iter()
        .map(|text| {
            wallhaven.parse_id(text).ok_or_else(|| {
                anyhow!("{text} is not a Wallhaven wallpaper id, link, or file name")
            })
        })
        .collect::<anyhow::Result<Vec<_>>>()?;
    // Scanned first, so wallpapers already in the library are found.
    let (mut library, _) = super::scan(paths)?;
    let folder = match to {
        None => None,
        Some(folder) if folder.is_absolute() => match folder.strip_prefix(library.root()) {
            Ok(relative) => Some(relative.to_path_buf()),
            Err(_) => bail!(
                "{} is not inside the library folder {}",
                folder.display(),
                library.root().display()
            ),
        },
        Some(folder) => Some(folder.to_path_buf()),
    };

    let mut failed = 0;
    for id in &ids {
        let result = (|| -> anyhow::Result<Downloaded> {
            if let Some(path) = library.find_source(&wallhaven, id)? {
                return Ok(Downloaded::AlreadyInLibrary(path));
            }
            let wallpaper = wallhaven.wallpaper(id).map_err(explain)?;
            let mut progress = progress_line(id);
            let downloaded =
                library.download(&wallhaven, &wallpaper, folder.as_deref(), &mut progress);
            progress_done();
            Ok(downloaded?)
        })();
        match result {
            Ok(Downloaded::Saved(path)) => {
                println!("saved: {}", library.root().join(path).display())
            }
            Ok(Downloaded::AlreadyInLibrary(path)) => {
                println!(
                    "already in library: {}",
                    library.root().join(path).display()
                )
            }
            Err(error) => {
                eprintln!("error: {id}: {error:#}");
                failed += 1;
            }
        }
    }
    if failed > 0 {
        bail!("{failed} of {} downloads failed", ids.len());
    }
    Ok(())
}

/// Shows download progress on one line of a terminal; elsewhere, nothing.
fn progress_line(id: &str) -> impl FnMut(u64, Option<u64>) {
    let terminal = io::IsTerminal::is_terminal(&io::stderr());
    move |received, total| {
        if !terminal {
            return;
        }
        let status = match total {
            Some(total) if total > 0 => format!(
                "{}% of {}",
                received * 100 / total,
                format::file_size(total)
            ),
            _ => format::file_size(received),
        };
        eprint!("\r\x1b[Kdownloading {id}: {status}");
    }
}

/// Clears the line [`progress_line`] drew.
fn progress_done() {
    if io::IsTerminal::is_terminal(&io::stderr()) {
        eprint!("\r\x1b[K");
    }
}

fn info(wallhaven: &Wallhaven, text: &str) -> anyhow::Result<()> {
    let Some(id) = wallhaven.parse_id(text) else {
        bail!("{text} is not a Wallhaven wallpaper id, link, or file name");
    };
    let wallpaper = wallhaven.wallpaper(&id).map_err(explain)?;
    print!("{}", describe(&wallpaper));
    Ok(())
}

/// The lines `backdrop wallhaven info` prints.
fn describe(wallpaper: &RemoteWallpaper) -> String {
    let (width, height) = (wallpaper.width(), wallpaper.height());
    let orientation = super::orientation_name(wallpaper.orientation());
    let mut lines = vec![
        ("id", wallpaper.id().to_owned()),
        ("page", wallpaper.page_url().to_owned()),
        ("image", wallpaper.image_url().to_owned()),
        (
            "dimensions",
            format!(
                "{} ({}, {orientation})",
                format::dimensions(width, height),
                format::aspect_ratio(width, height),
            ),
        ),
    ];
    if let Some(size) = wallpaper.file_size() {
        lines.push(("file size", format::file_size(size)));
    }
    lines.push(("rating", wallpaper.rating().as_str().to_owned()));
    if let Some(category) = wallpaper.category() {
        lines.push(("category", category.to_owned()));
    }
    if let Some(uploaded) = wallpaper.uploaded() {
        lines.push(("uploaded", uploaded.to_string().replace('T', " ")));
    }
    if let Some(uploader) = wallpaper.uploader() {
        lines.push(("uploader", uploader.to_owned()));
    }
    if let Some(source) = wallpaper.source_url() {
        lines.push(("source", source.to_owned()));
    }
    if let Some(views) = wallpaper.views() {
        lines.push(("views", format::grouped(views as usize)));
    }
    if let Some(favorites) = wallpaper.favorites() {
        lines.push(("favorites", format::grouped(favorites as usize)));
    }
    if !wallpaper.colors().is_empty() {
        lines.push(("colors", wallpaper.colors().join(" ")));
    }
    if let Some(tags) = wallpaper.tags().filter(|tags| !tags.is_empty()) {
        let names: Vec<_> = tags.iter().map(|tag| tag.name.as_str()).collect();
        lines.push(("tags", names.join(", ")));
    }
    lines
        .into_iter()
        .map(|(label, value)| format!("{label:<12}{value}\n"))
        .collect()
}

#[cfg(test)]
mod tests {
    use clap::Parser as _;

    use super::super::{Args, Command};
    use super::*;

    fn search_args(args: &[&str]) -> SearchArgs {
        let args =
            Args::try_parse_from(["backdrop", "wallhaven", "search"].iter().chain(args)).unwrap();
        match args.command {
            Some(Command::Wallhaven {
                command: WallhavenCommand::Search(search),
            }) => search,
            command => panic!("not a search: {command:?}"),
        }
    }

    #[test]
    fn search_defaults_to_newest_safe_for_work_wallpapers() {
        let query = search_args(&[]).query();

        assert_eq!(query.text, "");
        assert_eq!(query.sort, SearchSort::Newest);
        assert_eq!(query.page, 1);
        assert!(query.categories.is_empty());
        assert_eq!(query.effective_ratings(), [ContentRating::Sfw]);
    }

    #[test]
    fn search_takes_words_and_filters() {
        let query = search_args(&[
            "mountain",
            "lake",
            "--rating",
            "sfw,sketchy",
            "--category",
            "general",
            "--orientation",
            "portrait",
            "--at-least",
            "2560x1440",
            "--sort",
            "top",
            "--page",
            "2",
        ])
        .query();

        assert_eq!(query.text, "mountain lake");
        assert_eq!(query.ratings, [ContentRating::Sfw, ContentRating::Sketchy]);
        assert_eq!(query.categories, ["general"]);
        assert_eq!(query.at_least, Some((2560, 1440)));
        assert_eq!(query.sort, SearchSort::Top);
        assert_eq!(query.page, 2);
    }

    #[test]
    fn search_rejects_bad_pages_and_resolutions() {
        let parse = |args: &[&str]| {
            Args::try_parse_from(["backdrop", "wallhaven", "search"].iter().chain(args))
        };
        assert!(parse(&["--page", "0"]).is_err());
        assert!(parse(&["--at-least", "2560"]).is_err());
        assert!(parse(&["--at-least", "wide x tall"]).is_err());
        assert!(parse(&["--rating", "explicit"]).is_err());
    }

    #[test]
    fn resolutions_accept_common_separators() {
        assert_eq!(parse_resolution("2560x1440"), Ok((2560, 1440)));
        assert_eq!(parse_resolution("2560X1440"), Ok((2560, 1440)));
        assert_eq!(parse_resolution("2560 × 1440"), Ok((2560, 1440)));
    }

    #[test]
    fn download_takes_wallpapers_and_a_folder() {
        let args = Args::try_parse_from([
            "backdrop",
            "wallhaven",
            "download",
            "94x38z",
            "https://whvn.cc/lygg6r",
            "--to",
            "dark",
        ])
        .unwrap();
        assert!(matches!(
            args.command,
            Some(Command::Wallhaven {
                command: WallhavenCommand::Download { wallpapers, to: Some(to) }
            }) if wallpapers.len() == 2 && to == Path::new("dark")
        ));
        assert!(Args::try_parse_from(["backdrop", "wallhaven", "download"]).is_err());
    }

    #[test]
    fn api_key_set_can_read_the_key_later() {
        let args = Args::try_parse_from(["backdrop", "wallhaven", "api-key", "set"]).unwrap();
        assert!(matches!(
            args.command,
            Some(Command::Wallhaven {
                command: WallhavenCommand::ApiKey {
                    command: Some(ApiKeyCommand::Set { key: None })
                }
            })
        ));
    }

    #[test]
    fn api_key_set_and_clear_change_the_settings() {
        let temp = tempfile::tempdir().unwrap();
        let paths = AppPaths::new(temp.path(), temp.path(), temp.path());
        let file = paths.settings_file();
        let api_key = |command| WallhavenCommand::ApiKey {
            command: Some(command),
        };

        run(
            &paths,
            api_key(ApiKeyCommand::Set {
                key: Some("abc123".to_owned()),
            }),
        )
        .unwrap();
        assert_eq!(
            Settings::load(&file).unwrap().wallhaven_api_key(),
            Some("abc123")
        );

        run(&paths, api_key(ApiKeyCommand::Clear)).unwrap();
        assert_eq!(Settings::load(&file).unwrap().wallhaven_api_key(), None);
    }
}
