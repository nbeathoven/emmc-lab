# Changelog

All notable changes to `emmc-lab` are documented in this file.

## v0.2.1 - 2026-04-04

- added a shared terminal UI renderer for session reports, health, doctor, settings, and device listings
- standardized stats output around width-aware ASCII tables with ANSI headings and emphasis when the terminal supports color
- improved truncation and inline sanitization so long paths and multiline values remain readable over SSH
- promoted real block devices ahead of loop and ram devices in formatted device and diagnostic summaries

## v0.2.0 - 2026-04-04

- expanded the root README into a complete project overview with install, workflow, safety, reporting, and versioning sections
- added explicit release/versioning documentation and GitHub release links
- added Raspberry Pi self-deploy install documentation to the main project entrypoint
- published the first structured GitHub release flow for the repository

## v0.1.0 - 2026-04-04

- initial single-binary Rust implementation
- interactive menu and wizard
- file and raw-target workload execution engine
- procfs live sampler and deep-trace fallback entrypoint
- health, doctor, reporting, examples, and Raspberry Pi installer
