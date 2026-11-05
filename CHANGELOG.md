# Changelog

All notable changes to this project are documented in this file. The project
follows [Semantic Versioning](https://semver.org/spec/v2.0.0.html).


## [0.1.1](https://github.com/dbrockman/backdrop/compare/v0.1.0...v0.1.1) - 2026-11-03

### Changed

- Adapt to GPUI Kit 0.9.0 and update its skills
- Move the library folder choice into the settings
- Move the Wallhaven API key into the settings and apply the design review

## [0.1.0](https://github.com/dbrockman/backdrop/releases/tag/v0.1.0) - 2026-11-03

### Added

- Add settings persistence and library folder selection
- Add image discovery and the SQLite library index
- Add library metadata and follow moved images by hash
- Add favorites to the CLI
- Add the image grid with cached thumbnails
- Add the full-window image detail view
- Add sorting and orientation and favorites filters
- Add the info command to the CLI
- Add the Wallhaven provider and its CLI commands
- Show provider details and tags in the image detail view
- Show Wallhaven results in full with their details and tags
- Add user tags to the core and the CLI
- Add theme settings with system appearance and a watched themes folder

### Changed

- Set up the Rust workspace with GPUI Kit and a CLI
- Set and show the desktop wallpaper through Noctalia
- Delete images by moving them to the system trash
- Download Wallhaven wallpapers into the library
- Choose download folders with rules stored in the library
- Recognize and link existing Wallhaven files
- Browse and download Wallhaven wallpapers in the GUI
- Configure the Wallhaven API key from the GUI and check keys
- Filter Wallhaven results by content rating
- Reuse the full image when downloading from the detail view
- Hide Wallhaven results the library already has, on request
- Keep letter shortcuts from swallowing typed text
- Edit user tags in the image detail view
- Filter the library grid by tags
- Select several library images and delete them together
- Filter by provider tags right after getting an image's details
- Search Wallhaven for a tag clicked in the details view
