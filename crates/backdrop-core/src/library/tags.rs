//! User tags, and every tag in the library for suggestions (spec §2.5).
//!
//! User tags are stored in the image's metadata record, trimmed and with
//! inner whitespace collapsed; tags ignore case, so a tag is added only once,
//! in the spelling first used. Provider tags stay in the provider snapshots,
//! separate from the user's, but both are searchable.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use super::{Library, LibraryError};
use crate::metadata::{normalize_tag, same_tag};
use crate::query::ImageMarks;
use crate::snapshot::Snapshot;

/// A tag used in the library, and how many images have it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TagCount {
    name: String,
    user: usize,
    provider: usize,
}

impl TagCount {
    /// The tag as first spelled, preferring the user's spelling.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Images the user gave this tag.
    pub fn user(&self) -> usize {
        self.user
    }

    /// Images whose provider gave them this tag.
    pub fn provider(&self) -> usize {
        self.provider
    }
}

impl Library {
    /// Favorites and tags of every image, for filtering. Provider tags come
    /// from the snapshots of recorded sources; an unreadable snapshot adds
    /// none.
    pub fn marks(&self) -> Result<ImageMarks, LibraryError> {
        let metadata = self.metadata()?;
        let mut favorites = HashSet::new();
        let mut tags = HashMap::new();
        let mut provider_tags = HashMap::new();
        let mut snapshots: HashMap<String, Vec<String>> = HashMap::new();
        for (path, record) in metadata.records() {
            if record.is_favorite() {
                favorites.insert(path.clone());
            }
            if !record.tags().is_empty() {
                tags.insert(path.clone(), record.tags().to_vec());
            }
            if let Some(source) = record.source() {
                let names = snapshots.entry(source.to_owned()).or_insert_with(|| {
                    Snapshot::load(&self.root, source)
                        .ok()
                        .flatten()
                        .map(|snapshot| snapshot.tags().iter().map(|t| t.name.clone()).collect())
                        .unwrap_or_default()
                });
                if !names.is_empty() {
                    provider_tags.insert(path, names.clone());
                }
            }
        }
        Ok(ImageMarks::new(favorites, tags, provider_tags))
    }

    /// Replaces the user tags of the indexed image at `path`, and returns
    /// them as stored.
    pub fn set_tags(&mut self, path: &Path, tags: &[String]) -> Result<Vec<String>, LibraryError> {
        let tags = normalize_tags(tags)?;
        let stored = tags.clone();
        self.update_image(path, |record| record.set_tags(tags))?;
        Ok(stored)
    }

    /// Adds user tags the image doesn't have yet, and returns all of them.
    pub fn add_tags(&mut self, path: &Path, tags: &[String]) -> Result<Vec<String>, LibraryError> {
        let added = normalize_tags(tags)?;
        let mut stored = Vec::new();
        self.update_image(path, |record| {
            let mut tags = record.tags().to_vec();
            tags.extend(added);
            stored = dedupe(tags);
            record.set_tags(stored.clone());
        })?;
        Ok(stored)
    }

    /// Removes user tags, ignoring case, and returns those left.
    pub fn remove_tags(
        &mut self,
        path: &Path,
        tags: &[String],
    ) -> Result<Vec<String>, LibraryError> {
        let mut stored = Vec::new();
        self.update_image(path, |record| {
            stored = record
                .tags()
                .iter()
                .filter(|tag| !tags.iter().any(|removed| same_tag(tag, removed)))
                .cloned()
                .collect();
            record.set_tags(stored.clone());
        })?;
        Ok(stored)
    }

    /// Every tag in the library, user and provider, most used first.
    pub fn tag_counts(&self) -> Result<Vec<TagCount>, LibraryError> {
        let marks = self.marks()?;
        let paths: HashSet<PathBuf> = self.indexed_sizes()?.into_keys().collect();
        let mut counts: HashMap<String, TagCount> = HashMap::new();
        for path in &paths {
            for (tags, from_user) in [(marks.tags(path), true), (marks.provider_tags(path), false)]
            {
                for tag in tags {
                    let count = counts
                        .entry(tag.to_lowercase())
                        .or_insert_with(|| TagCount {
                            name: tag.clone(),
                            user: 0,
                            provider: 0,
                        });
                    if from_user {
                        if count.user == 0 {
                            // The user's spelling wins over a provider's.
                            count.name = tag.clone();
                        }
                        count.user += 1;
                    } else {
                        count.provider += 1;
                    }
                }
            }
        }
        let mut counts: Vec<TagCount> = counts.into_values().collect();
        counts.sort_by(|a, b| {
            (b.user + b.provider)
                .cmp(&(a.user + a.provider))
                .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
        });
        Ok(counts)
    }
}

