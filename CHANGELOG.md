# Release notes

## 1.0.3 — 2026-11-08

### Added

- GitHub release archives for Linux x86_64, Linux ARM64, and Apple Silicon macOS,
  including the server, journal CLI, simulator, documentation, and SHA-256 checksums.
- Multi-platform release container images published to GHCR.
- Importable Grafana dashboard for the exported Prometheus metrics.
- Running server version displayed in the dashboard and MQTT discovery metadata.
- BSD 3-Clause license.

### Changed

- `sigen-journal` accepts the journal path as an optional positional argument.
- Server, journal CLI, and simulator report version `1.0.3`.

## 1.0.2 — 2026-11-01

### Added

- Persistent SQLite event journal for server lifecycle, hardware connectivity,
  alert transitions, alert settings changes, and control requests and outcomes.
- Filterable dashboard timeline and paginated `GET /api/events` endpoint.
- `sigen-journal` CLI with category filters, limits, cursor pagination, and JSONL
  output. It reads an existing journal directly, including while the server is stopped.
- Control request IDs shared between journal events and the Modbus write audit.
- Journal health in server status and Prometheus metrics.

### Changed

- Graceful shutdown waits for hardware work and flushes the event journal.
- Intentional shutdown no longer generates a hardware-offline alert.
- Comments explain threading, stale-data handling, command cancellation, and
  other non-obvious behavior.
- Server, journal CLI, and simulator report version `1.0.2`.
- `sigen-journal` identifies itself correctly in help and version output.

### Operational notes

- The server now creates `sigen-events.sqlite3` in its working directory. Set
  `SIGEN_JOURNAL_FILE` or `--journal-file` to choose a different path in an existing
  writable directory. The NixOS service uses `/var/lib/sigen-server/events.sqlite3`.
- Events are retained indefinitely. Back up using SQLite's online backup facility,
  or stop the server cleanly before copying the database.
- Database initialization errors prevent startup. Runtime journal failures are
  reported without stopping hardware controls. A crash can lose events still queued
  in memory; the existing synchronous Modbus write audit remains separate.
- Prometheus remains the telemetry-history store; the journal records events.
