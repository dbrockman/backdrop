//! Core Backdrop logic shared by the GUI and the CLI.
//!
//! This crate has no GUI dependency. Library indexing, metadata, thumbnails,
//! providers, desktop integration, and settings live here so both front ends
//! use the same behavior, database, and configuration.

pub mod desktop;
pub mod discovery;
mod fs_util;
pub mod image;
pub mod index;
pub mod library;
pub mod metadata;
pub mod paths;
pub mod providers;
pub mod query;
pub mod settings;
pub mod thumbnails;

#[cfg(test)]
mod test_images;
