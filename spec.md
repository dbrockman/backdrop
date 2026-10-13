# Backdrop — Project Specification

**Status:** Initial design  
**Platform:** Linux  
**Technology:** Rust, GPUI, GPUI Kit, SQLite

## 1. Overview

Backdrop is a Linux desktop application for browsing, organizing, downloading, and managing wallpapers.

The application maintains a local wallpaper library based on a user-selected directory. It provides a visual interface for exploring the collection, viewing image metadata, filtering and sorting images, and selecting wallpapers for the desktop.

Backdrop also integrates with online wallpaper providers, initially [Wallhaven](https://wallhaven.cc), allowing users to discover and download new wallpapers directly into their local library.

The application integrates with the desktop environment to identify the currently active wallpaper and request wallpaper changes. It does not independently manage wallpaper state or require a persistent background process.

Backdrop can also provide basic Git integration for users who maintain their wallpaper collections in a Git repository.

The application is primarily a GUI, but its core functionality should also be accessible through a command-line interface.

### Design principles

- **Local-first:** The local wallpaper library remains usable without an internet connection.
- **Non-invasive:** Backdrop works with existing wallpaper directories and desktop environment configurations rather than imposing its own structure.
- **Source-independent:** Wallpapers from different sources can coexist in the same library.
- **Extensible:** Support additional online providers and desktop environments through modular interfaces.
- **Portable:** The library folder is self-contained. Images plus `.backdrop/` hold everything Backdrop cannot regenerate offline, in plain text that can be version-controlled and read without Backdrop.
- **Responsive:** Large collections should remain fast to browse through indexing, thumbnail caching, and asynchronous processing.
- **Safe:** Destructive file operations and Git operations should be explicit and predictable.

---

## 2. Local Wallpaper Library

### 2.1 Library directory

The user configures a directory containing their wallpaper collection.

Backdrop scans this directory and displays supported images in a visual grid.

Requirements:

- Support common image formats, initially JPEG, PNG, and WebP.
- Recursively discover images in subdirectories.
- Detect newly added, modified, moved, and deleted images.
- Index image metadata in a local SQLite database that can be rebuilt from the library (§5.4).
- Generate and cache thumbnails.
- Reconcile the index and the library metadata with the filesystem when the application starts, following renamed and moved images by content hash (§5.3).
- Handle missing or inaccessible files gracefully.
- Avoid requiring images to be imported or renamed before use.

The original image files remain in their existing locations.

### 2.2 Library grid

Display images in a responsive grid of thumbnails.

Each image may show contextual indicators such as:

- Currently active wallpaper
- Favorite status
- Source/provider
- Uncommitted Git status

These indicators should be configurable so the interface can remain visually minimal.

The grid should support efficient scrolling and loading even with thousands of images.

### 2.3 Image details

Selecting an image opens a larger preview with metadata and available actions.

Information displayed:

- Filename and filesystem path
- Image dimensions and aspect ratio
- File size and format
- Date added to the local library
- File modification date
- Provider and original URL, when available
- Provider publication date, when available
- Provider tags
- User-defined tags
- Git status, when applicable

Actions:

- Set as wallpaper
- Add or remove from favorites
- Edit user-defined tags
- Open image in the default external viewer
- Open containing directory in the file manager
- Open the image's original provider page
- Delete image

### 2.4 Filtering and sorting

The local library should support filtering by:

- Tags
- Orientation: landscape, portrait, square
- Favorites
- Image source/provider
- Currently active wallpaper
- Git status, when applicable

Additional filters to consider:

- Minimum resolution
- Specific aspect ratio
- Dominant color or light/dark classification

Sorting options:

- Date added
- File modification date
- Provider publication date
- Filename
- Resolution

Default sorting should prioritize recently added images.

### 2.5 Tag filtering

The filtering UI includes a text input with autocomplete.

As the user types, Backdrop suggests matching tags from the indexed library.

Users can select multiple tags to construct a filter.

The system should eventually support:

- Include specific tags
- Exclude specific tags
- Match all selected tags (AND)
- Match any selected tag (OR)

Tags originating from providers and tags created by users are stored separately, but both should be searchable.

### 2.6 Deleting images

Users can delete images directly from the library.

Deletion should support the workflow of downloading several candidate wallpapers, reviewing them, and removing unwanted images.

Requirements:

- Provide a clear delete action from the image details view.
- Support selecting and deleting multiple images.
- Prefer moving files to the system trash rather than permanently deleting them.
- Update the library index immediately after deletion.
- Handle Git-tracked and untracked files correctly.
- Avoid silently deleting files outside the configured library.

Potential future addition: a review mode that displays recently downloaded images and allows users to quickly keep or discard each one.

---

## 3. Online Wallpaper Providers

### 3.1 Provider architecture

Online sources should be accessed through a provider abstraction.

The initial implementation supports Wallhaven, with the possibility of additional providers later.

A provider is responsible for:

- Searching and browsing available wallpapers
- Retrieving image metadata
- Retrieving tags
- Providing thumbnails and previews
- Downloading original image files
- Handling authentication, where needed

The rest of the application should not depend directly on Wallhaven-specific data structures.

### 3.2 Wallhaven integration

Backdrop integrates with the Wallhaven API.

Users should be able to:

- Browse wallpapers
- Search by text or tags
- Filter by category and content rating
- Filter by orientation, aspect ratio, and resolution
- Sort using supported provider options
- View wallpaper details and tags
- Download original files directly into their configured library
- Open wallpapers on wallhaven.cc

### 3.3 Authentication and content filtering

Users may configure a Wallhaven API key in application settings.

The API key is optional for functionality that supports unauthenticated access.

**Safe-for-work content is the default.**

Explicit content must not be displayed unless the user deliberately enables it.

Provider-specific content filtering settings should remain separate from general library filters.

The application should respect the filtering and authentication capabilities of each provider.

API keys and other credentials must not be stored in Git-synchronized metadata.

A key is checked with the provider before it is saved, from the GUI (an API key dialog on the Wallhaven page) or the CLI (`backdrop wallhaven api-key set`). A key the provider rejects is not saved; one that can't be checked, for example offline, is saved with a warning. A saved key is never displayed again.

### 3.4 Detecting previously downloaded wallpapers

Backdrop should recognize when a remote wallpaper already exists in the local library.

For Wallhaven, filenames following this convention can identify the source image:

`wallhaven-{ID}.{extension}`

For example:

`wallhaven-94x38z.jpg`

The source association is also stored in the library metadata (§5.2), so it does not depend on the filename and survives renames.

Recognizing a file name only reads the name: it shows where an image came from but writes nothing and makes no request. Linking makes the association permanent. It is an explicit action, since it writes `.backdrop/` and fetches the wallpaper's details: Backdrop checks that the image has the remote wallpaper's dimensions, so a misnamed file is not given another wallpaper's tags, then saves the provider snapshot and records the source. Requests are paced to the provider's rate limit, so linking a large library slows down rather than fails.

When browsing online content, already-downloaded wallpapers should be visually distinguishable.

Potential behavior, configurable later:

- Show a small downloaded indicator
- Dim downloaded images
- Hide downloaded images entirely
- Filter to show downloaded or not-downloaded images

The default behavior remains undecided.

### 3.5 Download management

Users can download an image directly from the provider details view.

Requirements:

- Download the original full-resolution image.
- Save into the configured library directory or a matching subdirectory.
- Avoid accidentally overwriting existing files.
- Display download progress and errors.
- Update the local image index when a download completes.
- Avoid duplicate downloads of the same provider image.
- Support downloading multiple selected wallpapers.

### 3.6 Download directory rules

The user should be able to configure rules that determine which subdirectory receives a downloaded image.

Examples:

- Dark wallpapers → `dark/`
- Light wallpapers → `light/`
- NSFW wallpapers → `nsfw/`
- Portrait wallpapers → `portrait/`

Rules may use properties such as:

- Provider
- Content rating
- Orientation
- Resolution or aspect ratio
- Tags
- Light/dark classification

Initial implementation can use a small set of built-in criteria rather than a general-purpose rule language.

The library defines a default download directory and optionally a set of ordered rules. They describe the library's folder structure, so they are stored with it, in `.backdrop/downloads.toml` (§5.2), not in XDG settings.

Each matching download rule should resolve to a directory within the configured library root.

**Decided:** rules are ordered and the first matching rule wins. Rules do not combine; a nested destination such as `nsfw/dark/` is written as one rule whose criteria match both.

The initial criteria are provider, content rating, orientation, provider category, tags, and minimum resolution; light/dark classification follows with Phase 4. Within a rule every criterion must match, and a criterion with several values matches any of them. A rule without criteria matches everything. A wallpaper no rule matches goes to the default directory, or the library root if none is set. Choosing a folder for one download (`--to`) overrides the rules.

---

## 4. Desktop Environment Integration

### 4.1 General behavior

Backdrop should integrate with the user's desktop environment to identify and change wallpapers.

The desktop environment or wallpaper service remains responsible for displaying and maintaining the active wallpaper.

Backdrop should not introduce its own persistent wallpaper daemon.

Requirements:

- Read the currently active wallpaper where supported.
- Highlight the active wallpaper in the library.
- Allow the user to select and apply a new wallpaper.
- Detect external wallpaper changes where supported.
- Handle unsupported capabilities without breaking other functionality.

### 4.2 Backend abstraction

Desktop integrations should use a common interface.

Potential capabilities:

- Get currently active wallpaper
- Set wallpaper
- List available monitors
- Set wallpaper for a specific monitor
- Observe wallpaper changes

Not all desktop backends will support every capability.

### 4.3 Initial backend: Noctalia

The first implementation targets the Noctalia desktop shell, which owns wallpaper state in the user's session (running on the Niri compositor). Backdrop talks to it through its command-line IPC:

| Command | Use |
|---|---|
| `noctalia msg wallpaper-get [connector]` | Read the current wallpaper path, for all outputs or one output |
| `noctalia msg wallpaper-set [connector] <path>` | Set the wallpaper for all outputs or one output; Noctalia persists it |

Noctalia's own `wallpaper-random`, `wallpaper-next`, and `wallpaper-previous` pick from Noctalia's configured wallpaper folder, not from the Backdrop library, so Backdrop chooses random wallpapers itself and sets them with `wallpaper-set`.

The current wallpaper may be outside the Backdrop library; then no library image is marked as current.

Noctalia has no command to watch for wallpaper changes, so Backdrop reads the current wallpaper at startup, after setting one, and when its window becomes active.

Future desktop backends may include:

- GNOME
- KDE Plasma
- Other Wayland compositors and wallpaper services
- Custom user-defined commands

Multi-monitor functionality can be implemented incrementally.

### 4.4 Wallpaper preview

A future feature could preview how an image will be displayed on a particular monitor, accounting for its resolution, aspect ratio, and cropping behavior.

---

## 5. Metadata and Persistence

### 5.1 Where data lives

Data is placed by one rule: **can Backdrop regenerate it offline, from the library alone?**

- **No:** it is stored as text inside the library, in `.backdrop/`. These files are the source of truth. They can be version-controlled with the images, backed up, and read without Backdrop.
- **Yes:** it lives in the SQLite index or the thumbnail cache, outside the library. Deleting either loses nothing but rebuild time, and rebuilding needs no network access.
- **Machine- or user-specific settings and secrets** live in the XDG config directory, never in the library.

| Data | Location |
|---|---|
| Favorites, user tags, future ratings and notes | Library (`.backdrop/metadata.toml`) |
| Content hash of each image with metadata | Library (`.backdrop/metadata.toml`) |
| Provider association (provider and ID) | Library (`.backdrop/metadata.toml`) |
| Provider metadata snapshot (tags, URL, publication date, content rating, …) | Library (`.backdrop/sources/`) |
| Download rules and default download directory | Library (`.backdrop/downloads.toml`) |
| Paths, dimensions, file size, modification time, format | SQLite index |
| Hash cache, tag and search indexes, indexing state | SQLite index |
| Thumbnails | XDG cache directory |
| API keys, appearance, desktop backend, window state | XDG config directory |

Provider metadata is stored as text, not just cached, because a wallpaper can be deleted from the provider. Re-fetching cannot be relied on.

### 5.2 Library metadata files

```text
Wallpapers/
  wallhaven-94x38z.jpg
  mountain.png
  dark/
    another-wallpaper.jpg
  .backdrop/
    downloads.toml
    metadata.toml
    sources/
      wallhaven/
        94x38z.toml
```

`.backdrop/` is created only when there is something to write, so browsing a folder never modifies it.

**`metadata.toml`** holds one record per image that has user data or a provider association, keyed by its path relative to the library root with `/` separators. Images without metadata have no record, and a record left with nothing but its hash is removed. The file starts with a format version; Backdrop refuses to read or write a file from a newer version, and keeps fields it doesn't know when rewriting it.

```toml
version = 1

[images."dark/wallhaven-94x38z.jpg"]
hash = "blake3:9f2c…"
favorite = true
tags = ["mountains", "night"]
source = "wallhaven/94x38z"
```

**`sources/<provider>/<id>.toml`** holds a snapshot of one remote item's provider metadata. It is keyed by provider ID rather than path, so renames never touch it, and it is written once when the image is downloaded or first matched to a provider.

- Snapshots store normalized fields plus the time they were fetched, not raw API responses. Fields should be mapped generously, because unmapped fields are lost.
- Refreshing a snapshot only happens on explicit request. If the provider no longer has the item, the snapshot is kept and marked as removed upstream, with the date.

**`downloads.toml`** holds the download rules (§3.6): a format version, an optional `default` folder, and ordered `[[rules]]`, each with a `folder` and its criteria. Backdrop reads it but doesn't write it yet; it is edited by hand, so an unknown key or value is an error rather than a criterion silently ignored.

```toml
version = 1
default = "new"

[[rules]]
folder = "nsfw"
rating = ["sketchy", "nsfw"]

[[rules]]
folder = "nature/big"
tags = ["mountains", "forest"]
at_least = "3840x2160"
```

Writing rules:

- Output is deterministic: sorted keys and stable formatting, so each change produces a minimal diff and edits from different machines to different images merge cleanly.
- Writers re-read the file before changing it and replace it atomically, as with settings.
- Changes are written to the text files first and then to the index. If Backdrop stops in between, the next start reconciles from the files.
- The index records each metadata file's hash. When a file changes outside Backdrop (a `git pull`, a manual edit), its contents are re-imported.

Provider cache data that can be re-fetched, thumbnails, transient UI state, and secrets are never written into the library.

### 5.3 Renamed, moved, and deleted images

Users may rename, move, and delete images outside Backdrop. Each scan reconciles `metadata.toml` with the filesystem:

1. Walk the library. Changes are detected by size and modification time, compared with the index; this never reads file contents.
2. **Path exists:** the record stays. If the file changed, it was edited in place; the metadata stays with the path and the hash is updated.
3. **Path missing, same hash at an untracked path:** the image was renamed or moved, and the record moves to the new path. If several untracked copies match, the first by path is chosen.
4. **Path missing, no match:** the image was deleted. Its record is removed, and so is its provider snapshot if no other record refers to it.

A content hash only identifies an image after a rename or move, so files are hashed only when a hash will be used:

- when metadata is first saved for an image;
- when a file that has a record changes in place;
- when a record's file is missing: only untracked files with the same size the missing file had (from the index) are hashed as candidates. If the index has no size for it, for example after a rebuild, every untracked file is a candidate.

Images without metadata are never read beyond their headers. Computed hashes are cached in the index, keyed by path, size, and modification time, so no file is hashed twice.

All matching completes before anything is removed, so a move is never treated as a deletion.

Records are only removed after a complete, successful scan:

- If the library root is missing or unreadable, nothing is removed.
- Records under a subdirectory that could not be read (permissions, I/O errors, an unavailable symlink target) are kept.
- If one scan would remove a large share of the records (for example at least 25% and at least 20 images), Backdrop asks for confirmation before removing them. This protects against a checkout of an old Git branch, a drive that is not mounted, or a folder moved out by mistake.

Known limits: an image that is both moved and edited between scans loses its metadata, and a copy of an image whose original still exists starts without metadata.

### 5.4 SQLite index

Backdrop uses SQLite, through `rusqlite` with SQLite bundled, as a rebuildable index of the library. SQLite was chosen because the GUI and CLI access it from separate processes at the same time, and because filtering, sorting, and tag autocomplete are naturally expressed as indexed queries.

- The database lives in the XDG data directory, outside the library.
- Only `backdrop-core` opens it, always in WAL mode with a busy timeout, so every process configures it the same way.
- Its schema version is stored with `PRAGMA user_version`. On a version change the database is deleted and rebuilt from the library instead of migrated.
- Queries run off the UI thread.

### 5.5 Thumbnail caching

Thumbnails are generated and cached locally.

Requirements:

- Avoid repeatedly decoding full-resolution files for grid display.
- Invalidate stale thumbnails when source files change.
- Generate thumbnails asynchronously.
- Support an optional cache cleanup mechanism.
- Keep thumbnails outside Git-managed wallpaper directories.

---

## 6. Git Integration

Git support is optional and activated when the configured wallpaper directory is part of a Git repository.

### 6.1 Initial implementation: Git status

The first version of Git integration is read-only.

Backdrop detects changes within the repository and displays the Git status of images.

Possible statuses:

- Untracked
- Modified
- Deleted
- Renamed
- Clean

Requirements:

- Show which images have uncommitted changes.
- Display status indicators in the local library.
- Provide a filter for uncommitted images.
- Avoid modifying the repository automatically.

For the initial version, the application should avoid staging, committing, pulling, or pushing.

### 6.2 Future Git synchronization

Later versions may support:

- Stage selected images
- Commit changes
- Fetch changes
- Pull and push
- Display branch and remote status
- Synchronize portable metadata

Git operations should be explicit and should not silently discard local changes.

The app should recognize that image files can be large and that some repositories may use Git LFS.

---

## 7. Application Settings

Settings should be organized into logical sections.

### Library

- Wallpaper directory
- Recursive scanning behavior
- Thumbnail cache management
- Default sorting
- Default gallery filters

### Appearance

- Show/hide metadata overlays
- Show/hide image filenames
- Show/hide Git status indicators
- Show/hide downloaded indicators
- Gallery thumbnail size
- Optional theme preferences

### Providers

- Enable or disable individual providers
- Wallhaven API key
- Content rating filters
- Default provider search filters
- Handling of already-downloaded images

### Downloads

- Default download directory and subdirectory rules (stored in the library, §3.6)
- Duplicate download behavior
- Filename conventions

### Desktop integration

- Selected wallpaper backend
- Backend-specific options
- Monitor-specific preferences, where supported

### Git

- Show Git status indicators
- Future synchronization preferences

Settings should be persisted independently of the wallpaper library.

---

## 8. GUI and CLI Architecture

### 8.1 Technology

The application is implemented in Rust.

GUI framework:

- GPUI
- GPUI Kit for UI components

Storage:

- SQLite

The implementation should separate core application logic from its graphical presentation.

### 8.2 Application structure

A possible module layout:

```text
src/
  core/
    library/
    metadata/
    thumbnails/
    providers/
      wallhaven/
    desktop/
      noctalia/
    git/
    settings/
  gui/
    components/
    views/
  cli/
```

The core module should own library operations, provider integration, indexing, metadata, and desktop integration.

The GUI and CLI both interact with this shared functionality.

### 8.3 CLI

Backdrop should be usable through the command line without launching the graphical interface.

Commands in the first release:

```bash
backdrop                          # open the GUI (also `backdrop gui`)
backdrop list [--sort …] [--orientation …] [--favorites]
backdrop info <image>
backdrop set <image>
backdrop set --random [--orientation …] [--favorites]
backdrop current
backdrop scan [--remove-missing]
backdrop delete <image>…
backdrop favorite <image>…
backdrop unfavorite <image>…
backdrop library [set <dir>]
backdrop paths
```

Later phases add commands alongside their features, such as `backdrop wallhaven search`, `backdrop wallhaven info <id|link|file>`, `backdrop wallhaven download <id|link|file>… [--to <folder>]`, `backdrop wallhaven link [<image>…] [--dry-run]`, `backdrop rules` and `backdrop wallhaven api-key [set|clear]` (Phase 2) and `backdrop git status` (Phase 3). Future commands could provide filtering and metadata editing.

CLI operations should use the same settings, database, and core logic as the GUI.

The design must account for concurrent GUI and CLI access to the database.

---

## 9. Non-functional Requirements

### Performance

- Responsive browsing with thousands of images
- Asynchronous image scanning and thumbnail generation
- Lazy image decoding
- Virtualized or otherwise efficient gallery rendering
- Cached provider metadata
- No unnecessary API requests when browsing local images

### Reliability

- Filesystem remains the source of truth for local images.
- The database can be deleted and rebuilt offline from the library without losing any data.
- Metadata is never removed because of an incomplete scan, and removing a large share of it requires confirmation (§5.3).
- Interrupted downloads must not be mistaken for completed images.
- File deletion should use the system trash when available.
- Failed network requests should not prevent local browsing.

### Security and privacy

- SFW content is enabled by default.
- NSFW access requires explicit opt-in.
- Provider credentials are not stored in portable metadata.
- Download paths cannot escape the configured library directory.
- Remote data must not be treated as trusted filesystem paths or executable content.
- Background network activity should be limited and predictable.

### Extensibility

- Providers are interchangeable implementations behind a shared API.
- Desktop backends expose supported capabilities.
- GUI and CLI use shared business logic.
- Data models should accommodate additional providers without changing the core image model.

---

## 10. Implementation Roadmap

### Phase 1 — Local library MVP

The goal is a useful offline wallpaper browser.

- [x] Rust application setup with GPUI and GPUI Kit
- [x] Settings persistence and library directory configuration
- [x] Recursive image discovery
- [x] SQLite index, rebuildable from the library
- [x] Content hashing and library metadata reconciliation
- [x] Asynchronous thumbnail generation and caching
- [x] Image grid
- [x] Image detail view
- [x] Filter by orientation and sort by date
- [x] Noctalia wallpaper integration
- [x] Highlight the current wallpaper
- [x] Set wallpaper
- [x] Delete images using system trash
- [x] Basic shared CLI functionality

### Phase 2 — Wallhaven

- [x] Wallhaven provider implementation
- [x] Recognize Wallhaven filenames
- [x] Snapshot provider metadata into the library
- [ ] Display provider tags
- [x] Search and browse Wallhaven
- [x] API-key configuration
- [ ] SFW/NSFW filtering
- [x] Download original images
- [ ] Detect already-downloaded images
- [x] Download destination rules

### Phase 3 — Organization and Git

- [ ] User-defined tags
- [ ] Tag autocomplete and multi-tag filtering
- [ ] Favorites
- [ ] Multi-selection and batch deletion
- [ ] Git status detection
- [ ] Git status indicators and filters
- [ ] Additional sorting and filtering options
- [ ] Reverse the sort order (e.g. oldest first, smallest first)

### Phase 4 — Enhancements

- [ ] Additional online providers
- [ ] Additional desktop backends
- [ ] Multi-monitor support
- [ ] Random wallpaper selection
- [ ] Wallpaper history
- [ ] Light/dark classification and color filtering
- [ ] Git synchronization operations
- [ ] Wallpaper review workflow
- [ ] Automatic wallpaper rotation, without requiring the GUI to remain open

---

## 11. Open Design Decisions

These questions can be resolved during implementation.

| Topic | Question |
|---|---|
| Downloaded indicators | Highlight, dim, hide, or provide a toggle? |
| Content ratings | Should filtering apply to local images as well as online browsing? |
| Multi-monitor | How should images be highlighted if different monitors use different wallpapers? |
| Tag filtering | Should matching default to AND or OR? |
| Git integration | How should untracked and deleted images appear in the gallery? |
| Review workflow | Is a dedicated keep/discard mode more useful than ordinary batch deletion? |
| Folder changes | How should changing the library root affect existing metadata and cache records? |

---

## 12. Initial Release Criteria

The first usable version of Backdrop should allow a user to:

1. Select a directory containing wallpapers.
2. Browse its images in a responsive thumbnail grid.
3. Open an image to view its metadata and a larger preview.
4. Filter images by orientation and sort by date.
5. See which image is currently being used as the desktop wallpaper.
6. Set a different wallpaper through the supported Noctalia integration.
7. Delete unwanted images safely.
8. Restart the application without requiring a full rebuild of its thumbnail cache.

This establishes a useful core before introducing online providers, advanced metadata management, and Git functionality.
