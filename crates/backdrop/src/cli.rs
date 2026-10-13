//! Command-line interface. Commands run without starting GPUI.

use std::path::{Path, PathBuf};

use anyhow::bail;
use backdrop_core::index::ImageOrder;
use backdrop_core::library::{Library, LibraryScan};
use backdrop_core::paths::AppPaths;
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
    },
    /// Update the index and metadata with new, changed, moved, and removed images.
    Scan {
        /// Also remove metadata of missing images when there are many of them.
        #[arg(long)]
        remove_missing: bool,
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
    /// By path, A to Z.
    Path,
    /// Most pixels first.
    Resolution,
}

impl From<Sort> for ImageOrder {
    fn from(sort: Sort) -> Self {
        match sort {
            Sort::Added => Self::Added,
            Sort::Modified => Self::Modified,
            Sort::Path => Self::Path,
            Sort::Resolution => Self::Resolution,
        }
    }
}

pub fn run(command: Command) -> anyhow::Result<()> {
    let paths = AppPaths::from_env()?;
    match command {
        Command::Gui => unreachable!("the GUI is started by main"),
        Command::List { sort } => list(&paths, sort.into()),
        Command::Scan { remove_missing } => scan_command(&paths, remove_missing),
        Command::Library { command: None } => print_library(&paths.settings_file()),
        Command::Library {
            command: Some(LibraryCommand::Set { dir }),
        } => set_library(&paths.settings_file(), &dir),
        Command::Paths => {
            print_paths(&paths);
            Ok(())
        }
    }
}

/// Scans first, so the list is never stale. Scanning an unchanged library only
/// checks file sizes and modification times.
fn list(paths: &AppPaths, order: ImageOrder) -> anyhow::Result<()> {
    let (library, _) = scan(paths)?;
    for image in library.images(order)? {
        println!("{}", library.root().join(image.path()).display());
    }
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
    fn list_sorts_by_date_added_by_default() {
        let args = Args::try_parse_from(["backdrop", "list"]).unwrap();
        assert!(matches!(
            args.command,
            Some(Command::List { sort: Sort::Added })
        ));
    }
}
