//! Command-line interface. Commands run without starting GPUI.

use std::io::{self, Write as _};
use std::path::{Path, PathBuf};

use anyhow::bail;
use backdrop_core::desktop::WallpaperBackend;
use backdrop_core::image::{ImageFile, Orientation};
use backdrop_core::library::{Library, LibraryScan, SystemTrash};
use backdrop_core::paths::AppPaths;
use backdrop_core::query::{ImageFilter, ImageOrder};
use backdrop_core::settings::{LibraryStatus, Settings};
use clap::{Parser, Subcommand, ValueEnum};

use crate::format;

mod wallhaven;

use wallhaven::WallhavenCommand;

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
    /// Print the details of an image in the library.
    Info { image: PathBuf },
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
    /// Search and describe wallpapers on Wallhaven.
    Wallhaven {
        #[command(subcommand)]
        command: WallhavenCommand,
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
        Command::Info { image } => info(&paths, &image),
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
        Command::Wallhaven { command } => wallhaven::run(&paths.settings_file(), command),
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

/// Prints the image's details, as the detail view shows them. Scans first, so
/// a new or changed image is described correctly. Whether the image is the
/// current wallpaper is printed only when a supported desktop shell is running.
fn info(paths: &AppPaths, image: &Path) -> anyhow::Result<()> {
    let (library, _) = scan(paths)?;
    let images = library.images(ImageOrder::Path)?;
    let image = find_image(&library, &images, image)?;
    let favorite = library.favorites()?.contains(image.path());
    let file = library.root().join(image.path());
    let wallpaper = backdrop_core::desktop::detect().map(|desktop| {
        desktop
            .current_wallpaper()
            .ok()
            .flatten()
            .is_some_and(|current| current == file)
    });
    print!("{}", describe(image, &file, favorite, wallpaper));
    Ok(())
}

/// The lines `backdrop info` prints for `image`, whose absolute path is `file`.
fn describe(image: &ImageFile, file: &Path, favorite: bool, wallpaper: Option<bool>) -> String {
    let yes_no = |value: bool| if value { "yes" } else { "no" };
    let orientation = orientation_name(image.orientation());
    let mut lines = vec![
        ("path", file.display().to_string()),
        (
            "dimensions",
            format!(
                "{} ({}, {orientation})",
                format::dimensions(image.width(), image.height()),
                format::aspect_ratio(image.width(), image.height()),
            ),
        ),
        ("format", format::image_format(image.format()).to_owned()),
        ("file size", format::file_size(image.size())),
        ("added", format::date_time(image.added())),
        ("modified", format::date_time(image.modified())),
        ("favorite", yes_no(favorite).to_owned()),
    ];
    if let Some(wallpaper) = wallpaper {
        lines.push(("wallpaper", yes_no(wallpaper).to_owned()));
    }
    lines
        .into_iter()
        .map(|(label, value)| format!("{label:<12}{value}\n"))
        .collect()
}

/// How `info` commands name an orientation.
fn orientation_name(orientation: Orientation) -> &'static str {
    match orientation {
        Orientation::Landscape => "landscape",
        Orientation::Portrait => "portrait",
        Orientation::Square => "square",
    }
}

/// The indexed image at `image`, a path given on the command line.
fn find_image<'a>(
    library: &Library,
    images: &'a [ImageFile],
    image: &Path,
) -> anyhow::Result<&'a ImageFile> {
    let path = library.relative_path(image)?;
    match images.iter().find(|indexed| indexed.path() == path) {
        Some(indexed) => Ok(indexed),
        None => bail!("{} is not an image in the library", image.display()),
    }
}

/// Moves images to the trash. All paths are checked before any is moved.
fn delete(paths: &AppPaths, images: &[PathBuf]) -> anyhow::Result<()> {
    let (mut library, _) = scan(paths)?;
    let indexed = library.images(ImageOrder::Path)?;
    let images = images
        .iter()
        .map(|image| Ok(find_image(&library, &indexed, image)?.path().to_path_buf()))
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
        Some(image) => find_image(&library, &images, image)?.path().to_path_buf(),
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
    fn info_takes_one_image() {
        assert!(Args::try_parse_from(["backdrop", "info", "a.jpg"]).is_ok());
        assert!(Args::try_parse_from(["backdrop", "info"]).is_err());
        assert!(Args::try_parse_from(["backdrop", "info", "a.jpg", "b.jpg"]).is_err());
    }

    /// A scanned library holding `a.png`, 1920 × 1080, and the images in it.
    fn library() -> (tempfile::TempDir, Library, Vec<ImageFile>) {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("library");
        std::fs::create_dir(&root).unwrap();
        image::RgbImage::new(1920, 1080)
            .save(root.join("a.png"))
            .unwrap();
        let mut library = Library::open(&root, &dir.path().join("index.sqlite")).unwrap();
        library.scan().unwrap();
        let images = library.images(ImageOrder::Path).unwrap();
        (dir, library, images)
    }

    #[test]
    fn find_image_accepts_only_indexed_images() {
        let (_dir, library, images) = library();
        let found = find_image(&library, &images, &library.root().join("a.png")).unwrap();
        assert_eq!(found.path(), Path::new("a.png"));
        let notes = library.root().join("notes.txt");
        std::fs::write(&notes, "not an image").unwrap();
        let error = find_image(&library, &images, &notes).unwrap_err();
        assert!(error.to_string().contains("is not an image in the library"));
    }

    #[test]
    fn describe_prints_the_image_details() {
        let (_dir, library, images) = library();
        let file = library.root().join("a.png");
        let text = describe(&images[0], &file, true, None);
        let lines: Vec<_> = text.lines().collect();
        assert_eq!(lines[0], format!("path        {}", file.display()));
        assert_eq!(lines[1], "dimensions  1,920 × 1,080 (16:9, landscape)");
        assert_eq!(lines[2], "format      PNG");
        assert!(lines[3].starts_with("file size   "));
        assert!(lines[4].starts_with("added       "));
        assert!(lines[5].starts_with("modified    "));
        assert_eq!(lines[6], "favorite    yes");
        // Without a desktop shell, whether it is the wallpaper is unknown.
        assert_eq!(lines.len(), 7);

        let text = describe(&images[0], &file, false, Some(false));
        assert!(text.contains("favorite    no\n"));
        assert!(text.ends_with("wallpaper   no\n"));
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
