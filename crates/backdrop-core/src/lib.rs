//! Core Backdrop logic shared by the GUI and the CLI.
//!
//! This crate has no GUI dependency. Library indexing, metadata, thumbnails,
//! providers, desktop integration, and settings live here so both front ends
//! use the same behavior, database, and configuration.

pub mod paths;
