//! Command-line interface. Commands run without starting GPUI.

use std::io::{self, Write as _};
use std::path::{Path, PathBuf};

use anyhow::bail;
use backdrop_core::desktop::WallpaperBackend;
use backdrop_core::image::Orientation;
use backdrop_core::library::{Library, LibraryScan, SystemTrash};
use backdrop_core::paths::AppPaths;
use backdrop_core::query::{ImageFilter, ImageOrder};
use backdrop_core::settings::{LibraryStatus, Settings};
use clap::{Parser, Subcommand, ValueEnum};

/// Browse, organize, download, and manage wallpapers.
///
/// Run without a command to open the graphical interface.
#[derive(Debug, Parser)]
#[command(version)]
pub struct Args {
    #[command(subcommand)]
    pub command: Option<Command>,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Open the graphical interface.
    Gui,
    /// Print the paths of the images in the library.
    List {
        /// Order of the images.
        #[arg(long, value_enum, default_value_t = Sort::Added)]
        sort: Sort,
        /// Only list images with this orientation.
        #[arg(long, value_enum)]
        orientation: Option<OrientationArg>,
        /// Only list favorites.
        #[arg(long)]
        favorites: bool,
    },
    /// Move images to the trash and remove them from the library.
    Delete {
        #[arg(required = true)]
        images: Vec<PathBuf>,
    },
    /// Mark images as favorites.
    Favorite {
        #[arg(required = true)]
        images: Vec<PathBuf>,
    },
    /// Remove images from favorites.
    Unfavorite {
        #[arg(required = true)]
        images: Vec<PathBuf>,
    },
    /// Update the index and metadata with new, changed, moved, and removed images.
    Scan {
        /// Also remove metadata of missing images when there are many of them.
        #[arg(long)]
        remove_missing: bool,
    },
    /// Print the path of the current desktop wallpaper.
    Current,
    /// Show an image as the desktop wallpaper.
    Set {
        /// The image to show; it must be in the library.
        #[arg(required_unless_present = "random", conflicts_with = "random")]
        image: Option<PathBuf>,
        /// Show a random image from the library, other than the current one.
        #[arg(long)]
        random: bool,
        /// With --random, only pick images with this orientation.
        #[arg(long, value_enum, requires = "random")]
        orientation: Option<OrientationArg>,
        /// With --random, only pick favorites.
        #[arg(long, requires = "random")]
        favorites: bool,
    },
    /// Print the library folder, or change it.
    Library {
        #[command(subcommand)]
        command: Option<LibraryCommand>,
    },
    /// Print where Backdrop stores its settings, index, and thumbnails.
    Paths,
}

#[derive(Debug, Subcommand)]
pub enum LibraryCommand {
    /// Use DIR as the library folder.
    Set { dir: PathBuf },
}

#[derive(Clone, Copy, Debug, ValueEnum)]
pub enum Sort {
    /// Most recently added first.
    Added,
    /// Most recently modified first.
    Modified,
    /// By file name, A to Z.
    Name,
    /// By path, A to Z.
    Path,
    /// Most pixels first.
    Resolution,
}

#[derive(Clone, Copy, Debug, ValueEnum)]
pub enum OrientationArg {
    Landscape,
    Portrait,
    Square,
}

impl From<OrientationArg> for Orientation {
    fn from(orientation: OrientationArg) -> Self {
        match orientation {
            OrientationArg::Landscape => Self::Landscape,
            OrientationArg::Portrait => Self::Portrait,
            OrientationArg::Square => Self::Square,
        }
    }
}

impl From<Sort> for ImageOrder {
    fn from(sort: Sort) -> Self {
        match sort {
            Sort::Added => Self::Added,
            Sort::Modified => Self::Modified,
            Sort::Name => Self::Name,
            Sort::Path => Self::Path,
            Sort::Resolution => Self::Resolution,
        }
    }
}

