//! The SQLite index of the library.
//!
//! The index holds only data derived from the library, so it can be deleted and
//! rebuilt at any time. A schema change therefore deletes the database instead
//! of migrating it. The GUI and CLI open it from separate processes, so every
//! connection uses WAL mode and waits for locks instead of failing.

use std::collections::HashMap;
use std::ffi::OsStr;
use std::fs;
use std::io;
use std::os::unix::ffi::OsStrExt as _;
use std::path::{Path, PathBuf};
use std::time::Duration;

use rusqlite::{Connection, OptionalExtension as _, TransactionBehavior, params};

use crate::discovery::{self, DiscoveryError, FileStamp, ScanIssue};
use crate::fs_util;
use crate::image::{ImageFile, ImageFormat};
use crate::query::ImageOrder;

/// Bump when the schema changes; existing databases are then rebuilt.
const SCHEMA_VERSION: i32 = 3;

const SCHEMA: &str = "
    CREATE TABLE IF NOT EXISTS meta (
        key TEXT PRIMARY KEY,
        value BLOB NOT NULL
    );
    CREATE TABLE IF NOT EXISTS images (
        id INTEGER PRIMARY KEY,
        path BLOB NOT NULL UNIQUE,
        size INTEGER NOT NULL,
        modified INTEGER NOT NULL,
        added INTEGER NOT NULL,
        width INTEGER NOT NULL,
        height INTEGER NOT NULL,
        format TEXT NOT NULL,
        -- Content hash, computed only when needed; cleared when the file changes.
        hash TEXT
    );
    CREATE INDEX IF NOT EXISTS images_by_added ON images (added);
    CREATE INDEX IF NOT EXISTS images_by_modified ON images (modified);
";

const LOCK_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Debug, thiserror::Error)]
pub enum IndexError {
    #[error("could not open the index at {}: {cause}", path.display())]
    Open { path: PathBuf, cause: io::Error },
    #[error("index database error: {0}")]
    Database(rusqlite::Error),
    #[error(transparent)]
    Discovery(DiscoveryError),
    #[error("could not read {}: {cause}", path.display())]
    Hash { path: PathBuf, cause: io::Error },
}

impl From<rusqlite::Error> for IndexError {
    fn from(error: rusqlite::Error) -> Self {
        Self::Database(error)
    }
}

/// What a scan changed.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ScanSummary {
    total: usize,
    added: usize,
    updated: usize,
    removed: usize,
    issues: Vec<ScanIssue>,
    /// The last known size of each image this scan removed.
    pub(crate) removed_sizes: HashMap<PathBuf, u64>,
    /// Paths that could not be read; images below them may still exist.
    pub(crate) unreadable: Vec<PathBuf>,
}

impl ScanSummary {
    /// Images in the index after the scan.
    pub fn total(&self) -> usize {
        self.total
    }

    pub fn added(&self) -> usize {
        self.added
    }

    pub fn updated(&self) -> usize {
        self.updated
    }

    pub fn removed(&self) -> usize {
        self.removed
    }

    /// Files and folders that could not be indexed.
    pub fn issues(&self) -> &[ScanIssue] {
        &self.issues
    }

    /// Whether an image at `path` that is not in the index may still exist,
    /// because its folder could not be read.
    pub(crate) fn may_exist(&self, path: &Path) -> bool {
        self.unreadable
            .iter()
            .any(|prefix| path.starts_with(prefix))
    }
}

/// A connection to the index. Open one per thread.
pub struct Index {
    connection: Connection,
}

impl Index {
    /// Opens the index at `path`, creating it, or rebuilding it if it was
    /// written by a different schema version.
    pub fn open(path: &Path) -> Result<Self, IndexError> {
        if let Some(dir) = path.parent() {
            fs::create_dir_all(dir).map_err(|cause| IndexError::Open {
                path: path.to_path_buf(),
                cause,
            })?;
        }
        let connection = connect(path)?;
        let version: i32 = connection.pragma_query_value(None, "user_version", |row| row.get(0))?;
        let connection = if version == 0 || version == SCHEMA_VERSION {
            connection
        } else {
            drop(connection);
            delete_database(path).map_err(|cause| IndexError::Open {
                path: path.to_path_buf(),
                cause,
            })?;
            connect(path)?
        };
        Self::with_connection(connection)
    }

    /// Opens an index that lives only in memory, for tests.
    pub fn in_memory() -> Result<Self, IndexError> {
        Self::with_connection(Connection::open_in_memory()?)
    }

    fn with_connection(mut connection: Connection) -> Result<Self, IndexError> {
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        transaction.execute_batch(SCHEMA)?;
        transaction.pragma_update(None, "user_version", SCHEMA_VERSION)?;
        transaction.commit()?;
        Ok(Self { connection })
    }

