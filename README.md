# sip-fax-rs

English | [繁體中文](README.zh-TW.md)

[![CI](https://github.com/leoyu0607/sip-fax-rs/actions/workflows/ci.yml/badge.svg)](https://github.com/leoyu0607/sip-fax-rs/actions/workflows/ci.yml)

A SIP fax server written in Rust. It receives faxes from VoIP gateways (Sonus, AudioCodes, ...) over **T.38**, with **G.711 passthrough** as a fallback. It stores the results in a database and delivers them by email, webhook or folder.

> **Status:** alpha, in field testing (latest: [v0.1.1-alpha](https://github.com/leoyu0607/sip-fax-rs/releases/tag/v0.1.1-alpha)). **Receiving faxes over T.38 works** and has been verified with a real AudioCodes gateway: `sip-faxd serve` answers the call, switches to T.38, receives a multi-page fax (non-ECM, MH/MR, up to 14400 bit/s) and writes it as TIFF and PDF. In progress (M3, not released yet): **fax records in a database** (list with ANI/DNIS, hang-up reason, status while receiving, page count; retention), ECM receive (off by default), per-trunk resolutions; **delivery** to a folder, by e-mail or to a webhook (`[[routes]]`, with persistent retries); still to come in M3: PNG preview in the UI. Not yet: management UI, G.711 passthrough. See [CHANGELOG.md](CHANGELOG.md) and [docs/ROADMAP.md](docs/ROADMAP.md).

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

Besides the SIP settings, the file has `[database]` (`url`, `auto_migrate`, `max_connections`), `[retention]` (`files_days`, `records_days`, `run_at`), `[[routes]]` and `[delivery]` (see [Delivery](#delivery)) and, per trunk, `[trunks.fax]` (`resolutions`, `ecm`) and `[trunks.numbers]` (where the calling and called number come from: `ani_sources`, `dnis_sources`, `strip_plus`, `ani_strip_prefix`, `dnis_strip_prefix`).

## Fax records and the database

Every call is a row in the `faxes` table of the database named by `[database] url` (default: SQLite file `data/sip-faxd.db`; `postgres://`, `mysql://` / `mariadb://` and `mssql://` work too, see [docs/database.md](docs/database.md)). The row is created when the call starts (`fax_status = receiving`), its `pages` count goes up with every page the fax machine delivers, and it is completed when the call ends: the status (`received`, `partial`, `failed`, `no_fax`), who hung up and why (`hangup_direction`, `hangup_code`), the calling number (ANI) and called number (DNIS), the file paths and sizes, and the negotiated resolution, rate, coding and ECM. Calls from sources outside every trunk's allow-list are refused and not recorded.

**A database problem never costs a fax.** The fax and `result.json` are written as usual; the database writes wait in an in-memory queue and are retried with exponential backoff (1 s, 2 s, 4 s ... 60 s). A write that still fails is logged as an error, and `sip-faxd db backfill` restores it later from `logs/calls/*/result.json`. If the database is down when the server starts, the server starts anyway; a database that is *newer* than the program (or outdated while `auto_migrate = false`) stops the start.

Retention: `[retention]` is copied into the database on the **first** start; after that the database decides (the web UI of a later release edits it there). A job runs every day at `run_at` (server local time): it deletes the TIFF/PDF files after `files_days` (the record stays, marked "files purged") and the records after `records_days`. `0` means never.

| Command | What it does |
|---|---|
| `sip-faxd db migrate` | Create or upgrade the schema (safe to run from several hosts at once; MySQL/MariaDB migrations can be re-run after a failure) |
| `sip-faxd db backfill [--dir DIR]` | Insert the calls found in `logs/calls/*/result.json` that are missing from the database |
| `sip-faxd purge [--dry-run]` | Run the retention job now; `--dry-run` only counts |

## Delivery

With `[[routes]]` in the configuration, every fax that ends as `received` is delivered as soon as its record is in the database (a fax that broke off, `partial`, only when the route says `deliver_partial = true`; `failed` and `no_fax` never). Routes are tried in order and the **first match** wins; a route matches on `dnis` (exact), `dnis_prefix` and `trunk` (all given conditions must hold), or `default = true` for everything else (last route). Each route has one or more deliveries:

```toml
[[routes]]
dnis_prefix = "2557"
[[routes.deliveries]]
kind = "folder"                     # PDF and TIFF, written atomically
dir = "/srv/fax-inbox"
filename = "{date}_{ani}_{id}"
[[routes.deliveries]]
kind = "email"                      # PDF attached; STARTTLS (default), SMTPS or none; AUTH
host = "smtp.example.com"
username = "fax"
password = "secret"
from = "fax@example.com"
to = ["ops@example.com"]
[[routes.deliveries]]
kind = "webhook"                    # JSON POST; optional HMAC-SHA256 signature and PDF upload
url = "https://example.com/fax-hook"
secret = "hmac-key"                 # X-Sipfax-Signature: sha256=<hex of the body>
```

* A failed delivery is retried after 1, 5, 15 and 60 minutes, then every 4 hours, up to `[delivery] max_attempts` (default 10) times, then it is given up. The queue is the `delivery_attempts` table (one row per attempt), so a restart continues where it stopped. A delivery problem, or a database that is down, never affects receiving faxes or the fax record. Delivery is at-least-once.
* `[delivery] catch_up_hours` (default 24): at start, finished faxes of that many hours that have no delivery yet are queued. This repairs a crash between "stored" and "queued" and delivers calls restored by `sip-faxd db backfill` (backfill itself never delivers, so restoring old records never floods a mailbox; calls older than the window stay undelivered). `0` turns it off.
* `sip-faxd config check` lists the routes; passwords, webhook secrets and credentials or query values in URLs are shown as `***`.
* A permanently failing delivery (a 4xx answer other than 408/425/429, SMTP 5xx, a bad destination) is not retried; after fixing the cause an administrator has to re-deliver (a later release adds a button for that).

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
