# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- Initial crate scaffold with 8-table schema (workspaces, projects, states, items, labels, webhooks, assets, project_sequences) + 3 junction tables (item_labels, item_assignees, item_dependencies)
- CRUD for all entities with soft-delete, partial unique indexes, UUIDv7 primary keys
- State machine with 6 fixed groups + custom names, seeded on project creation
- Per-project auto-increment sequence IDs
- Error type with NotFound, Duplicate, InvalidTransition variants
