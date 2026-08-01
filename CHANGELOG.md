# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.2.2](https://github.com/bbaldino/homelab-health/compare/v0.2.1...v0.2.2) - 2026-08-01

### Fixed

- widen the monitor modal when it has a rule list

## [0.2.1](https://github.com/bbaldino/homelab-health/compare/v0.2.0...v0.2.1) - 2026-07-31

### Fixed

- readable, aligned prometheus rule-builder rows

## [0.2.0](https://github.com/bbaldino/homelab-health/compare/v0.1.1...v0.2.0) - 2026-07-31

### Added

- metric/label autocomplete in the prometheus rule builder
- UI list-of-objects field kind and options dropdowns
- prometheus inspect endpoint for metric/label autocomplete
- prometheus check evaluation, schema, and registration
- prometheus check core — config, rules, matcher parser, scrape
- add List field kind and options to config schema

### Fixed

- normalize existing list-field rows to form values on load

### Other

- prometheus metrics check implementation plan
- prometheus metrics check design spec

## [0.1.1](https://github.com/bbaldino/homelab-health/compare/v0.1.0...v0.1.1) - 2026-07-31

### Added

- expose running version on /api/v1/version
