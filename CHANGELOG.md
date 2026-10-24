# Changelog

All notable changes to Prime Desktop. The format follows [Keep a Changelog](https://keepachangelog.com/).

## [Unreleased]

### Added

- A Tauri v2 host with a Rust core, driving the Rust Prime Agent through its daemon protocol, and a sandbox for the agent's code. The Electron app is unchanged for now.

### Changed

- CI builds on macOS 26 (Apple silicon and Intel), Windows Server 2025 and Ubuntu 24.04, and runs the sandbox tests as well as the host's.
- Releases: `npm run release` bumps the version everywhere, dates this changelog and tags it; the tag builds the installers and publishes them with the matching section of this file as the release notes.

### Fixed

- The pinned Rust toolchain now installs rustfmt and clippy, which stopped every CI job at the first step.
- The terminal keeps up with a command that prints a lot. When the page was slow to take output, the terminal passed it on one read at a time, and on macOS that is often one line, so it fell steadily behind and its queue kept growing. It now passes on everything already waiting, in batches.
