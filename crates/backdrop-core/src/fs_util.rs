//! Filesystem helpers shared by the modules that write files.

use std::fs;
use std::io::{self, Write as _};
use std::path::Path;

/// Writes to a temporary file beside `path`, then renames it over `path`, so
/// readers see either the old contents or the new, never a partial write. The
/// new file is readable only by the user, as temporary files are created.
pub(crate) fn write_atomically(path: &Path, contents: &[u8]) -> io::Result<()> {
    let dir = path
        .parent()
        .filter(|dir| !dir.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    fs::create_dir_all(dir)?;
    let mut file = tempfile::NamedTempFile::new_in(dir)?;
    file.write_all(contents)?;
    file.as_file().sync_all()?;
    file.persist(path).map_err(|error| error.error)?;
    Ok(())
}

/// The BLAKE3 hash of a file's contents, as stored in library metadata.
pub(crate) fn hash_file(path: &Path) -> io::Result<String> {
    let mut hasher = blake3::Hasher::new();
    hasher.update_reader(fs::File::open(path)?)?;
    Ok(format!("blake3:{}", hasher.finalize().to_hex()))
}
