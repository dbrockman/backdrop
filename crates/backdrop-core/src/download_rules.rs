//! Download rules: `.backdrop/downloads.toml` in the library folder.
//!
//! The rules describe the library's folder structure, so they are stored with
//! it (spec §3.6). Each rule names a folder and the criteria a wallpaper must
//! meet to be saved there. Rules are tried in order and the first match wins;
//! a wallpaper no rule matches goes to the default folder, or the library
//! folder itself.
//!
//! ```toml
//! version = 1
//! default = "new"
//!
//! [[rules]]
//! folder = "nsfw"
//! rating = ["sketchy", "nsfw"]
//!
//! [[rules]]
//! folder = "portrait"
//! orientation = "portrait"
//! ```
//!
//! Within a rule every criterion must match, and a criterion with several
//! values matches any of them. A rule without criteria matches everything.
//! The file is edited by hand, so anything misspelled is an error rather than
//! a criterion silently ignored.

use std::io;
use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::fs_util;
use crate::image::Orientation;
use crate::metadata::METADATA_DIR;
use crate::providers::{ContentRating, RemoteWallpaper};

const RULES_FILE: &str = "downloads.toml";
const FORMAT_VERSION: u32 = 1;

#[derive(Debug, thiserror::Error)]
pub enum RulesError {
    #[error("could not read {}: {cause}", path.display())]
    Read { path: PathBuf, cause: io::Error },
    #[error("could not parse {}: {cause}", path.display())]
    Parse {
        path: PathBuf,
        cause: toml::de::Error,
    },
    #[error("{} was written by a newer version of Backdrop", path.display())]
    NewerVersion { path: PathBuf },
    #[error("{}: {folder:?} is not a folder in the library; use a relative path without hidden folders", path.display())]
    InvalidFolder { path: PathBuf, folder: String },
}

/// The download rules of one library.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DownloadRules {
    default: PathBuf,
    rules: Vec<DownloadRule>,
}

/// One rule: where to save the wallpapers that meet its criteria.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DownloadRule {
    folder: PathBuf,
    providers: Vec<String>,
    ratings: Vec<ContentRating>,
    orientations: Vec<Orientation>,
    categories: Vec<String>,
    tags: Vec<String>,
    at_least: Option<(u32, u32)>,
}

/// Where a download goes, and why.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Destination {
    /// Relative to the library root.
    pub folder: PathBuf,
    /// The index of the rule that chose the folder, or `None` for the default.
    pub rule: Option<usize>,
}

impl DownloadRules {
    pub fn file(root: &Path) -> PathBuf {
        root.join(METADATA_DIR).join(RULES_FILE)
    }

    /// Reads the rules of the library at `root`. A missing file means no
    /// rules: everything goes to the library folder.
    pub fn load(root: &Path) -> Result<Self, RulesError> {
        let path = Self::file(root);
        let text = match std::fs::read_to_string(&path) {
            Ok(text) => text,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Self::default()),
            Err(cause) => return Err(RulesError::Read { path, cause }),
        };
        Self::parse(&text, &path)
    }

    fn parse(text: &str, path: &Path) -> Result<Self, RulesError> {
        let file: RulesFile = toml::from_str(text).map_err(|cause| RulesError::Parse {
            path: path.to_path_buf(),
            cause,
        })?;
        if file.version > FORMAT_VERSION {
            return Err(RulesError::NewerVersion {
                path: path.to_path_buf(),
            });
        }
        let folder = |folder: String| {
            let relative = PathBuf::from(&folder);
            if fs_util::is_plain_folder(&relative) {
                Ok(relative)
            } else {
                Err(RulesError::InvalidFolder {
                    path: path.to_path_buf(),
                    folder,
                })
            }
        };
        let rules = file
            .rules
            .into_iter()
            .map(|rule| {
                Ok(DownloadRule {
                    folder: folder(rule.folder)?,
                    providers: rule.provider.into_vec(),
                    ratings: rule.rating.into_vec(),
                    orientations: rule.orientation.into_vec(),
                    categories: rule.category.into_vec(),
                    tags: rule.tags.into_vec(),
                    at_least: rule.at_least.map(|resolution| resolution.0),
                })
            })
            .collect::<Result<_, RulesError>>()?;
        Ok(Self {
            default: folder(file.default)?,
            rules,
        })
    }

    /// The folder wallpapers no rule matches go to; empty for the library
    /// folder itself.
    pub fn default_folder(&self) -> &Path {
        &self.default
    }

    pub fn rules(&self) -> &[DownloadRule] {
        &self.rules
    }

    /// Where `wallpaper` goes: the folder of the first rule it matches, or
    /// the default folder. Tag criteria need the wallpaper's tags, which
    /// search results leave out.
    pub fn destination(&self, wallpaper: &RemoteWallpaper) -> Destination {
        match self.rules.iter().position(|rule| rule.matches(wallpaper)) {
            Some(index) => Destination {
                folder: self.rules[index].folder.clone(),
                rule: Some(index),
            },
            None => Destination {
                folder: self.default.clone(),
                rule: None,
            },
        }
    }
}

