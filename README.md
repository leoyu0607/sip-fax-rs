# sip-fax-rs

English | [繁體中文](README.zh-TW.md)

[![CI](https://github.com/leoyu0607/sip-fax-rs/actions/workflows/ci.yml/badge.svg)](https://github.com/leoyu0607/sip-fax-rs/actions/workflows/ci.yml)

A SIP fax server written in Rust. It receives faxes from VoIP gateways (Sonus, AudioCodes, ...) over **T.38**, with **G.711 passthrough** as a fallback. It stores the results in a database and delivers them by email, webhook or folder.

> **Status:** alpha, in field testing (latest: [v0.1.1-alpha](https://github.com/leoyu0607/sip-fax-rs/releases/tag/v0.1.1-alpha)). **Receiving faxes over T.38 works** and has been verified with a real AudioCodes gateway: `sip-faxd serve` answers the call, switches to T.38, receives a multi-page fax (non-ECM, MH/MR, up to 14400 bit/s) and writes it as TIFF and PDF. In progress (M3, not released yet): **fax records in a database** (list with ANI/DNIS, hang-up reason, status while receiving, page count; retention), ECM receive (off by default), per-trunk resolutions; still to come in M3: PNG preview and delivery (email/webhook/folder). Not yet: management UI, G.711 passthrough. See [CHANGELOG.md](CHANGELOG.md) and [docs/ROADMAP.md](docs/ROADMAP.md).

## Features (planned)

- **Fax-in first.** Fax-out will come later.
- **T.38** (UDPTL with redundancy/FEC) is the default transport. **G.711 passthrough** is the fallback.
- The fax protocol stack is **pure Rust**: T.38 IFP/UDPTL, T.30, T.4/T.6 (MH/MR/MMR), and TIFF/PDF output.
- **Databases:** SQLite is the default. MySQL/MariaDB, PostgreSQL and Microsoft SQL Server are also supported.
- **Delivery:** stored file plus DB record, email (SMTP), HTTP webhook, or a folder.
- **Interfaces:** a simple CLI, a TUI, and a web management UI (an embedded SPA).
- **Deployment:** one statically linked binary (musl). Unpack the tarball and run it on RHEL 9/10. No dependencies to install.

## Architecture

The project has three main modules:

| Module | Crate(s) | Description |
|---|---|---|
| SIP core engine | `sip-core` | Standalone SIP stack: messages, UDP/TCP transport, RFC 3261 transactions, dialogs, SDP with T.38. It is designed to become an independent project later. Parts are derived from [Sipress](https://github.com/leoyu0607/Sipress). |
| Fax-in | `fax-in`, `t38`, `t30`, `t4`, `fax-modem` | Inbound call control and the fax protocol stack |
| Fax-out | — | Future |

Supporting crates:
- `store`: database layer
- `delivery`: email, webhook and folder delivery
- `web`: management API and UI

The `sip-faxd` binary ties everything together.

See [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) for details.

## Quick start (once released)

```sh
tar xzf sip-fax-rs-<version>-linux-x86_64.tar.gz
cd sip-fax-rs-<version>-linux-x86_64
./sip-faxd serve
```

## Configuration

`sip-faxd` reads a TOML file (default: `conf/sip-faxd.toml` next to the executable; override with `--config`). [`conf/sip-faxd.toml.example`](conf/sip-faxd.toml.example) documents every option: the SIP listener, per-peer `[[trunks]]` profiles (presets `generic` and `cht-cs210`, source-IP allow-lists, T.38 policy, optional registration), per-call SIP/UDPTL capture and the fax output directory.

```sh
./sip-faxd config check --config conf/sip-faxd.toml   # validate (shows database, retention, fax and number settings)
./sip-faxd serve --config conf/sip-faxd.toml          # run
```

Besides the SIP settings, the file has `[database]` (`url`, `auto_migrate`, `max_connections`), `[retention]` (`files_days`, `records_days`, `run_at`) and, per trunk, `[trunks.fax]` (`resolutions`, `ecm`) and `[trunks.numbers]` (where the calling and called number come from: `ani_sources`, `dnis_sources`, `strip_plus`, `ani_strip_prefix`, `dnis_strip_prefix`).

## Fax records and the database

Every call is a row in the `faxes` table of the database named by `[database] url` (default: SQLite file `data/sip-faxd.db`; `postgres://`, `mysql://` / `mariadb://` and `mssql://` work too, see [docs/database.md](docs/database.md)). The row is created when the call starts (`fax_status = receiving`), its `pages` count goes up with every page the fax machine delivers, and it is completed when the call ends: the status (`received`, `partial`, `failed`, `no_fax`), who hung up and why (`hangup_direction`, `hangup_code`), the calling number (ANI) and called number (DNIS), the file paths and sizes, and the negotiated resolution, rate, coding and ECM. Calls from sources outside every trunk's allow-list are refused and not recorded.

**A database problem never costs a fax.** The fax and `result.json` are written as usual; the database writes wait in an in-memory queue and are retried with exponential backoff (1 s, 2 s, 4 s ... 60 s). A write that still fails is logged as an error, and `sip-faxd db backfill` restores it later from `logs/calls/*/result.json`. If the database is down when the server starts, the server starts anyway; a database that is *newer* than the program (or outdated while `auto_migrate = false`) stops the start.

Retention: `[retention]` is copied into the database on the **first** start; after that the database decides (the web UI of a later release edits it there). A job runs every day at `run_at` (server local time): it deletes the TIFF/PDF files after `files_days` (the record stays, marked "files purged") and the records after `records_days`. `0` means never.

| Command | What it does |
|---|---|
| `sip-faxd db migrate` | Create or upgrade the schema (safe to run from several hosts at once; MySQL/MariaDB migrations can be re-run after a failure) |
| `sip-faxd db backfill [--dir DIR]` | Insert the calls found in `logs/calls/*/result.json` that are missing from the database |
| `sip-faxd purge [--dry-run]` | Run the retention job now; `--dry-run` only counts |

## Receiving faxes

Each received fax is written to

```
data/faxes/<YYYY>/<MM>/<DD>/<call-id>.tif    multi-page TIFF Class F (CCITT G4)
data/faxes/<YYYY>/<MM>/<DD>/<call-id>.pdf    the same pages as a PDF
```

(the date is the UTC date of the call; the directory is `[storage] data_dir`, by default `data/` next to the executable). A fax that broke off after some pages is still written and marked `"partial": true` in its result. With capture enabled (the default), `logs/calls/<call-id>/` holds `sip.log` (every SIP message), `udptl.log` (every T.38 datagram, one line each: a summary with the T.30 frames in hex but never the image data; `[capture] udptl = "full"` records complete datagrams and therefore the fax content) and `result.json` (the call result: T.30 result code, pages, bit rate, resolution, coding, sender's TSI, bad lines per page, file paths, UDPTL counters).

## Development

```sh
cargo build
cargo test --workspace
scripts/ci-local.sh        # same checks as CI; run before every push
scripts/e2e-docker.sh      # SpanDSP end-to-end tests in a Rocky 9 container (Linux, macOS, Windows)
scripts/db-matrix.sh       # the store tests on PostgreSQL, MySQL, MariaDB and MSSQL in containers
scripts/musl-docker.sh     # the static musl build (zig 0.14.1) in a container; ci-local.sh uses it when needed
```

Testing uses [SpanDSP](https://github.com/freeswitch/spandsp) to simulate inbound faxes from a gateway. SpanDSP is used only in the test tooling (`tools/fax-sim`) and is never linked into the shipped binary. Every push must pass CI.

Further reading:
- [CLAUDE.md](CLAUDE.md): the entry point for AI agents and contributors
- [docs/](docs/): goals, architecture, roadmap, TODO and decisions

## License

[MIT](LICENSE)
