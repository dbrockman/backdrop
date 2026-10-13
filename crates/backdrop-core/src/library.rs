//! A library folder: its index and its metadata, kept consistent with the
//! files.
//!
//! Users may rename, move, and delete images outside Backdrop. Each scan
//! updates the index, then reconciles the metadata records with the files
//! (spec §5.3): a record whose file is gone follows the file to its new path
//! if an untracked image with the same content hash appears, and is otherwise
//! removed. Removing a large share of the records at once needs confirmation
//! through [`Library::remove_missing`].
//!
//! Files are hashed only when a hash is needed: when a record is created, when
//! a file with a record changes, and when an untracked file of the right size
//! could be the new location of a missing one.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};

use crate::image::ImageFile;
use crate::index::{Index, IndexError, ScanSummary};
use crate::metadata::{self, ImageMetadata, LibraryMetadata, MetadataError};
use crate::query::ImageOrder;

/// Removals above both limits in one scan need confirmation.
const CONFIRM_REMOVALS_FROM_COUNT: usize = 20;
const CONFIRM_REMOVALS_FROM_SHARE: f64 = 0.25;

#[derive(Debug, thiserror::Error)]
pub enum LibraryError {
    #[error(transparent)]
    Index(IndexError),
    #[error(transparent)]
    Metadata(MetadataError),
    #[error("{} is not an image in the library", path.display())]
    NotInLibrary { path: PathBuf },
    #[error("could not move {} to the trash: {message}", path.display())]
    Trash { path: PathBuf, message: String },
    #[error("{} is not inside the library folder {}", path.display(), root.display())]
    OutsideLibrary { path: PathBuf, root: PathBuf },
    #[error("could not find {}: {cause}", path.display())]
    NotFound {
        path: PathBuf,
        cause: std::io::Error,
    },
}

impl From<IndexError> for LibraryError {
    fn from(error: IndexError) -> Self {
        Self::Index(error)
    }
}

impl From<MetadataError> for LibraryError {
    fn from(error: MetadataError) -> Self {
        Self::Metadata(error)
    }
}

/// The result of [`Library::scan`].
#[derive(Debug)]
pub struct LibraryScan {
    index: ScanSummary,
    metadata: Result<Reconciliation, LibraryError>,
}

impl LibraryScan {
    pub fn index(&self) -> &ScanSummary {
        &self.index
    }

    /// How the metadata was brought in line with the files. The index is
    /// updated even when this fails.
    pub fn metadata(&self) -> Result<&Reconciliation, &LibraryError> {
        self.metadata.as_ref()
    }
}

/// What reconciling the metadata with the files changed.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Reconciliation {
    moved: Vec<(PathBuf, PathBuf)>,
    removed: Vec<PathBuf>,
    unconfirmed_removals: Vec<PathBuf>,
}

impl Reconciliation {
    /// Records that followed their image to a new path, as `(from, to)`.
    pub fn moved(&self) -> &[(PathBuf, PathBuf)] {
        &self.moved
    }

    /// Records removed because their image was deleted.
    pub fn removed(&self) -> &[PathBuf] {
        &self.removed
    }

    /// Records whose image is missing, kept because removing this many at once
    /// needs confirmation. Pass them to [`Library::remove_missing`] to remove
    /// them.
    pub fn unconfirmed_removals(&self) -> &[PathBuf] {
        &self.unconfirmed_removals
    }
}

/// Where deleted images go.
pub trait Trash: Send + Sync {
    /// Moves `file` out of the library so the user can still restore it.
    fn trash(&self, file: &Path) -> Result<(), String>;
}

/// The desktop's trash (the freedesktop.org trash on Linux), from which file
/// managers can restore files.
pub struct SystemTrash;

impl Trash for SystemTrash {
    fn trash(&self, file: &Path) -> Result<(), String> {
        trash::delete(file).map_err(|error| error.to_string())
    }
}

/// A library folder and a connection to its index. Open one per thread.
pub struct Library {
    root: PathBuf,
    index: Index,
}

impl Library {
    pub fn open(root: &Path, database_file: &Path) -> Result<Self, LibraryError> {
        Ok(Self::with_index(root, Index::open(database_file)?))
    }