/// Tags as stored: normalized, each once. Fails on a tag that can't be one.
fn normalize_tags(tags: &[String]) -> Result<Vec<String>, LibraryError> {
    let tags = tags
        .iter()
        .map(|tag| normalize_tag(tag).ok_or_else(|| LibraryError::InvalidTag { tag: tag.clone() }))
        .collect::<Result<Vec<_>, _>>()?;
    Ok(dedupe(tags))
}

/// Keeps the first of tags that differ only in case.
fn dedupe(tags: Vec<String>) -> Vec<String> {
    let mut kept: Vec<String> = Vec::new();
    for tag in tags {
        if !kept.iter().any(|existing| same_tag(existing, &tag)) {
            kept.push(tag);
        }
    }
    kept
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;
    use crate::index::Index;
    use crate::providers::RemoteWallpaper;
    use crate::test_images::png;

    fn library(names: &[&str]) -> (tempfile::TempDir, Library) {
        let dir = tempfile::tempdir().unwrap();
        for (seed, name) in names.iter().enumerate() {
            fs::write(dir.path().join(name), png(20, 10, seed as u8)).unwrap();
        }
        let mut library = Library::with_index(dir.path(), Index::in_memory().unwrap());
        library.scan().unwrap();
        (dir, library)
    }

    fn strings(tags: &[&str]) -> Vec<String> {
        tags.iter().map(|tag| (*tag).to_owned()).collect()
    }

    #[test]
    fn tags_are_normalized_and_kept_once_in_their_first_spelling() {
        let (_dir, mut library) = library(&["a.png"]);
        let path = Path::new("a.png");

        let stored = library
            .set_tags(path, &strings(&[" Snowy   peaks ", "night", "Night"]))
            .unwrap();

        assert_eq!(stored, ["Snowy peaks", "night"]);
        let metadata = library.metadata().unwrap();
        assert_eq!(metadata.get(path).unwrap().tags(), ["Snowy peaks", "night"]);
    }

    #[test]
    fn tags_are_added_and_removed_ignoring_case() {
        let (_dir, mut library) = library(&["a.png"]);
        let path = Path::new("a.png");
        library.add_tags(path, &strings(&["lake"])).unwrap();

        let added = library
            .add_tags(path, &strings(&["LAKE", "forest"]))
            .unwrap();
        assert_eq!(added, ["lake", "forest"]);
        let left = library.remove_tags(path, &strings(&["Lake"])).unwrap();
        assert_eq!(left, ["forest"]);

        // A record left with nothing but its hash is removed.
        library.remove_tags(path, &strings(&["forest"])).unwrap();
        assert!(library.metadata().unwrap().is_empty());
    }

    #[test]
    fn invalid_tags_change_nothing() {
        let (_dir, mut library) = library(&["a.png"]);

        let result = library.add_tags(Path::new("a.png"), &strings(&["ok", "  "]));

        assert!(matches!(result, Err(LibraryError::InvalidTag { .. })));
        assert!(library.metadata().unwrap().is_empty());
    }

    #[test]
    fn counts_include_provider_tags_and_prefer_the_users_spelling() {
        let (_dir, mut library) = library(&["a.png", "b.png", "c.png"]);
        library
            .add_tags(Path::new("a.png"), &strings(&["Mountains"]))
            .unwrap();
        library
            .add_tags(Path::new("c.png"), &strings(&["night"]))
            .unwrap();
        let wallpaper =
            RemoteWallpaper::for_test("wallhaven", "abc123").with_tags(&["mountains", "lake"]);
        library.link_source(Path::new("b.png"), &wallpaper).unwrap();

        let counts = library.tag_counts().unwrap();

        let summary: Vec<_> = counts
            .iter()
            .map(|count| (count.name(), count.user(), count.provider()))
            .collect();
        assert_eq!(
            summary,
            [("Mountains", 1, 1), ("lake", 0, 1), ("night", 1, 0)]
        );
        let marks = library.marks().unwrap();
        assert!(marks.has_tag(Path::new("b.png"), "LAKE"));
        assert!(marks.has_tag(Path::new("a.png"), "mountains"));
    }
}