pub fn run(command: Command) -> anyhow::Result<()> {
    let paths = AppPaths::from_env()?;
    match command {
        Command::Gui => unreachable!("the GUI is started by main"),
        Command::List {
            sort,
            orientation,
            favorites,
        } => {
            let filter = ImageFilter::default()
                .with_orientation(orientation.map(Into::into))
                .with_favorites_only(favorites);
            list(&paths, sort.into(), &filter)
        }
        Command::Delete { images } => delete(&paths, &images),
        Command::Favorite { images } => set_favorite(&paths, &images, true),
        Command::Unfavorite { images } => set_favorite(&paths, &images, false),
        Command::Scan { remove_missing } => scan_command(&paths, remove_missing),
        Command::Library { command: None } => print_library(&paths.settings_file()),
        Command::Library {
            command: Some(LibraryCommand::Set { dir }),
        } => set_library(&paths.settings_file(), &dir),
        Command::Current => {
            match desktop()?.current_wallpaper()? {
                Some(path) => println!("{}", path.display()),
                None => bail!("no wallpaper is set"),
            }
            Ok(())
        }
        Command::Set {
            image,
            random: _,
            orientation,
            favorites,
        } => {
            if image.is_some() && (favorites || orientation.is_some()) {
                bail!("--orientation and --favorites only apply with --random");
            }
            let filter = ImageFilter::default()
                .with_orientation(orientation.map(Into::into))
                .with_favorites_only(favorites);
            set_wallpaper(&paths, image.as_deref(), &filter)
        }
        Command::Paths => {
            print_paths(&paths);
            Ok(())
        }
    }
}

/// Scans first, so the list is never stale. Scanning an unchanged library only
/// checks file sizes and modification times.
fn list(paths: &AppPaths, order: ImageOrder, filter: &ImageFilter) -> anyhow::Result<()> {
    let (library, _) = scan(paths)?;
    let favorites = library.favorites()?;
    let mut out = io::stdout().lock();
    for image in library.images(order)? {
        if !filter.matches(&image, &favorites) {
            continue;
        }
        let line = writeln!(out, "{}", library.root().join(image.path()).display());
        // The reader stopped early, as `backdrop list | head` does.
        if let Err(error) = line {
            return match error.kind() {
                io::ErrorKind::BrokenPipe => Ok(()),
                _ => Err(error.into()),
            };
        }
    }
    Ok(())
}

/// Moves images to the trash. All paths are checked before any is moved.
fn delete(paths: &AppPaths, images: &[PathBuf]) -> anyhow::Result<()> {
    let (mut library, _) = scan(paths)?;
    let indexed = library.images(ImageOrder::Path)?;
    let images = images
        .iter()
        .map(|image| {
            let path = library.relative_path(image)?;
            if !indexed.iter().any(|indexed| indexed.path() == path) {
                bail!("{} is not an image in the library", image.display());
            }
            Ok(path)
        })
        .collect::<anyhow::Result<Vec<_>>>()?;
    for image in images {
        library.delete_image(&image, &SystemTrash)?;
        println!("moved to trash: {}", library.root().join(&image).display());
    }
    Ok(())
}

/// Scans first, so an image added since the last scan can be marked. All
/// paths are checked before any is changed.
fn set_favorite(paths: &AppPaths, images: &[PathBuf], favorite: bool) -> anyhow::Result<()> {
    let (mut library, _) = scan(paths)?;
    let images = images
        .iter()
        .map(|image| library.relative_path(image))
        .collect::<Result<Vec<_>, _>>()?;
    for image in images {
        library.set_favorite(&image, favorite)?;
    }
    Ok(())
}

fn desktop() -> anyhow::Result<Box<dyn WallpaperBackend>> {
    match backdrop_core::desktop::detect() {
        Some(backend) => Ok(backend),
        None => bail!("no supported desktop shell is running; Backdrop works with Noctalia"),
    }
}

/// Shows `image`, or a random library image matching `filter`, and prints its
/// path. Scans first, so new images can be shown and chosen.
fn set_wallpaper(
    paths: &AppPaths,
    image: Option<&Path>,
    filter: &ImageFilter,
) -> anyhow::Result<()> {
    let desktop = desktop()?;
    let (library, _) = scan(paths)?;
    let images = library.images(ImageOrder::Path)?;
    let path = match image {
        Some(image) => {
            let path = library.relative_path(image)?;
            if !images.iter().any(|indexed| indexed.path() == path) {
                bail!("{} is not an image in the library", image.display());
            }
            path
        }
        None => {
            let favorites = library.favorites()?;
            let current = desktop
                .current_wallpaper()
                .ok()
                .flatten()
                .and_then(|current| library.relative_path(&current).ok());
            let candidates: Vec<_> = images
                .iter()
                .filter(|image| filter.matches(image, &favorites))
                .filter(|image| Some(image.path()) != current.as_deref())
                .collect();
            if candidates.is_empty() {
                bail!("no other images in the library match");
            }
            candidates[fastrand::usize(..candidates.len())]
                .path()
                .to_path_buf()
        }
    };
    let file = library.root().join(&path);
    desktop.set_wallpaper(&file)?;
    println!("{}", file.display());
    Ok(())
}