impl DownloadRule {
    pub fn folder(&self) -> &Path {
        &self.folder
    }

    /// Provider ids, such as `wallhaven`.
    pub fn providers(&self) -> &[String] {
        &self.providers
    }

    pub fn ratings(&self) -> &[ContentRating] {
        &self.ratings
    }

    pub fn orientations(&self) -> &[Orientation] {
        &self.orientations
    }

    /// Provider category names, matched ignoring case.
    pub fn categories(&self) -> &[String] {
        &self.categories
    }

    /// Tag names, matched ignoring case; any one of them is enough.
    pub fn tags(&self) -> &[String] {
        &self.tags
    }

    pub fn at_least(&self) -> Option<(u32, u32)> {
        self.at_least
    }

    pub fn matches(&self, wallpaper: &RemoteWallpaper) -> bool {
        let any = |values: &[String], value: &str| {
            values
                .iter()
                .any(|wanted| wanted.eq_ignore_ascii_case(value))
        };
        let tags = wallpaper.tags().unwrap_or_default();
        (self.providers.is_empty() || any(&self.providers, wallpaper.provider()))
            && (self.ratings.is_empty() || self.ratings.contains(&wallpaper.rating()))
            && (self.orientations.is_empty()
                || self.orientations.contains(&wallpaper.orientation()))
            && (self.categories.is_empty()
                || wallpaper
                    .category()
                    .is_some_and(|category| any(&self.categories, category)))
            && (self.tags.is_empty() || tags.iter().any(|tag| any(&self.tags, &tag.name)))
            && self.at_least.is_none_or(|(width, height)| {
                wallpaper.width() >= width && wallpaper.height() >= height
            })
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RulesFile {
    version: u32,
    #[serde(default)]
    default: String,
    #[serde(default)]
    rules: Vec<RuleEntry>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RuleEntry {
    folder: String,
    #[serde(default)]
    provider: OneOrMany<String>,
    #[serde(default)]
    rating: OneOrMany<ContentRating>,
    #[serde(default)]
    orientation: OneOrMany<Orientation>,
    #[serde(default)]
    category: OneOrMany<String>,
    #[serde(default)]
    tags: OneOrMany<String>,
    at_least: Option<Resolution>,
}

/// A value, or a list of values, so `rating = "nsfw"` reads naturally.
#[derive(Deserialize)]
#[serde(untagged)]
enum OneOrMany<T> {
    One(T),
    Many(Vec<T>),
}

impl<T> Default for OneOrMany<T> {
    fn default() -> Self {
        Self::Many(Vec::new())
    }
}

impl<T> OneOrMany<T> {
    fn into_vec(self) -> Vec<T> {
        match self {
            Self::One(value) => vec![value],
            Self::Many(values) => values,
        }
    }
}

/// `"3840x2160"`.
#[derive(Deserialize)]
#[serde(try_from = "String")]
struct Resolution((u32, u32));

impl TryFrom<String> for Resolution {
    type Error = String;

    fn try_from(text: String) -> Result<Self, String> {
        let invalid = || format!("expected WIDTHxHEIGHT, such as \"3840x2160\", not {text:?}");
        let (width, height) = text.split_once('x').ok_or_else(invalid)?;
        let width = width.trim().parse().map_err(|_| invalid())?;
        let height = height.trim().parse().map_err(|_| invalid())?;
        Ok(Self((width, height)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::providers::Tag;

    fn parse(text: &str) -> Result<DownloadRules, RulesError> {
        DownloadRules::parse(text, Path::new("downloads.toml"))
    }

    /// A 20 × 10 safe-for-work wallpaper.
    fn wallpaper() -> RemoteWallpaper {
        RemoteWallpaper::for_test("wallhaven", "abc123")
    }

    fn folder(rules: &DownloadRules, wallpaper: &RemoteWallpaper) -> String {
        rules.destination(wallpaper).folder.display().to_string()
    }

    #[test]
    fn missing_file_sends_everything_to_the_library_folder() {
        let library = tempfile::tempdir().unwrap();
        let rules = DownloadRules::load(library.path()).unwrap();

        assert_eq!(
            rules.destination(&wallpaper()),
            Destination {
                folder: PathBuf::new(),
                rule: None
            }
        );
    }

    #[test]
    fn the_first_matching_rule_wins() {
        let rules = parse(
            r#"
            version = 1
            default = "new"

            [[rules]]
            folder = "nsfw"
            rating = ["sketchy", "nsfw"]

            [[rules]]
            folder = "portrait"
            orientation = "portrait"
            "#,
        )
        .unwrap();
        let mut sketchy_portrait = wallpaper();
        sketchy_portrait.rating = ContentRating::Sketchy;
        sketchy_portrait.width = 10;
        sketchy_portrait.height = 20;
        let mut portrait = wallpaper();
        portrait.width = 10;
        portrait.height = 20;

        assert_eq!(
            rules.destination(&sketchy_portrait),
            Destination {
                folder: PathBuf::from("nsfw"),
                rule: Some(0)
            }
        );
        assert_eq!(folder(&rules, &portrait), "portrait");
        assert_eq!(
            rules.destination(&wallpaper()),
            Destination {
                folder: PathBuf::from("new"),
                rule: None
            }
        );
    }

    #[test]
    fn every_criterion_of_a_rule_must_match() {
        let rules = parse(
            r#"
            version = 1

            [[rules]]
            folder = "nature/big"
            provider = "wallhaven"
            category = "General"
            tags = ["Mountains", "lakes"]
            at_least = "3840x2160"
            "#,
        )
        .unwrap();
        let mut matching = wallpaper();
        matching.category = Some("general".to_owned());
        matching.width = 5120;
        matching.height = 2880;
        matching.tags = Some(vec![Tag {
            name: "mountains".to_owned(),
            category: None,
            id: None,
        }]);

        assert_eq!(folder(&rules, &matching), "nature/big");
        let mut small = matching.clone();
        small.height = 2000;
        assert_eq!(folder(&rules, &small), "");
        let mut untagged = matching.clone();
        untagged.tags = None;
        assert_eq!(folder(&rules, &untagged), "");
        let mut other_category = matching.clone();
        other_category.category = Some("anime".to_owned());
        assert_eq!(folder(&rules, &other_category), "");
        let mut other_provider = matching;
        other_provider.provider = "other";
        assert_eq!(folder(&rules, &other_provider), "");
    }

    #[test]
    fn a_rule_without_criteria_matches_everything() {
        let rules = parse("version = 1\n[[rules]]\nfolder = \"inbox\"\n").unwrap();
        assert_eq!(folder(&rules, &wallpaper()), "inbox");
    }

    #[test]
    fn misspellings_are_errors() {
        for text in [
            "version = 1\n[[rules]]\nfolder = \"a\"\nratings = \"nsfw\"\n",
            "version = 1\n[[rules]]\nfolder = \"a\"\nrating = \"explicit\"\n",
            "version = 1\n[[rules]]\nfolder = \"a\"\norientation = \"wide\"\n",
            "version = 1\n[[rules]]\nfolder = \"a\"\nat_least = \"4k\"\n",
            "version = 1\n[[rules]]\nrating = \"nsfw\"\n",
            "version = 1\ndefault_folder = \"a\"\n",
        ] {
            assert!(
                matches!(parse(text), Err(RulesError::Parse { .. })),
                "{text}"
            );
        }
    }

    #[test]
    fn folders_must_be_plain_library_folders() {
        for folder in ["../outside", "/tmp", ".hidden", "a/../b", "./a"] {
            let text = format!("version = 1\n[[rules]]\nfolder = {folder:?}\n");
            assert!(
                matches!(parse(&text), Err(RulesError::InvalidFolder { .. })),
                "{folder}"
            );
            let text = format!("version = 1\ndefault = {folder:?}\n");
            assert!(
                matches!(parse(&text), Err(RulesError::InvalidFolder { .. })),
                "{folder}"
            );
        }
    }

    #[test]
    fn newer_format_is_refused() {
        assert!(matches!(
            parse("version = 2\n"),
            Err(RulesError::NewerVersion { .. })
        ));
    }
}
