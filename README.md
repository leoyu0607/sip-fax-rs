# sip-fax-rs

English | [繁體中文](README.zh-TW.md)

[![CI](https://github.com/leoyu0607/sip-fax-rs/actions/workflows/ci.yml/badge.svg)](https://github.com/leoyu0607/sip-fax-rs/actions/workflows/ci.yml)

A SIP fax server written in Rust. It receives faxes from VoIP gateways (Sonus, AudioCodes, ...) over **T.38**, with **G.711 passthrough** as a fallback. It stores the results in a database and delivers them by email, webhook or folder.

> **Status:** early development (milestone M0). Nothing is usable yet. See [docs/ROADMAP.md](docs/ROADMAP.md).

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

`sip-faxd` reads a TOML file (default: `conf/sip-faxd.toml` next to the executable; override with `--config`). [`conf/sip-faxd.toml.example`](conf/sip-faxd.toml.example) documents every option: the SIP listener, per-peer `[[trunks]]` profiles (presets `generic` and `cht-cs210`, source-IP allow-lists, T.38 policy, optional registration), and per-call SIP capture.

```sh
./sip-faxd config check --config conf/sip-faxd.toml   # validate
./sip-faxd serve --config conf/sip-faxd.toml          # run
```

## Development

```sh
cargo build
cargo test --workspace
scripts/ci-local.sh        # same checks as CI; run before every push
```

Testing uses [SpanDSP](https://github.com/freeswitch/spandsp) to simulate inbound faxes from a gateway. SpanDSP is used only in the test tooling (`tools/fax-sim`) and is never linked into the shipped binary. Every push must pass CI.

Further reading:
- [CLAUDE.md](CLAUDE.md): the entry point for AI agents and contributors
- [docs/](docs/): goals, architecture, roadmap, TODO and decisions

## License

[MIT](LICENSE)