fn scan_command(paths: &AppPaths, remove_missing: bool) -> anyhow::Result<()> {
    let (mut library, scan) = scan(paths)?;
    let index = scan.index();
    println!(
        "{} images: {} added, {} updated, {} removed",
        index.total(),
        index.added(),
        index.updated(),
        index.removed()
    );
    let Ok(metadata) = scan.metadata() else {
        return Ok(());
    };
    for (from, to) in metadata.moved() {
        println!("metadata moved: {} -> {}", from.display(), to.display());
    }
    for path in metadata.removed() {
        println!("metadata removed: {}", path.display());
    }
    let unconfirmed = metadata.unconfirmed_removals();
    if remove_missing && !unconfirmed.is_empty() {
        let removed = library.remove_missing(unconfirmed)?;
        println!("metadata removed for {removed} missing images");
    } else if !unconfirmed.is_empty() {
        eprintln!(
            "warning: {} images with metadata are missing; their metadata was kept. \
             Run `backdrop scan --remove-missing` to remove it.",
            unconfirmed.len()
        );
    }
    Ok(())
}

/// Scans the library, printing any files that could not be indexed and any
/// metadata problem.
fn scan(paths: &AppPaths) -> anyhow::Result<(Library, LibraryScan)> {
    let root = library_root(&paths.settings_file())?;
    let mut library = Library::open(&root, &paths.database_file())?;
    let scan = library.scan()?;
    for issue in scan.index().issues() {
        eprintln!(
            "warning: skipped {}: {}",
            root.join(issue.path()).display(),
            issue.message()
        );
    }
    if let Err(error) = scan.metadata() {
        eprintln!("warning: metadata not updated: {error}");
    }
    Ok((library, scan))
}

fn library_root(settings_file: &Path) -> anyhow::Result<PathBuf> {
    match Settings::load(settings_file)?.library_status() {
        LibraryStatus::NotChosen => {
            bail!("no library folder is set; choose one with `backdrop library set <DIR>`")
        }
        LibraryStatus::Missing(dir) => bail!("library folder {} not found", dir.display()),
        LibraryStatus::Available(dir) => Ok(dir),
    }
}

fn print_library(settings_file: &Path) -> anyhow::Result<()> {
    println!("{}", library_root(settings_file)?.display());
    Ok(())
}

fn set_library(settings_file: &Path, dir: &Path) -> anyhow::Result<()> {
    let (_, dir) = Settings::update(settings_file, |settings| settings.set_library_dir(dir))?;
    println!("{}", dir.display());
    Ok(())
}

fn print_paths(paths: &AppPaths) {
    println!("settings    {}", paths.settings_file().display());
    println!("database    {}", paths.database_file().display());
    println!("thumbnails  {}", paths.thumbnails_dir().display());
}

#[cfg(test)]
mod tests {
    use clap::CommandFactory as _;

    use super::*;

    #[test]
    fn cli_definition_is_valid() {
        Args::command().debug_assert();
    }

    #[test]
    fn no_command_opens_the_gui() {
        let args = Args::try_parse_from(["backdrop"]).unwrap();
        assert!(args.command.is_none());
    }

    #[test]
    fn library_set_takes_a_folder() {
        let args = Args::try_parse_from(["backdrop", "library", "set", "Wallpapers"]).unwrap();
        assert!(matches!(
            args.command,
            Some(Command::Library {
                command: Some(LibraryCommand::Set { dir })
            }) if dir == Path::new("Wallpapers")
        ));
    }

    #[test]
    fn set_takes_an_image_or_random() {
        assert!(Args::try_parse_from(["backdrop", "set", "a.jpg"]).is_ok());
        assert!(Args::try_parse_from(["backdrop", "set", "--random", "--favorites"]).is_ok());
        assert!(Args::try_parse_from(["backdrop", "set"]).is_err());
        assert!(Args::try_parse_from(["backdrop", "set", "a.jpg", "--random"]).is_err());
    }

    #[test]
    fn list_sorts_by_date_added_by_default() {
        let args = Args::try_parse_from(["backdrop", "list"]).unwrap();
        assert!(matches!(
            args.command,
            Some(Command::List {
                sort: Sort::Added,
                orientation: None,
                favorites: false
            })
        ));
    }
}
