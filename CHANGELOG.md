# Changelog

All notable changes to `emmc-lab` are documented in this file.

## v0.2.4 - 2026-04-12

- made shared tables and the live monitor respect the active terminal width with an 80-column fallback when dimensions cannot be queried
- switched truncation sentinels to `…` so clipped paths and long values stay visibly truncated over narrow SSH sessions
- added `--color=auto|always|never` support while continuing to disable ANSI output when `NO_COLOR` is set or stdout is not a TTY
- added inline invalid-choice feedback and direct numeric row selection for managed menus so bad input no longer looks like a hang
- echoed accepted wizard defaults and typed answers back to the operator before advancing to the next prompt
- reflowed the live monitor after `SIGWINCH` and moved deep-trace fallback notices to stderr so redirected stdout stays clean

## v0.2.3 - 2026-04-06

- added a one-sector wizard preset for exact-count logical-sector read and write runs
- kept old session reports readable when diagnostics fields are missing from earlier JSON schema versions
- improved live monitor attribution visibility for restricted and kernel-like activity
- kept live monitor headers visible on shorter terminals by tightening layout thresholds
- improved operator-facing size prompts with grouped exact values in brackets
- fixed wizard block-size alignment for sector-range runs so invalid defaults are corrected before execution
- expanded the root README with direct usage examples for monitor, health, exports, and one-sector runs

## v0.2.2 - 2026-04-04

- reduced wizard friction with default answers, typed target discovery, smarter assumptions, and range-aware prompts
- added runtime discovery of MMC and SD target type, size, and logical sector size for device pickers and suggestions
- prevented invalid file targets and late direct-I/O failures by normalizing directory paths and aligned block sizes during setup

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