    /// Brings the index up to date with the library at `root`. Only new and
    /// changed files are read. Images that could not be checked, because their
    /// folder was unreadable, stay in the index.
    ///
    /// The index covers one library; scanning a different root replaces it.
    pub fn scan(&mut self, root: &Path) -> Result<ScanSummary, IndexError> {
        if self.root()?.as_deref() != Some(root) {
            let transaction = self.connection.transaction()?;
            transaction.execute("DELETE FROM images", [])?;
            transaction.execute(
                "INSERT OR REPLACE INTO meta (key, value) VALUES ('root', ?1)",
                [root.as_os_str().as_bytes()],
            )?;
            transaction.commit()?;
        }

        let known = self.stamps()?;
        let discovery = discovery::discover(root, &known).map_err(IndexError::Discovery)?;

        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let mut summary = ScanSummary::default();
        {
            let mut upsert = transaction.prepare(
                "INSERT INTO images (path, size, modified, added, width, height, format)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
                 ON CONFLICT (path) DO UPDATE SET
                    size = excluded.size,
                    modified = excluded.modified,
                    added = excluded.added,
                    width = excluded.width,
                    height = excluded.height,
                    format = excluded.format,
                    hash = NULL",
            )?;
            for image in &discovery.changed {
                upsert.execute(params![
                    path_bytes(image.path()),
                    image.size() as i64,
                    image.modified(),
                    image.added(),
                    image.width(),
                    image.height(),
                    image.format().as_str(),
                ])?;
                if known.contains_key(image.path()) {
                    summary.updated += 1;
                } else {
                    summary.added += 1;
                }
            }

            let mut delete = transaction.prepare("DELETE FROM images WHERE path = ?1")?;
            for (path, stamp) in &known {
                if !discovery.found.contains(path) && !discovery.may_exist(path) {
                    delete.execute([path_bytes(path)])?;
                    summary.removed += 1;
                    summary.removed_sizes.insert(path.clone(), stamp.size);
                }
            }
        }
        let total: i64 =
            transaction.query_row("SELECT count(*) FROM images", [], |row| row.get(0))?;
        summary.total = total as usize;
        transaction.commit()?;
        summary.issues = discovery.issues;
        summary.unreadable = discovery.unreadable;
        Ok(summary)
    }

