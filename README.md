# sip-fax-rs

English | [繁體中文](README.zh-TW.md)

[![CI](https://github.com/leoyu0607/sip-fax-rs/actions/workflows/ci.yml/badge.svg)](https://github.com/leoyu0607/sip-fax-rs/actions/workflows/ci.yml)

A SIP fax server written in Rust. It receives faxes from VoIP gateways (Sonus, AudioCodes, ...) over **T.38**, with **G.711 passthrough** as a fallback. It stores the results in a database and delivers them by email, webhook or folder.

> **Status:** alpha, in field testing (latest: [v0.3.0-alpha](https://github.com/leoyu0607/sip-fax-rs/releases/tag/v0.3.0-alpha)). **Receiving faxes over T.38 works** and has been verified with a real AudioCodes gateway: `sip-faxd serve` answers the call, switches to T.38, receives a multi-page fax (MH/MR/MMR, up to 14400 bit/s; ECM available but off by default) and writes it as TIFF and PDF. Every call is **recorded in a database** (SQLite by default; PostgreSQL, MySQL/MariaDB, MSSQL) with ANI/DNIS, hang-up reason and status, files and records are cleaned up after configurable periods, and faxes can be **delivered** to a folder, by e-mail or to a webhook with persistent retries. A **web interface** built into the binary (HTTPS on port 8443, Traditional Chinese and English) lists the faxes live as they arrive, shows and downloads them, keeps read flags and notes, and lets administrators change the settings (stored in the database, applied without a restart) and the accounts; `sip-faxd start|stop|status|logs` and a TUI manage the service from the shell. Not yet: G.711 passthrough (M5). See [CHANGELOG.md](CHANGELOG.md) and [docs/ROADMAP.md](docs/ROADMAP.md).

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

The configuration file (default: `conf/sip-faxd.toml` next to the executable; override with `--config`) holds the **startup settings only**: `[database]`, `[web]`, `[storage]`, `[log]` and `[service]` ([`conf/sip-faxd.toml.example`](conf/sip-faxd.toml.example)). With the built-in SQLite nothing has to be changed. Another database is written field by field, the password as it is (no percent-encoding), or from a file:

```toml
[database]
type = "mssql"                  # sqlite | postgres | mysql | mariadb | mssql
host = "10.1.2.3"
name = "faxdb"
user = "fax"
password = "p@ss:w/rd#1"        # or password_file = "conf/db-password"
trust_server_certificate = true
```

```sh
./sip-faxd config check     # validate the file (passwords shown as ***)
./sip-faxd db check         # test the database connection: server version, schema; changes nothing
./sip-faxd serve            # run in the foreground (or start / stop / status, below)
```

**Everything else lives in the database and is set in the web interface** (Settings): the SIP listener and the address we advertise, per-peer trunks (presets `generic` and `cht-cs210`, source-IP allow-lists, T.38 policy, registration, fax capabilities, where the numbers come from), call handling, per-call capture, delivery routes and retention (ADR-051, ADR-060). The first start stores the defaults, without any trunk: calls are refused until a trunk is added (the web interface and the log say so). Changes apply to the next call without a restart, except the listen address, the UDPTL port range and the capture. Secrets (registration, e-mail and webhook passwords) are stored encrypted with `conf/secret.key`. An older configuration file that still has these sections keeps working: they are imported once, while the database has none, and can then be removed.

```sh
./sip-faxd config export [--include-secrets]      # the settings of the database as TOML (a backup, a template)
./sip-faxd config import [--force] --file PATH    # set them by script (conf/runtime.toml.example documents every option)
./sip-faxd user add alice --role agent            # accounts: add | passwd | disable | enable | list
./sip-faxd fax list --unread                      # faxes: list | show | read | note | redeliver
```

The first start creates the administrator `admin` with a random password in `conf/initial-admin-password` (it has to be changed at the first login).

## Running it as a service (Linux)

```sh
./sip-faxd start              # start in the background, wait until it is ready, then show the log
./sip-faxd start -d           # start only
./sip-faxd status             # pid, version, uptime, listeners, calls in progress, trunk registrations
./sip-faxd logs -f -n 100     # the last 100 log lines, then follow (also across log rotation)
./sip-faxd stop               # wait for the faxes in progress, then stop
./sip-faxd stop --force       # end the calls now
./sip-faxd restart            # stop (waits for the calls), then start
```

- **`start`** runs `sip-faxd serve --daemonized` in a session of its own (`setsid`), writes `run/sip-faxd.pid` (locked with `flock`, so a second start is refused and says which pid runs) and waits until the control socket `run/sip-faxd.sock` answers. Ctrl+C only stops the log display ("the service is still running"); it never stops the service. If the service does not become ready within `[service] start_timeout_secs` (30), or exits during the start, `start` prints the last 50 log lines, exits non-zero and leaves no PID file.
- **`stop`** is graceful: new INVITEs are answered **503 at once**, the faxes in progress are **waited for** (at most `[service] stop_timeout_secs`, default 300; a progress line every 5 s), then what is left is ended. The pages received so far are written and the record is `partial` with `hangup_code = shutdown`. `--force` skips the wait, `--timeout SECS` overrides the configured one. Then the delivery service is closed as usual (see [Delivery](#delivery)). `SIGTERM` to `serve` is the same stop; a second `SIGTERM` or Ctrl+C forces it.
- **`status`** exits with `0` when the service runs and `3` when it does not.
- **systemd:** use the foreground `serve` in the unit (`ExecStart=/opt/sip-faxd/sip-faxd serve`, `TimeoutStopSec=` above `stop_timeout_secs`). When systemd manages the service (`INVOCATION_ID` is set, or the PID file's process is a child of PID 1 with a `NOTIFY_SOCKET`), `start`, `stop` and `restart` say "use `systemctl ...`" and exit non-zero; `--ignore-systemd` overrides that.
- **Log file:** `logs/sip-faxd.log`, rotated by size (`[log] max_size_mb`, default 50) or every day, `[log] keep` (14) old files are kept and gzip-compressed. A foreground `serve` writes the file and stderr.
- **Control socket:** one JSON object per line (`status`, `calls`, `stop`, `reload`, `subscribe`), mode 0600; whoever can open it is an administrator. The TUI and the `reload` after a settings change use it.

- **`run/` must be on a local file system:** `flock` does not lock on some network file systems (and on Docker Desktop bind mounts), and then a running service looks stopped.

### TUI

```sh
./sip-faxd tui                # on the server itself
```

The list of the latest faxes (live), their details with the delivery attempts, the calls in progress and the service status. Keys: `Tab` switches between faxes, calls and status; `↑`/`↓` or `j`/`k` move; `Enter` opens the details, `Esc` goes back; `u` shows only unread faxes; `r` marks the selected fax read or unread; `n` edits its note (`Enter` saves, `Esc` cancels); `q` quits. The fax data comes from the database (changes are recorded as `cli:<os user>`, like the `fax` command), the calls and the status from the control socket: without a running service the TUI says so and still works on the database. The language follows `LANG` (`zh_TW…` → Traditional Chinese, otherwise English).

These commands need a unix system (Linux; macOS works for testing) because of flock, setsid and the unix socket. On other systems only `serve` is available.

## Web interface

On by default (also without a `[web]` table in the file): HTTPS on `0.0.0.0:8443`. Without a certificate of your own a self-signed one is made in `conf/tls/` at the first start (825 days, renewed 30 days before it ends); the start log shows its SHA-256 fingerprint so you can compare it with the browser's warning. At the first start the password of `admin` is in `conf/initial-admin-password`; it has to be changed at the first login. Turn the interface off with `[web] enabled = false`; see `[web]` in `conf/sip-faxd.toml.example` for the address, `http`, your own certificate and the session time. Open `https://<server>:8443/` in a browser: the fax list with live receiving progress, the fax pages (preview, PDF/TIFF download, read flag, notes, deliveries), and for administrators the settings, accounts and audit log, in Traditional Chinese or English. The API is under `/api/v1`. [docs/web.md](docs/web.md) describes the pages (in Traditional Chinese). Building from source: run `npm ci && npm run build` in `web-ui/` before `cargo build`, or the binary shows a notice page instead of the interface.

PNG previews of every page are written next to the TIFF and the PDF when a fax is received (`<name>-p1.png`, `<name>-p1-thumb.png`, ...) and are deleted with them.
## Fax records and the database

Every call is a row in the `faxes` table of the database of `[database]` (default: SQLite file `data/sip-faxd.db`; PostgreSQL, MySQL / MariaDB and SQL Server work too, see [docs/database.md](docs/database.md)). The row is created when the call starts (`fax_status = receiving`), its `pages` count goes up with every page the fax machine delivers, and it is completed when the call ends: the status (`received`, `partial`, `failed`, `no_fax`), who hung up and why (`hangup_direction`, `hangup_code`), the calling number (ANI) and called number (DNIS), the file paths and sizes, and the negotiated resolution, rate, coding and ECM. Calls from sources outside every trunk's allow-list are refused and not recorded.

**A database problem never costs a fax.** The fax and `result.json` are written as usual; the database writes wait in an in-memory queue and are retried with exponential backoff (1 s, 2 s, 4 s ... 60 s). A write that still fails is logged as an error, and `sip-faxd db backfill` restores it later from `logs/calls/*/result.json`. If the database is down when the server starts, the server starts anyway; a database that is *newer* than the program (or outdated while `auto_migrate = false`) stops the start.

Retention (web interface, Settings → Retention): A job runs every day at `run_at` (server local time): it deletes the TIFF/PDF files after `files_days` (the record stays, marked "files purged") and the records after `records_days`. `0` means never.

| Command | What it does |
|---|---|
| `sip-faxd db migrate` | Create or upgrade the schema (safe to run from several hosts at once; MySQL/MariaDB migrations can be re-run after a failure) |
| `sip-faxd db backfill [--dir DIR]` | Insert the calls found in `logs/calls/*/result.json` that are missing from the database |
| `sip-faxd purge [--dry-run]` | Run the retention job now; `--dry-run` only counts |
| `sip-faxd db check [--url URL [--password-stdin]]` | Test the connection: server version, schema version; changes nothing |
| `sip-faxd db copy --to URL` (or `--to-config FILE`) | Move to another database, also of another kind (e.g. SQLite to SQL Server): every table with its ids, into an empty database; the service must be stopped |

## Delivery

With delivery routes (web interface, Settings → Delivery routes; below in the file format of `config import`), every fax that ends as `received` is delivered as soon as its record is in the database (a fax that broke off, `partial`, only when the route says `deliver_partial = true`; `failed` and `no_fax` never). Routes are tried in order and the **first match** wins; a route matches on `dnis` (exact), `dnis_prefix` and `trunk` (all given conditions must hold), or `default = true` for everything else (last route). Each route has one or more deliveries:

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
