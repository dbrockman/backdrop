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
- **Portable:** User-created metadata should eventually be transferable between installations.
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
- Index image metadata in a local SQLite database.
- Generate and cache thumbnails.
- Reconcile the index with the filesystem when the application starts.
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

### 3.4 Detecting previously downloaded wallpapers

Backdrop should recognize when a remote wallpaper already exists in the local library.

For Wallhaven, filenames following this convention can identify the source image:

`wallhaven-{ID}.{extension}`

For example:

`wallhaven-94x38z.jpg`

The source association should also be stored in the metadata database so it does not depend exclusively on the filename.

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

Settings should define a default download directory and optionally a set of ordered rules.

Each matching download rule should resolve to a directory within the configured library root.

**Open decision:** Define whether the first matching rule wins or whether rules can combine to construct nested directories, such as `nsfw/dark/`.

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

### 4.3 Initial backend: Niri

The first implementation targets Niri, using the wallpaper service configured in the user's session.

Niri itself is a compositor; the active wallpaper may be managed by another program. Backdrop therefore needs to integrate with the relevant wallpaper service rather than assuming Niri directly owns wallpaper state.

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

### 5.1 SQLite database

Backdrop uses SQLite for local indexing and cached metadata.

The database should store:

- Indexed image records
- File paths and filesystem information
- Image dimensions and other technical metadata
- Provider associations
- Cached provider metadata
- Provider tags
- User-defined tags
- Favorites
- Thumbnail references
- Indexing state

The database should live outside the wallpaper directory, using an appropriate XDG application data location.

### 5.2 Separate metadata categories

Maintain a clear separation between three categories of information.

**Filesystem metadata**

Information derived from local image files, such as paths, dimensions, modification times, and file size.

**Provider metadata**

Information retrieved from online APIs, including provider IDs, tags, publication dates, and content ratings.

This information can be cached and regenerated.

**User metadata**

Information explicitly created or modified through Backdrop, such as favorites, custom tags, and future organization preferences.

This information should be preserved and preferably portable.

### 5.3 Portable metadata

Consider storing user-created metadata in human-readable text files within the wallpaper library.

This would allow user metadata to be version-controlled and synchronized alongside the images.

Possible structure:

```text
Wallpapers/
  wallhaven-94x38z.jpg
  mountain.png
  dark/
    another-wallpaper.jpg
  .backdrop/
    metadata.json
```

The portable metadata should store only information that must survive rebuilding the local database.

Provider cache data, thumbnails, transient UI state, and secrets should not be written into the wallpaper repository.

**Open decision:** Choose between one metadata file for the collection, multiple per-directory files, or per-image sidecar files.

The implementation should account for renamed or moved files and avoid losing user metadata when file paths change.

### 5.4 Thumbnail caching

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

- Default download directory
- Download subdirectory rules
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
      niri/
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

Possible commands:

```bash
backdrop
backdrop list
backdrop info <image>
backdrop set <image>
backdrop set --random
backdrop scan
backdrop git status
```

Future commands could provide provider search, downloads, filtering, and metadata editing.

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
- Database can be rebuilt without losing image files.
- User-created metadata should remain recoverable.
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

- [ ] Rust application setup with GPUI and GPUI Kit
- [ ] Settings persistence and library directory configuration
- [ ] Recursive image discovery
- [ ] SQLite database and image index
- [ ] Asynchronous thumbnail generation and caching
- [ ] Image grid
- [ ] Image detail view
- [ ] Filter by orientation and sort by date
- [ ] Niri wallpaper integration
- [ ] Highlight the current wallpaper
- [ ] Set wallpaper
- [ ] Delete images using system trash
- [ ] Basic shared CLI functionality

### Phase 2 — Wallhaven

- [ ] Wallhaven provider implementation
- [ ] Recognize Wallhaven filenames
- [ ] Retrieve and cache remote metadata
- [ ] Display provider tags
- [ ] Search and browse Wallhaven
- [ ] API-key configuration
- [ ] SFW/NSFW filtering
- [ ] Download original images
- [ ] Detect already-downloaded images
- [ ] Download destination rules

### Phase 3 — Organization and Git

- [ ] User-defined tags
- [ ] Tag autocomplete and multi-tag filtering
- [ ] Favorites
- [ ] Multi-selection and batch deletion
- [ ] Git status detection
- [ ] Git status indicators and filters
- [ ] Portable user metadata
- [ ] Additional sorting and filtering options

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
| Portable metadata | Single collection file, per-directory files, or sidecars? |
| Metadata identity | How should custom metadata survive renaming or moving an image? |
| Download rules | First matching rule or composable directory rules? |
| Downloaded indicators | Highlight, dim, hide, or provide a toggle? |
| Content ratings | Should filtering apply to local images as well as online browsing? |
| Niri integration | Which wallpaper services should the initial backend support? |
| Multi-monitor | How should images be highlighted if different monitors use different wallpapers? |
| Tag filtering | Should matching default to AND or OR? |
| Git integration | How should untracked and deleted images appear in the gallery? |
| CLI | Which functionality should be available in the first release? |
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
6. Set a different wallpaper through the supported Niri integration.
7. Delete unwanted images safely.
8. Restart the application without requiring a full rebuild of its thumbnail cache.

This establishes a useful core before introducing online providers, advanced metadata management, and Git functionality.