    /// All indexed images.
    /// All indexed images, in `order`.
    pub fn images(&self, order: ImageOrder) -> Result<Vec<ImageFile>, IndexError> {
        let mut statement = self
            .connection
            .prepare("SELECT path, size, modified, added, width, height, format FROM images")?;
        let mut images = statement
            .query_map([], |row| {
                let path: Vec<u8> = row.get(0)?;
                let size: i64 = row.get(1)?;
                let format: String = row.get(6)?;
                Ok(ImageFile::new(
                    PathBuf::from(OsStr::from_bytes(&path)),
                    size as u64,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                    row.get(5)?,
                    // Only this module writes the column.
                    ImageFormat::from_name(&format).expect("valid format in index"),
                ))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        order.sort(&mut images);
        Ok(images)
    }

    /// The content hash of the indexed image at `path`, computing and caching it
    /// if needed. Returns `None` if the image is not indexed.
    pub(crate) fn hash(&self, root: &Path, path: &Path) -> Result<Option<String>, IndexError> {
        let cached: Option<Option<String>> = self
            .connection
            .query_row(
                "SELECT hash FROM images WHERE path = ?1",
                [path_bytes(path)],
                |row| row.get(0),
            )
            .optional()?;
        match cached {
            None => Ok(None),
            Some(Some(hash)) => Ok(Some(hash)),
            Some(None) => {
                let hash =
                    fs_util::hash_file(&root.join(path)).map_err(|cause| IndexError::Hash {
                        path: path.to_path_buf(),
                        cause,
                    })?;
                self.connection.execute(
                    "UPDATE images SET hash = ?1 WHERE path = ?2",
                    params![hash, path_bytes(path)],
                )?;
                Ok(Some(hash))
            }
        }
    }

    #[cfg(test)]
    pub(crate) fn cached_hash(&self, path: &Path) -> Option<String> {
        self.connection
            .query_row(
                "SELECT hash FROM images WHERE path = ?1",
                [path_bytes(path)],
                |row| row.get(0),
            )
            .unwrap()
    }

    /// The library root the index was last scanned for.
    fn root(&self) -> Result<Option<PathBuf>, IndexError> {
        let root: Option<Vec<u8>> = self
            .connection
            .query_row("SELECT value FROM meta WHERE key = 'root'", [], |row| {
                row.get(0)
            })
            .optional()?;
        Ok(root.map(|bytes| PathBuf::from(OsStr::from_bytes(&bytes))))
    }

    fn stamps(&self) -> Result<HashMap<PathBuf, FileStamp>, IndexError> {
        let mut statement = self
            .connection
            .prepare("SELECT path, size, modified FROM images")?;
        let stamps = statement
            .query_map([], |row| {
                let path: Vec<u8> = row.get(0)?;
                let size: i64 = row.get(1)?;
                Ok((
                    PathBuf::from(OsStr::from_bytes(&path)),
                    FileStamp {
                        size: size as u64,
                        modified: row.get(2)?,
                    },
                ))
            })?
            .collect::<Result<_, _>>()?;
        Ok(stamps)
    }
}

fn connect(path: &Path) -> Result<Connection, IndexError> {
    let connection = Connection::open(path)?;
    connection.busy_timeout(LOCK_TIMEOUT)?;
    connection.pragma_update(None, "journal_mode", "WAL")?;
    connection.pragma_update(None, "synchronous", "NORMAL")?;
    Ok(connection)
}

fn delete_database(path: &Path) -> io::Result<()> {
    for suffix in ["", "-wal", "-shm"] {
        let mut file = path.as_os_str().to_owned();
        file.push(suffix);
        match fs::remove_file(&file) {
            Err(error) if error.kind() != io::ErrorKind::NotFound => return Err(error),
            _ => {}
        }
    }
    Ok(())
}

fn path_bytes(path: &Path) -> &[u8] {
    path.as_os_str().as_bytes()
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::{PermissionsExt as _, symlink};

    use super::*;

    use crate::test_images::{jpeg, rotated_jpeg};

    fn png(width: u32, height: u32) -> Vec<u8> {
        crate::test_images::png(width, height, 0)
    }

    fn write(root: &Path, path: &str, contents: &[u8]) {
        let path = root.join(path);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, contents).unwrap();
    }

    fn paths(index: &Index, order: ImageOrder) -> Vec<String> {
        index
            .images(order)
            .unwrap()
            .iter()
            .map(|image| image.path().display().to_string())
            .collect()
    }

    #[test]
    fn finds_images_recursively() {
        let library = tempfile::tempdir().unwrap();
        write(library.path(), "a.png", &png(192, 108));
        write(library.path(), "dark/b.JPG", &jpeg(108, 192));
        write(library.path(), "notes.txt", b"not an image");
        let mut index = Index::in_memory().unwrap();

        let summary = index.scan(library.path()).unwrap();
        assert_eq!((summary.total(), summary.added()), (2, 2));
        let images = index.images(ImageOrder::Path).unwrap();
        assert_eq!(images[0].path(), Path::new("a.png"));
        assert_eq!(images[0].format(), ImageFormat::Png);
        assert_eq!((images[0].width(), images[0].height()), (192, 108));
        assert_eq!(images[1].path(), Path::new("dark/b.JPG"));
        assert_eq!(images[1].format(), ImageFormat::Jpeg);
        assert_eq!((images[1].width(), images[1].height()), (108, 192));
    }

    #[test]
    fn rotated_photos_are_indexed_upright() {
        let library = tempfile::tempdir().unwrap();
        // Stored landscape, shown portrait.
        write(library.path(), "photo.jpg", &rotated_jpeg(64, 48));
        let mut index = Index::in_memory().unwrap();

        index.scan(library.path()).unwrap();

        let photo = &index.images(ImageOrder::Path).unwrap()[0];
        assert_eq!((photo.width(), photo.height()), (48, 64));
        assert_eq!(photo.orientation(), crate::image::Orientation::Portrait);
    }

    #[test]
    fn skips_hidden_entries_and_symlinks() {
        let library = tempfile::tempdir().unwrap();
        write(library.path(), "visible.png", &png(10, 10));
        write(library.path(), ".hidden.png", &png(10, 10));
        write(library.path(), ".git/objects/a.png", &png(10, 10));
        write(library.path(), ".backdrop/b.png", &png(10, 10));
        symlink(
            library.path().join("visible.png"),
            library.path().join("link.png"),
        )
        .unwrap();
        let mut index = Index::in_memory().unwrap();

        index.scan(library.path()).unwrap();

        assert_eq!(paths(&index, ImageOrder::Path), ["visible.png"]);
    }

    #[test]
    fn rescans_only_record_changes() {
        let library = tempfile::tempdir().unwrap();
        write(library.path(), "kept.png", &png(10, 10));
        write(library.path(), "changed.png", &png(10, 10));
        write(library.path(), "removed.png", &png(10, 10));
        let mut index = Index::in_memory().unwrap();
        index.scan(library.path()).unwrap();

        let unchanged = index.scan(library.path()).unwrap();
        write(library.path(), "changed.png", &jpeg(40, 30));
        fs::remove_file(library.path().join("removed.png")).unwrap();
        write(library.path(), "new.png", &png(10, 10));
        let changed = index.scan(library.path()).unwrap();

        assert_eq!(
            (unchanged.added(), unchanged.updated(), unchanged.removed()),
            (0, 0, 0)
        );
        assert_eq!(
            (changed.added(), changed.updated(), changed.removed()),
            (1, 1, 1)
        );
        assert_eq!(
            paths(&index, ImageOrder::Path),
            ["changed.png", "kept.png", "new.png"]
        );
        let changed = &index.images(ImageOrder::Path).unwrap()[0];
        assert_eq!((changed.format(), changed.width()), (ImageFormat::Jpeg, 40));
    }

    #[test]
    fn invalid_images_are_reported_and_not_indexed() {
        let library = tempfile::tempdir().unwrap();
        write(library.path(), "text.jpg", b"not really a jpeg");
        write(library.path(), "truncated.png", &png(10, 10)[..30]);
        let mut index = Index::in_memory().unwrap();

        let summary = index.scan(library.path()).unwrap();

        assert_eq!(summary.total(), 0);
        let mut reported: Vec<_> = summary.issues().iter().map(|i| i.path()).collect();
        reported.sort();
        assert_eq!(
            reported,
            [Path::new("text.jpg"), Path::new("truncated.png")]
        );
    }

    #[test]
    fn images_in_unreadable_folders_stay_indexed() {
        let library = tempfile::tempdir().unwrap();
        write(library.path(), "locked/a.png", &png(10, 10));
        let mut index = Index::in_memory().unwrap();
        index.scan(library.path()).unwrap();
        let locked = library.path().join("locked");
        fs::set_permissions(&locked, fs::Permissions::from_mode(0o000)).unwrap();
        if fs::read_dir(&locked).is_ok() {
            // Running with permission to read anything, e.g. as root.
            return;
        }

        let summary = index.scan(library.path());
        fs::set_permissions(&locked, fs::Permissions::from_mode(0o755)).unwrap();

        let summary = summary.unwrap();
        assert_eq!((summary.removed(), summary.total()), (0, 1));
        assert_eq!(summary.issues()[0].path(), Path::new("locked"));
    }

    #[test]
    fn missing_library_is_an_error() {
        let library = tempfile::tempdir().unwrap();
        let mut index = Index::in_memory().unwrap();

        let result = index.scan(&library.path().join("missing"));

        assert!(matches!(result, Err(IndexError::Discovery(_))));
    }

    #[test]
    fn orders_images() {
        let library = tempfile::tempdir().unwrap();
        write(library.path(), "small.png", &png(100, 100));
        write(library.path(), "large.png", &png(400, 300));
        write(library.path(), "medium.png", &png(200, 200));
        let mut index = Index::in_memory().unwrap();
        index.scan(library.path()).unwrap();

        assert_eq!(
            paths(&index, ImageOrder::Resolution),
            ["large.png", "medium.png", "small.png"]
        );
        assert_eq!(
            paths(&index, ImageOrder::Path),
            ["large.png", "medium.png", "small.png"]
        );
    }

    #[test]
    fn scanning_another_library_replaces_the_index() {
        let first = tempfile::tempdir().unwrap();
        let second = tempfile::tempdir().unwrap();
        write(first.path(), "first.png", &png(10, 10));
        write(second.path(), "second.png", &png(10, 10));
        let mut index = Index::in_memory().unwrap();
        index.scan(first.path()).unwrap();

        let summary = index.scan(second.path()).unwrap();

        assert_eq!((summary.added(), summary.removed()), (1, 0));
        assert_eq!(paths(&index, ImageOrder::Path), ["second.png"]);
    }

    #[test]
    fn database_from_another_schema_version_is_rebuilt() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("index/library.sqlite3");
        fs::create_dir_all(file.parent().unwrap()).unwrap();
        {
            let old = Connection::open(&file).unwrap();
            old.execute_batch("CREATE TABLE images (unrelated TEXT); PRAGMA user_version = 99;")
                .unwrap();
        }
        let library = tempfile::tempdir().unwrap();
        write(library.path(), "a.png", &png(10, 10));

        let mut index = Index::open(&file).unwrap();
        let summary = index.scan(library.path()).unwrap();

        assert_eq!(summary.total(), 1);
        let version: i32 = index
            .connection
            .pragma_query_value(None, "user_version", |row| row.get(0))
            .unwrap();
        assert_eq!(version, SCHEMA_VERSION);
    }
}