    pub fn with_index(root: &Path, index: Index) -> Self {
        Self {
            root: root.to_path_buf(),
            index,
        }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Brings the index and the metadata up to date with the files.
    pub fn scan(&mut self) -> Result<LibraryScan, LibraryError> {
        let index = self.index.scan(&self.root)?;
        let metadata = self.reconcile(&index);
        Ok(LibraryScan { index, metadata })
    }

    pub fn images(&self, order: ImageOrder) -> Result<Vec<ImageFile>, LibraryError> {
        Ok(self.index.images(order)?)
    }

    pub fn metadata(&self) -> Result<LibraryMetadata, LibraryError> {
        Ok(LibraryMetadata::load(&self.root)?)
    }

    /// Paths of the images marked as favorites.
    pub fn favorites(&self) -> Result<HashSet<PathBuf>, LibraryError> {
        Ok(self
            .metadata()?
            .records()
            .filter(|(_, record)| record.is_favorite())
            .map(|(path, _)| path)
            .collect())
    }

    pub fn set_favorite(&mut self, path: &Path, favorite: bool) -> Result<(), LibraryError> {
        self.update_image(path, |record| record.set_favorite(favorite))
    }

    /// Turns a path given by the user, absolute or relative to the working
    /// directory, into a path relative to the library root.
    pub fn relative_path(&self, path: &Path) -> Result<PathBuf, LibraryError> {
        let absolute = fs::canonicalize(path).map_err(|cause| LibraryError::NotFound {
            path: path.to_path_buf(),
            cause,
        })?;
        absolute
            .strip_prefix(&self.root)
            .ok()
            .filter(|relative| !relative.as_os_str().is_empty())
            .map(Path::to_path_buf)
            .ok_or_else(|| LibraryError::OutsideLibrary {
                path: path.to_path_buf(),
                root: self.root.clone(),
            })
    }

    /// Changes the metadata of the indexed image at `path`, creating its record
    /// (and hashing the file) if it has none. A record left with nothing but
    /// its hash is removed.
    pub fn update_image(
        &mut self,
        path: &Path,
        change: impl FnOnce(&mut ImageMetadata),
    ) -> Result<(), LibraryError> {
        let not_in_library = || LibraryError::NotInLibrary {
            path: path.to_path_buf(),
        };
        let existing = LibraryMetadata::load(&self.root)?.get(path).cloned();
        let mut record = match existing {
            Some(record) => record,
            None => ImageMetadata::new(
                self.index
                    .hash(&self.root, path)?
                    .ok_or_else(not_in_library)?,
            ),
        };
        change(&mut record);
        LibraryMetadata::update(&self.root, |metadata| {
            if record.is_empty() {
                metadata.remove(path);
                Ok(())
            } else {
                metadata.insert(path, record)
            }
        })?;
        Ok(())
    }

    /// Removes the records for `paths` whose images are still missing, and
    /// returns how many were removed.
    /// Moves the indexed image at `path` to `trash` and forgets it: its index
    /// entry, its metadata record, and a provider snapshot nothing else uses.
    ///
    /// Once the file is in the trash this succeeds even if the metadata can't
    /// be updated; the next scan removes the record of the missing image.
    pub fn delete_image(&mut self, path: &Path, trash: &dyn Trash) -> Result<(), LibraryError> {
        if !self.indexed_sizes()?.contains_key(path) {
            return Err(LibraryError::NotInLibrary {
                path: path.to_path_buf(),
            });
        }
        let file = self.root.join(path);
        trash.trash(&file).map_err(|message| LibraryError::Trash {
            path: file,
            message,
        })?;
        self.index.remove(path)?;
        if let Ok((metadata, Some(record))) =
            LibraryMetadata::update(&self.root, |metadata| Ok(metadata.remove(path)))
        {
            remove_orphaned_snapshots(&self.root, &metadata, &[record]);
        }
        Ok(())
    }

    pub fn remove_missing(&mut self, paths: &[PathBuf]) -> Result<usize, LibraryError> {
        let indexed: HashSet<PathBuf> = self.indexed_sizes()?.into_keys().collect();
        let root = self.root.clone();
        let (metadata, removed) = LibraryMetadata::update(&root, |metadata| {
            let mut removed = Vec::new();
            for path in paths {
                let missing =
                    !indexed.contains(path) && fs::symlink_metadata(root.join(path)).is_err();
                if missing && let Some(record) = metadata.remove(path) {
                    removed.push(record);
                }
            }
            Ok(removed)
        })?;
        remove_orphaned_snapshots(&self.root, &metadata, &removed);
        Ok(removed.len())
    }

    fn reconcile(&mut self, scan: &ScanSummary) -> Result<Reconciliation, LibraryError> {
        let metadata = LibraryMetadata::load(&self.root)?;
        if metadata.is_empty() {
            return Ok(Reconciliation::default());
        }
        let indexed = self.indexed_sizes()?;
        let tracked: HashSet<PathBuf> = metadata.records().map(|(path, _)| path).collect();

        // Records whose file is still here, edited or not, and those whose
        // file is gone.
        let mut rehashed = Vec::new();
        let mut missing = Vec::new();
        for (path, record) in metadata.records() {
            if indexed.contains_key(&path) {
                match self.index.hash(&self.root, &path) {
                    Ok(Some(hash)) if hash != record.hash() => {
                        rehashed.push((path, record.hash().to_owned(), hash));
                    }
                    // Unreadable right now; leave the record as it is.
                    Ok(_) | Err(IndexError::Hash { .. }) => {}
                    Err(error) => return Err(error.into()),
                }
            } else if !scan.may_exist(&path) && fs::symlink_metadata(self.root.join(&path)).is_err()
            {
                missing.push((path, record.hash().to_owned()));
            }
        }

        // Look for each missing image among untracked images of the size it
        // had. Without a known size, any untracked image may be it.
        let mut untracked: Vec<(&PathBuf, u64)> = indexed
            .iter()
            .filter(|(path, _)| !tracked.contains(*path))
            .map(|(path, size)| (path, *size))
            .collect();
        untracked.sort();
        let mut claimed = HashSet::new();
        let mut moved = Vec::new();
        let mut unmatched = Vec::new();
        for (path, hash) in missing {
            let size = scan.removed_sizes.get(&path).copied();
            let mut found = None;
            for &(candidate, candidate_size) in &untracked {
                if claimed.contains(candidate) || size.is_some_and(|size| size != candidate_size) {
                    continue;
                }
                match self.index.hash(&self.root, candidate) {
                    Ok(Some(candidate_hash)) if candidate_hash == hash => {
                        found = Some(candidate.clone());
                        break;
                    }
                    Ok(_) | Err(IndexError::Hash { .. }) => {}
                    Err(error) => return Err(error.into()),
                }
            }
            match found {
                Some(to) => {
                    claimed.insert(to.clone());
                    moved.push((path, hash, to));
                }
                None => unmatched.push((path, hash)),
            }
        }

        let needs_confirmation = unmatched.len() >= CONFIRM_REMOVALS_FROM_COUNT
            && unmatched.len() as f64 >= metadata.len() as f64 * CONFIRM_REMOVALS_FROM_SHARE;
        let (removals, unconfirmed) = if needs_confirmation {
            (Vec::new(), unmatched)
        } else {
            (unmatched, Vec::new())
        };

        let mut reconciliation = Reconciliation {
            unconfirmed_removals: unconfirmed.into_iter().map(|(path, _)| path).collect(),
            ..Reconciliation::default()
        };
        if rehashed.is_empty() && moved.is_empty() && removals.is_empty() {
            return Ok(reconciliation);
        }

        // Apply to a fresh copy, skipping any record another process changed
        // since it was read.
        let unchanged = |metadata: &LibraryMetadata, path: &Path, hash: &str| {
            metadata
                .get(path)
                .is_some_and(|record| record.hash() == hash)
        };
        let (metadata, removed_records) = LibraryMetadata::update(&self.root, |metadata| {
            for (path, old, new) in rehashed {
                if unchanged(metadata, &path, &old) {
                    let mut record = metadata.remove(&path).expect("checked above");
                    record.set_hash(new);
                    metadata.insert(&path, record)?;
                }
            }
            for (from, hash, to) in moved {
                if unchanged(metadata, &from, &hash) && metadata.get(&to).is_none() {
                    let record = metadata.remove(&from).expect("checked above");
                    metadata.insert(&to, record)?;
                    reconciliation.moved.push((from, to));
                }
            }
            let mut removed_records = Vec::new();
            for (path, hash) in removals {
                if unchanged(metadata, &path, &hash) {
                    removed_records.push(metadata.remove(&path).expect("checked above"));
                    reconciliation.removed.push(path);
                }
            }
            Ok(removed_records)
        })?;
        remove_orphaned_snapshots(&self.root, &metadata, &removed_records);
        Ok(reconciliation)
    }

    fn indexed_sizes(&self) -> Result<HashMap<PathBuf, u64>, LibraryError> {
        Ok(self
            .index
            .images(ImageOrder::Path)?
            .into_iter()
            .map(|image| (image.path().to_path_buf(), image.size()))
            .collect())
    }
}

/// Deletes the provider snapshots of removed records that no remaining record
/// refers to. A snapshot that can't be deleted is left for a later scan.
fn remove_orphaned_snapshots(root: &Path, metadata: &LibraryMetadata, removed: &[ImageMetadata]) {
    for source in removed.iter().filter_map(ImageMetadata::source) {
        if !metadata.references_source(source)
            && let Some(file) = metadata::snapshot_file(root, source)
        {
            let _ = fs::remove_file(file);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::test_images::png;

    struct Fixture {
        dir: tempfile::TempDir,
        library: Library,
    }

    impl Fixture {
        fn new() -> Self {
            let dir = tempfile::tempdir().unwrap();
            let library = Library::with_index(dir.path(), Index::in_memory().unwrap());
            Self { dir, library }
        }

        fn root(&self) -> &Path {
            self.dir.path()
        }

        fn write(&self, path: &str, contents: &[u8]) {
            let path = self.root().join(path);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, contents).unwrap();
        }

        fn rename(&self, from: &str, to: &str) {
            let to = self.root().join(to);
            fs::create_dir_all(to.parent().unwrap()).unwrap();
            fs::rename(self.root().join(from), to).unwrap();
        }

        fn favorite(&mut self, path: &str) {
            self.library.set_favorite(Path::new(path), true).unwrap();
        }

        fn scan(&mut self) -> Reconciliation {
            self.library.scan().unwrap().metadata().unwrap().clone()
        }

        fn favorites(&self) -> Vec<String> {
            self.library
                .metadata()
                .unwrap()
                .records()
                .filter(|(_, record)| record.is_favorite())
                .map(|(path, _)| path.display().to_string())
                .collect()
        }
    }

    #[test]
    fn first_metadata_hashes_the_image() {
        let mut fixture = Fixture::new();
        fixture.write("a.png", &png(10, 10, 1));
        fixture.scan();

        fixture.favorite("a.png");

        let metadata = fixture.library.metadata().unwrap();
        let record = metadata.get(Path::new("a.png")).unwrap();
        assert!(record.hash().starts_with("blake3:"));
        assert!(record.is_favorite());
    }

    #[test]
    fn metadata_only_for_indexed_images() {
        let mut fixture = Fixture::new();
        fixture.scan();

        let result = fixture
            .library
            .update_image(Path::new("missing.png"), |record| record.set_favorite(true));

        assert!(matches!(result, Err(LibraryError::NotInLibrary { .. })));
        assert!(!fixture.root().join(".backdrop").exists());
    }

    #[test]
    fn clearing_everything_removes_the_record() {
        let mut fixture = Fixture::new();
        fixture.write("a.png", &png(10, 10, 1));
        fixture.scan();
        fixture.favorite("a.png");

        fixture
            .library
            .update_image(Path::new("a.png"), |record| record.set_favorite(false))
            .unwrap();

        assert!(fixture.library.metadata().unwrap().is_empty());
    }

    #[test]
    fn metadata_follows_a_renamed_and_moved_image() {
        let mut fixture = Fixture::new();
        fixture.write("a.png", &png(10, 10, 1));
        fixture.write("other.png", &png(10, 10, 2));
        fixture.scan();
        fixture.favorite("a.png");

        fixture.rename("a.png", "dark/renamed.png");
        let reconciliation = fixture.scan();

        assert_eq!(
            reconciliation.moved(),
            [(PathBuf::from("a.png"), PathBuf::from("dark/renamed.png"))]
        );
        assert_eq!(fixture.favorites(), ["dark/renamed.png"]);
    }

    #[test]
    fn only_same_size_images_are_hashed_as_move_candidates() {
        let mut fixture = Fixture::new();
        fixture.write("a.png", &png(10, 10, 1));
        fixture.scan();
        fixture.favorite("a.png");

        fixture.write("0-bigger.png", &[png(10, 10, 1), vec![0; 100]].concat());
        fixture.rename("a.png", "b.png");
        fixture.scan();

        assert_eq!(fixture.favorites(), ["b.png"]);
        let cached: Option<String> = fixture.library.index.cached_hash(Path::new("0-bigger.png"));
        assert_eq!(cached, None, "a file of another size was hashed");
    }

    #[test]
    fn metadata_of_a_deleted_image_is_removed_with_its_snapshot() {
        let mut fixture = Fixture::new();
        fixture.write("a.png", &png(10, 10, 1));
        fixture.write("keep.png", &png(10, 10, 2));
        fixture.scan();
        fixture.favorite("keep.png");
        fixture
            .library
            .update_image(Path::new("a.png"), |record| {
                record.set_source(Some("wallhaven/abc123".into()))
            })
            .unwrap();
        let snapshot = metadata::snapshot_file(fixture.root(), "wallhaven/abc123").unwrap();
        fs::create_dir_all(snapshot.parent().unwrap()).unwrap();
        fs::write(&snapshot, "").unwrap();

        fs::remove_file(fixture.root().join("a.png")).unwrap();
        let reconciliation = fixture.scan();

        assert_eq!(reconciliation.removed(), [PathBuf::from("a.png")]);
        assert_eq!(fixture.favorites(), ["keep.png"]);
        assert!(!snapshot.exists());
    }

    #[test]
    fn an_image_edited_in_place_keeps_its_metadata() {
        let mut fixture = Fixture::new();
        fixture.write("a.png", &png(10, 10, 1));
        fixture.scan();
        fixture.favorite("a.png");
        let before = fixture
            .library
            .metadata()
            .unwrap()
            .get(Path::new("a.png"))
            .unwrap()
            .hash()
            .to_owned();

        fixture.write("a.png", &png(20, 20, 1));
        fixture.scan();

        let metadata = fixture.library.metadata().unwrap();
        let record = metadata.get(Path::new("a.png")).unwrap();
        assert!(record.is_favorite());
        assert_ne!(record.hash(), before);
    }

    #[test]
    fn removing_many_records_needs_confirmation() {
        let mut fixture = Fixture::new();
        for ix in 0..30 {
            fixture.write(&format!("gone/{ix}.png"), &png(10, 10, ix));
        }
        fixture.write("kept.png", &png(10, 10, 99));
        fixture.scan();
        for ix in 0..30 {
            fixture.favorite(&format!("gone/{ix}.png"));
        }
        fixture.favorite("kept.png");

        fs::remove_dir_all(fixture.root().join("gone")).unwrap();
        let reconciliation = fixture.scan();

        assert!(reconciliation.removed().is_empty());
        assert_eq!(reconciliation.unconfirmed_removals().len(), 30);
        assert_eq!(fixture.favorites().len(), 31);

        let removed = fixture
            .library
            .remove_missing(reconciliation.unconfirmed_removals())
            .unwrap();

        assert_eq!(removed, 30);
        assert_eq!(fixture.favorites(), ["kept.png"]);
    }

    #[test]
    fn confirmed_removal_keeps_images_that_came_back() {
        let mut fixture = Fixture::new();
        fixture.write("a.png", &png(10, 10, 1));
        fixture.scan();
        fixture.favorite("a.png");
        fs::remove_file(fixture.root().join("a.png")).unwrap();
        let root = fixture.root().to_path_buf();
        fixture.library.index.scan(&root).unwrap();

        fixture.write("a.png", &png(10, 10, 1));
        let removed = fixture
            .library
            .remove_missing(&[PathBuf::from("a.png")])
            .unwrap();

        assert_eq!(removed, 0);
        assert_eq!(fixture.favorites(), ["a.png"]);
    }

    #[test]
    fn records_in_unreadable_folders_are_kept() {
        use std::os::unix::fs::PermissionsExt as _;

        let mut fixture = Fixture::new();
        fixture.write("locked/a.png", &png(10, 10, 1));
        fixture.scan();
        fixture.favorite("locked/a.png");
        let locked = fixture.root().join("locked");
        fs::set_permissions(&locked, fs::Permissions::from_mode(0o000)).unwrap();
        if fs::read_dir(&locked).is_ok() {
            return;
        }

        let result = fixture.library.scan();
        fs::set_permissions(&locked, fs::Permissions::from_mode(0o755)).unwrap();

        let scan = result.unwrap();
        let reconciliation = scan.metadata().unwrap();
        assert!(reconciliation.removed().is_empty());
        assert!(reconciliation.unconfirmed_removals().is_empty());
        assert_eq!(fixture.favorites(), ["locked/a.png"]);
    }

    /// A trash that moves files into a folder of its own.
    struct FolderTrash(PathBuf);

    impl Trash for FolderTrash {
        fn trash(&self, file: &Path) -> Result<(), String> {
            let target = self.0.join(file.file_name().unwrap());
            fs::rename(file, target).map_err(|error| error.to_string())
        }
    }

    #[test]
    fn deleting_moves_the_image_to_the_trash_and_forgets_it() {
        let mut fixture = Fixture::new();
        fixture.write("a.png", &png(10, 10, 1));
        fixture.write("b.png", &png(10, 10, 2));
        fixture.scan();
        fixture
            .library
            .update_image(Path::new("a.png"), |record| {
                record.set_favorite(true);
                record.set_source(Some("wallhaven/abc123".into()));
            })
            .unwrap();
        let snapshot = metadata::snapshot_file(fixture.root(), "wallhaven/abc123").unwrap();
        fs::create_dir_all(snapshot.parent().unwrap()).unwrap();
        fs::write(&snapshot, "").unwrap();
        let trash = tempfile::tempdir().unwrap();

        fixture
            .library
            .delete_image(Path::new("a.png"), &FolderTrash(trash.path().into()))
            .unwrap();

        assert!(!fixture.root().join("a.png").exists());
        assert!(trash.path().join("a.png").exists());
        let paths: Vec<_> = fixture
            .library
            .images(ImageOrder::Path)
            .unwrap()
            .iter()
            .map(|image| image.path().to_path_buf())
            .collect();
        assert_eq!(paths, [PathBuf::from("b.png")]);
        assert!(fixture.library.metadata().unwrap().is_empty());
        assert!(!snapshot.exists());
    }

    #[test]
    fn only_indexed_images_can_be_deleted() {
        let mut fixture = Fixture::new();
        fixture.write("notes.txt", b"not an image");
        fixture.scan();
        let trash = tempfile::tempdir().unwrap();
        let trash = FolderTrash(trash.path().into());

        for path in ["notes.txt", "missing.png", "../outside.png"] {
            assert!(matches!(
                fixture.library.delete_image(Path::new(path), &trash),
                Err(LibraryError::NotInLibrary { .. })
            ));
        }
        assert!(fixture.root().join("notes.txt").exists());
    }

    #[test]
    fn a_failed_trash_keeps_the_image() {
        let mut fixture = Fixture::new();
        fixture.write("a.png", &png(10, 10, 1));
        fixture.scan();

        let result = fixture.library.delete_image(
            Path::new("a.png"),
            &FolderTrash(fixture.root().join("no-such-folder")),
        );

        assert!(matches!(result, Err(LibraryError::Trash { .. })));
        assert!(fixture.root().join("a.png").exists());
        assert_eq!(fixture.library.images(ImageOrder::Path).unwrap().len(), 1);
    }

    #[test]
    fn a_copy_does_not_take_the_metadata() {
        let mut fixture = Fixture::new();
        fixture.write("a.png", &png(10, 10, 1));
        fixture.scan();
        fixture.favorite("a.png");

        fs::copy(
            fixture.root().join("a.png"),
            fixture.root().join("copy.png"),
        )
        .unwrap();
        let reconciliation = fixture.scan();

        assert_eq!(reconciliation, Reconciliation::default());
        assert_eq!(fixture.favorites(), ["a.png"]);
    }

    #[test]
    fn favorites_are_listed_and_can_be_cleared() {
        let mut fixture = Fixture::new();
        fixture.write("a.png", &png(10, 10, 1));
        fixture.write("b.png", &png(10, 10, 2));
        fixture.scan();
        fixture.favorite("a.png");
        fixture.favorite("b.png");

        fixture
            .library
            .set_favorite(Path::new("a.png"), false)
            .unwrap();

        assert_eq!(
            fixture.library.favorites().unwrap(),
            HashSet::from([PathBuf::from("b.png")])
        );
    }

    #[test]
    fn user_paths_resolve_to_library_paths() {
        let fixture = Fixture::new();
        fixture.write("dark/a.png", &png(10, 10, 1));
        let outside = tempfile::tempdir().unwrap();
        fs::write(outside.path().join("b.png"), png(10, 10, 2)).unwrap();
        let library = &fixture.library;

        assert_eq!(
            library
                .relative_path(&fixture.root().join("dark/../dark/a.png"))
                .unwrap(),
            Path::new("dark/a.png")
        );
        assert!(matches!(
            library.relative_path(&outside.path().join("b.png")),
            Err(LibraryError::OutsideLibrary { .. })
        ));
        assert!(matches!(
            library.relative_path(fixture.root()),
            Err(LibraryError::OutsideLibrary { .. })
        ));
        assert!(matches!(
            library.relative_path(&fixture.root().join("missing.png")),
            Err(LibraryError::NotFound { .. })
        ));
    }
}
