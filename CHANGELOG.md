# Changelog

English | [繁體中文](#繁體中文)

All notable changes to this project are documented here. Versions follow [Semantic Versioning](https://semver.org/); `-alpha` releases are field-test builds.

## [Unreleased]
### Security
- `ctutils` (via `hmac` / `digest`, used for webhook signatures) updated to 0.4.4 ([RUSTSEC-2026-0385](https://rustsec.org/advisories/RUSTSEC-2026-0385): constant-time masks could compile to branches). 0.4.1 and earlier ship 0.4.2.

### Added
- **Faxes over G.711** (milestone M5): when a call cannot get T.38 the fax is received on the audio, with our own V.21, V.27ter (2400 / 4800) and V.29 (7200 / 9600) receivers; V.17 is not offered, so a G.711 fax runs at 9600 bit/s at most. It happens with `t38_policy = "audio-only"`, when the gateway refuses our T.38 re-INVITE (488 / 415 / 606, no usable `m=image`, a 2xx without SDP), and with `wait-peer` when no T.38 re-INVITE came within `initiate_after_secs`; a T.38 re-INVITE later still takes over. ECM is not used on the audio path. Calls that used to fail with `g711_passthrough_unsupported` now receive the fax.
- `result.json`: `audio` (codec, RTP port, why the fax went over audio), `fax.media` (`t38` / `g711`) and `fax.rtp` counters; `logs/calls/<call-id>/audio.log` (frames, modem changes, signal events).
- Database (migration 0003, applied at start): `faxes.modem` and `faxes.media`; shown on the fax page ("Transport") and sent in the webhook.
- Web interface: "Today" on the status page (faxes received today, unread, deliveries failed and given up) and the free disk space of the data directory; `GET /api/v1/faxes/counts?since=` and `storage` in `GET /api/v1/status`.
- Web interface: previous / next fax buttons on the fax page, in the order of the list.
- Web interface: leaving a page with an unsaved note or settings asks first.
- Web interface: the password can be shown on the sign-in page.

### Changed
- RTP of a call answered on audio uses the odd port right after the call's UDPTL port: open the whole `port_range` (even and odd ports) in the firewall. `[media] audio_port` is only used with `fax_session = "hold"`.
- Web interface: the live-updates dot is gone; a notice appears only when the live updates have been down for a few seconds.
- Web interface: an empty fax list says whether there are no faxes yet or none that match the filter (with a button to clear it); the note column takes the room left in the list; the fields of a form all have the same width.

### Fixed
- Web interface: a click outside the account menu right after opening it did not close it.

## [0.4.1-alpha] - 2026-10-23
Web interface improvements.

### Added
- Web interface: the number of unread faxes next to "Faxes" in the menu, live (`GET /api/v1/faxes/counts`).

### Changed
- Web interface: the language, the theme and signing out are in an account menu under your name.
- Web interface: a click anywhere on a fax row opens it; times in the list are short ("Today 14:05", "Yesterday", the date before that), with the full time on hover.
- Web interface: the fax page shows the details, the note and the deliveries beside the pages; the technical details are under "More details".
- Web interface: settings that need a restart have a ⟲ mark at their label, explained once at the top; the save bar is part of the form; the new account form opens with its button.

### Fixed
- Web interface: the buttons of the account list were out of line with their rows.

## [0.4.0-alpha] - 2026-10-23
Milestone M4e/M4f: moving to another database, simpler database settings, the configuration file holds the startup settings only.

### Added
- **`[database]` field by field**: `type`, `host`, `port`, `name`, `user`, `password` (written as it is, no percent-encoding) or `password_file`, `trust_server_certificate`; SQLite: `type = "sqlite"`, `path`. `url` still works.
- **`sip-faxd db check`**: test the database connection (server version, schema version, time); changes nothing, explains why a connection fails (host, password, missing database, certificate). `--url URL --password-stdin` tests another database.
- `db copy` takes the target's password from stdin (`--to-password-stdin`) or the target from another configuration file (`--to-config FILE`).
- A banner in the web interface while no trunk is configured.
- Web interface: a theme switch next to the language (system, light, dark), kept in the browser.
- **`sip-faxd db copy --to <URL>`**: move to another database, also of another kind (for example from the built-in SQLite to SQL Server). Every table is copied with its ids (faxes with read flags and notes, deliveries, settings, audit log, accounts, sessions) in one transaction into an empty target, then checked table by table. The service must be stopped; afterwards set `[database] url` to the target and keep `conf/secret.key`.

### Changed
- **The configuration file holds the startup settings only** (`[database]`, `[web]`, `[storage]`, `[log]`, `[service]`). SIP, trunks, routes and the other runtime settings are set in the web interface; the first start stores the defaults without any trunk (calls are refused until one is added; the log and the web interface say so). `conf/runtime.toml.example` documents them for `config import --file`. An older file that still has runtime sections keeps working: they are imported once, and the log and `config check` say they can be removed.

### Fixed
- Web interface: the uptime on the status page counts on every second (it stood still between two reads of the status).

## [0.3.0-alpha] - 2026-10-22
Milestone M4: the web interface, accounts, settings in the database, service management, TUI.

### Added
- **Settings in the database** (M4a): `[sip]`, `[media]`, `[call]`, `[capture]`, `[[trunks]]`, `[[routes]]`, `[delivery]` and `[retention]` are imported into the database at the first start and edited there afterwards (web interface, or `sip-faxd config import --force`). A change applies to the next call without a restart, except the listen address, the UDPTL port range and `[capture]`, which are listed as waiting for a restart. Every change is validated by the server and recorded in an audit log. Passwords of registrations, e-mail and webhooks are stored encrypted with `conf/secret.key`; `sip-faxd config export` prints the settings as TOML (secrets only with `--include-secrets`).
- **Accounts** (M4a): administrators and agents; passwords hashed with Argon2id, at least 10 characters; 5 wrong passwords lock an account for 15 minutes. At the first start an `admin` account is created with a random password in `conf/initial-admin-password`, to be changed at the first login. `sip-faxd user add|passwd|disable|enable|list`.
- **Fax commands** (M4a): `sip-faxd fax list [--unread]`, `show`, `read`, `note`, `redeliver` (queue failed or given-up deliveries again).
- **TUI** (M4d): `sip-faxd tui` shows the faxes (read flag and note editable), the calls in progress and the service status in a terminal, live while the service runs.
- **Service management** (M4d, Linux): `sip-faxd start [-d]`, `stop [--force] [--timeout SECS]`, `restart`, `status [--json]` (exit code 3 when not running) and `logs [-f] [-n N]`. `start` runs the service in the background (own session, PID file locked with `flock`, ready when the control socket answers) and shows the log; Ctrl+C only stops the display. systemd-managed services are detected and `start`/`stop`/`restart` point to `systemctl` (`--ignore-systemd` overrides). Use `serve` in a systemd unit.
- **Graceful stop**: `stop` and `SIGTERM` answer new INVITEs with 503, wait for the faxes in progress (`[service] stop_timeout_secs`, default 300) and only then end the rest; their pages are kept (`partial`, `hangup_code = shutdown`).
- Control socket `run/sip-faxd.sock` (mode 0600, one JSON object per line: `status`, `calls`, `stop`, `reload`, `subscribe`).
- Log file `logs/sip-faxd.log` with rotation by size or day, `[log] keep` files kept, gzip-compressed (`[log]`, `[service]` in the configuration).
- **Web interface server** (M4b): HTTPS on `0.0.0.0:8443` with a self-signed certificate (825 days, renewed 30 days before it ends; its SHA-256 fingerprint is logged) or your own; REST API under `/api/v1` for login, faxes (list, details, read flag, note, page previews, PDF/TIFF download, redelivery), configuration, accounts and status; live updates over a WebSocket (`/api/v1/events`).
- **PNG page previews are written with the fax**: next to `<name>.tif` and `<name>.pdf` come `<name>-p1.png` (whole page) and `<name>-p1-thumb.png` (300 px wide) for every page. They are deleted with the TIFF and the PDF.
- **Web interface** (M4c, guide in `docs/web.md`), built into the `sip-faxd` binary, in Traditional Chinese and English: the fax list (unread in bold, filters, search in numbers, remote ID and notes, endless scrolling, faxes being received shown live page by page without reloading), the fax page (thumbnails, zoomable page view, PDF/TIFF download, read/unread switch, notes with a warning when someone else changed them meanwhile, delivery status, redelivery), and for administrators the settings (SIP, media, calls, capture, trunks, delivery routes, delivery, retention; settings that need a restart are marked), accounts and the settings audit log; the service status for everyone. Usable with the keyboard alone.
- `GET /api/v1/faxes` takes `q`: text anywhere in the calling or called number, the remote ID or the note (ignoring case). `PUT /api/v1/auth/me` sets the account's own language. `GET /api/v1/config` lists the settings that need a restart (`restart_fields`).

### Changed
- **The web interface is on by default**, also when the configuration file has no `[web]` table: an upgraded server starts listening on TCP 8443. Open it in the firewall only for the networks that use it, or turn it off with `[web] enabled = false`.
- `[storage] preview_cache_mb` is no longer used (there is no separate preview cache any more); `config check` asks you to remove it.
- **Upgrading from 0.2.0**: at the first start of 0.3.0 the runtime sections of the configuration file are copied into the database; from then on editing them in the file has no effect (the start log warns about it). Back up `conf/secret.key` together with the database.

### Security
- tokio updated to 1.54.1 ([RUSTSEC-2026-0384](https://rustsec.org/advisories/RUSTSEC-2026-0384): a race in `tokio::time::Interval` on the multi-threaded runtime could crash the server; 0.2.0 shipped the affected 1.54.0).

## [0.2.0-alpha] - 2026-10-21
### Added
- **Fax records in a database** (M3): SQLite by default (built in, `data/sip-faxd.db`), or PostgreSQL, MySQL/MariaDB, Microsoft SQL Server. One row per call: ANI/DNIS (header sources configurable per trunk in `[trunks.numbers]`), hang-up direction and reason, status (receiving / received / partial / failed / no fax), live page count while receiving, file sizes, read/unread and notes (API for the coming UI).
- A database outage never stops fax reception: writes are queued and retried, and `sip-faxd db backfill` restores missing records from `logs/calls/*/result.json`.
- New commands: `sip-faxd db migrate`, `sip-faxd db backfill`, `sip-faxd purge [--dry-run]`. Migrations run automatically at start-up (`[database] auto_migrate`) and are safe when several instances start at once.
- **Retention**: files and records have separate retention periods (`[retention]`, stored in the database after the first start); a daily clean-up runs at `run_at`.
- **Delivery** (`[[routes]]`, `[delivery]`): copy to a folder, send by e-mail (SMTP with STARTTLS/SMTPS, rustls only) or POST to a webhook (JSON, optional HMAC-SHA256 signature and PDF attachment). Failed deliveries are retried with back-off (1, 5, 15, 60 min, then every 4 h) and survive restarts; faxes finished shortly before a restart are caught up. `config check` lists routes with secrets hidden.
- **ECM receive** (T.30 error correction) with MMR (T.6); **off by default** (`[trunks.fax] ecm = false`) until verified with a real gateway.
- Per-trunk resolutions announced in DIS (`[trunks.fax] resolutions`).
- PNG page preview and thumbnails (library only; the UI comes in M4).
- `result.json` now includes ECM details (`ecm`, PPR count, lost frames).

## [0.1.1-alpha] - 2026-10-21
### Fixed
- **Multi-page faxes from AudioCodes gateways stopped after page 1** (field issue F1). We answered while the gateway's V.21 signal was still on; a half-duplex gateway drops that answer. The engine now waits for the peer's signal to end before answering.
- A repeated MPS/EOP/EOM (or CRP) now restarts the T.30 T2 timer.
- A spurious bad line (and an extra blank row) at the top of every page, caused by idle bits before the first EOL.
- Release tarballs now store files as `root:root` (they showed up owned by an unrelated local user after extraction).

## [0.1.0-alpha] - 2026-10-21
### Added
- First field-test build: `sip-faxd serve` receives faxes over SIP + T.38 (non-ECM, MH/MR, up to 14400 bit/s) from VoIP gateways and writes multi-page G4 TIFF and PDF.
- Trunk profiles (`generic`, `cht-cs210`), source-IP allow-list, three ways of establishing T.38, per-call SIP/UDPTL capture, `config check`.
- Static musl binaries for x86_64 and aarch64; unpack-and-run on RHEL 9/10.

---

## 繁體中文

### [Unreleased]
#### 安全性
- `ctutils`（經由 `hmac`／`digest`，用於 Webhook 簽章）更新到 0.4.4（[RUSTSEC-2026-0385](https://rustsec.org/advisories/RUSTSEC-2026-0385)：常數時間的遮罩可能被編譯成分支）。0.4.1 以前的版本使用 0.4.2。

#### 新增
- **G.711 傳真**（里程碑 M5）：通話建立不了 T.38 時，改從音訊接收傳真，使用自行實作的 V.21、V.27ter（2400／4800）與 V.29（7200／9600）收端；不宣告 V.17，所以 G.711 傳真最高 9600 bit/s。適用情況：`t38_policy = "audio-only"`；gateway 拒絕我方的 T.38 re-INVITE（488／415／606、沒有可用的 `m=image`、2xx 沒有 SDP）；`wait-peer` 在 `initiate_after_secs` 內沒等到 T.38 re-INVITE。之後對方再發 T.38 re-INVITE 仍會切到 T.38。音訊路徑不使用 ECM。以前以 `g711_passthrough_unsupported` 失敗的來電，現在可以收下傳真。
- `result.json`：`audio`（codec、RTP 埠、改走音訊的原因）、`fax.media`（`t38`／`g711`）與 `fax.rtp` 計數；`logs/calls/<call-id>/audio.log`（V.21 frame、數據機切換、訊號事件）。
- 資料庫（migration 0003，啟動時自動套用）：`faxes.modem`、`faxes.media`；傳真內容頁顯示「傳送方式」，Webhook 也會帶上。
- Web 介面：系統狀態頁的「今日概況」（今天收到、未讀、投遞失敗與已放棄）與存檔目錄所在磁碟的剩餘空間；`GET /api/v1/faxes/counts?since=`、`GET /api/v1/status` 的 `storage`。
- Web 介面：傳真內容頁的「上一份／下一份」，依列表順序。
- Web 介面：備註或設定還沒儲存就要離開這一頁時會先詢問。
- Web 介面：登入頁可以顯示輸入的密碼。

#### 變更
- 以音訊接通的來電，RTP 使用該通 UDPTL 埠的下一個奇數埠：防火牆要開放整個 `port_range`（偶數與奇數埠）。`[media] audio_port` 只在 `fax_session = "hold"` 時使用。
- Web 介面：拿掉即時連線的圓點，只在連線中斷超過幾秒時顯示提示。
- Web 介面：傳真列表空的時候說明是還沒有傳真、還是篩選不到（附「清除篩選」）；備註欄占用列表剩下的寬度；表單欄位寬度一致。

#### 修正
- Web 介面：帳號選單剛打開時立刻點選單外面，選單不會關閉。

### [0.4.1-alpha] - 2026-10-23
Web 介面改善。

#### 新增
- Web 介面：選單的「傳真」旁顯示未讀份數，即時更新（`GET /api/v1/faxes/counts`）。

#### 變更
- Web 介面：語言、外觀與登出移到點自己名稱打開的帳號選單。
- Web 介面：點傳真列表的一列任何地方就會開啟；列表的時間改為簡短顯示（「今天 14:05」、「昨天」、更早顯示日期），滑鼠移上去看完整時間。
- Web 介面：傳真內容頁把詳細資料、備註與投遞放在預覽旁邊；技術欄位收在「更多資訊」。
- Web 介面：需要重啟的設定改為欄位名稱旁的 ⟲ 標記，區段上方說明一次；儲存列改為表單的一部分；新增帳號的表單按按鈕才打開。

#### 修正
- Web 介面：帳號列表的按鈕與所在的列沒有對齊。

### [0.4.0-alpha] - 2026-10-23
里程碑 M4e／M4f：換資料庫、資料庫設定簡化、設定檔只留啟動設定。

#### 新增
- **`[database]` 可以分欄位填寫**：`type`、`host`、`port`、`name`、`user`、`password`（原樣填寫，不用百分比編碼）或 `password_file`、`trust_server_certificate`；SQLite 為 `type = "sqlite"`、`path`。`url` 照樣可用。
- **`sip-faxd db check`**：測試資料庫連線（伺服器版本、資料表版本、耗時），不做任何修改；連不上時說明原因（主機、密碼、資料庫不存在、憑證）。`--url URL --password-stdin` 可以測試另一個資料庫。
- `db copy` 的目標密碼可以從標準輸入讀（`--to-password-stdin`），或從另一個設定檔取目標（`--to-config 檔案`）。
- 還沒有設定中繼時，Web 介面上方會顯示提示。
- Web 介面：語言旁新增外觀選單（跟隨系統、明亮、深色），存在瀏覽器。
- **`sip-faxd db copy --to <URL>`**：換資料庫，也可以換成另一種（例如從內建的 SQLite 改用 SQL Server）。全部資料表（傳真與已讀、備註、投遞、設定、稽核紀錄、帳號、登入 session）連同 `id` 在一個交易內複製到空的目標資料庫，完成後逐表核對。服務必須先停止；之後把 `[database] url` 改成目標，並沿用 `conf/secret.key`。

#### 變更
- **設定檔只放啟動設定**（`[database]`、`[web]`、`[storage]`、`[log]`、`[service]`）。SIP、中繼、投遞路由等執行設定在 Web 介面設定；第一次啟動以預設值建立，沒有中繼（新增中繼前所有來電都會被拒絕，log 與 Web 介面會提示）。`conf/runtime.toml.example` 說明這些設定，供 `config import --file` 使用。舊設定檔裡還有執行設定區段時照樣可用：匯入一次，之後 log 與 `config check` 會提示可以刪除。

#### 修正
- Web 介面：系統狀態頁的已執行時間每秒更新（原本在兩次讀取之間不會動）。

### [0.3.0-alpha] - 2026-10-22
里程碑 M4：Web 介面、帳號、設定存資料庫、服務管理、TUI。

#### 新增
- **設定存資料庫**（M4a）：`[sip]`、`[media]`、`[call]`、`[capture]`、`[[trunks]]`、`[[routes]]`、`[delivery]`、`[retention]` 在第一次啟動時匯入資料庫，之後在資料庫修改（Web 介面，或 `sip-faxd config import --force`）。修改不必重啟，下一通來電就套用；只有監聽位址、UDPTL port 範圍與 `[capture]` 需要重啟，會列為「等待重啟」。每次修改都由伺服器驗證，並記在稽核紀錄。註冊、Email、Webhook 的密碼以 `conf/secret.key` 加密存放；`sip-faxd config export` 以 TOML 輸出設定（加 `--include-secrets` 才含密碼）。
- **帳號**（M4a）：管理員與座席兩種角色；密碼以 Argon2id 雜湊，至少 10 個字元；連續輸錯 5 次鎖定 15 分鐘。第一次啟動建立 `admin`，隨機密碼寫在 `conf/initial-admin-password`，第一次登入必須修改。`sip-faxd user add|passwd|disable|enable|list`。
- **傳真指令**（M4a）：`sip-faxd fax list [--unread]`、`show`、`read`、`note`、`redeliver`（把失敗或已放棄的投遞重新排入）。
- **TUI**（M4d）：`sip-faxd tui` 在終端機顯示傳真（可切換已讀、編輯備註）、進行中的通話與服務狀態，服務執行中時即時更新。
- **服務管理**（M4d，Linux）：`sip-faxd start [-d]`、`stop [--force] [--timeout 秒數]`、`restart`、`status [--json]`（沒有在執行時回傳 3）、`logs [-f] [-n N]`。`start` 在背景啟動服務（獨立 session、以 `flock` 鎖定的 PID 檔、控制 socket 有回應才算就緒）並顯示 log；Ctrl+C 只停止顯示。偵測到由 systemd 管理時，`start`／`stop`／`restart` 提示改用 `systemctl`（`--ignore-systemd` 可略過）。systemd unit 請使用 `serve`。
- **優雅停止**：`stop` 與 `SIGTERM` 對新的 INVITE 回 503，等進行中的傳真收完（`[service] stop_timeout_secs`，預設 300 秒）才結束剩下的通話；已收到的頁面照樣保留（`partial`、`hangup_code = shutdown`）。
- 控制 socket `run/sip-faxd.sock`（權限 0600，一行一個 JSON：`status`、`calls`、`stop`、`reload`、`subscribe`）。
- Log 檔 `logs/sip-faxd.log`，依大小或每天輪替，保留 `[log] keep` 份並以 gzip 壓縮（設定檔新增 `[log]`、`[service]` 選項）。
- **Web 介面伺服器**（M4b）：HTTPS，監聽 `0.0.0.0:8443`；沒有提供憑證時使用自簽憑證（有效期 825 天，到期前 30 天自動更新，啟動時在 log 印出 SHA-256 指紋），也可以使用自己的憑證。`/api/v1` 提供登入、傳真（列表、詳細資料、已讀、備註、頁面預覽、下載 PDF／TIFF、重新投遞）、設定、帳號與狀態的 REST API，並以 WebSocket（`/api/v1/events`）即時推送更新。
- **PNG 頁面預覽隨傳真一起產生**：在 `<名稱>.tif`、`<名稱>.pdf` 旁邊，每頁寫出 `<名稱>-p1.png`（整頁）與 `<名稱>-p1-thumb.png`（寬 300 px 的縮圖），清整時和 TIFF、PDF 一起刪除。
- **Web 介面**（M4c，使用說明見 `docs/web.md`），內建在 `sip-faxd` 執行檔中，提供繁體中文與英文：傳真列表（未讀加粗、篩選、搜尋號碼／對方識別／備註、捲動自動載入，接收中的傳真即時逐頁顯示，不必重新整理）、傳真內容（縮圖、可縮放的整頁預覽、下載 PDF／TIFF、切換已讀／未讀、備註編輯並在別人同時修改時提示、投遞狀態、重新投遞）；管理員另有設定（SIP、媒體、通話、擷取、中繼、投遞路由、投遞參數、保存期限；需要重啟才生效的設定會標示）、帳號管理與設定稽核紀錄；所有人都可看系統狀態。可以只用鍵盤操作。
- `GET /api/v1/faxes` 新增 `q`：在發話／受話號碼、對方識別或備註中任何位置搜尋（不分大小寫）。`PUT /api/v1/auth/me` 設定自己帳號的介面語言。`GET /api/v1/config` 列出需要重啟才生效的設定（`restart_fields`）。

#### 變更
- **Web 介面預設開啟**，設定檔沒有 `[web]` 也會啟動：升級後的伺服器會開始監聽 TCP 8443。請在防火牆只開放給要使用介面的網段，或以 `[web] enabled = false` 關閉。
- `[storage] preview_cache_mb` 不再使用（已沒有獨立的預覽快取），`config check` 會提示移除。
- **從 0.2.0 升級**：0.3.0 第一次啟動時，設定檔的執行設定區段會複製到資料庫；之後再改設定檔中的這些區段不會生效（啟動 log 會提醒）。`conf/secret.key` 要和資料庫一起備份。

#### 安全性
- tokio 更新到 1.54.1（[RUSTSEC-2026-0384](https://rustsec.org/advisories/RUSTSEC-2026-0384)：多執行緒 runtime 上 `tokio::time::Interval` 的競態可能讓伺服器當掉；0.2.0 使用的就是受影響的 1.54.0）。

### [0.2.0-alpha] - 2026-10-21
**新增**
- **傳真紀錄存入資料庫**（M3）：預設 SQLite（內建，`data/sip-faxd.db`），也支援 PostgreSQL、MySQL/MariaDB、Microsoft SQL Server。每通電話一筆：ANI/DNIS（每個 trunk 可在 `[trunks.numbers]` 設定取自哪個 header）、掛線方向與原因、狀態（接收中／完整／部分／失敗／非傳真）、接收中的即時頁數、檔案大小、已讀／未讀與備註（供之後的介面使用）。
- 資料庫故障不會影響收傳真：寫入會排隊重試；`sip-faxd db backfill` 可從 `logs/calls/*/result.json` 補回缺漏的紀錄。
- 新指令：`sip-faxd db migrate`、`sip-faxd db backfill`、`sip-faxd purge [--dry-run]`。啟動時自動執行 migration（`[database] auto_migrate`），多個實例同時啟動也安全。
- **清整**：檔案與紀錄分別設定保留天數（`[retention]`，第一次啟動後以資料庫為準），每天在 `run_at` 執行。
- **投遞**（`[[routes]]`、`[delivery]`）：複製到資料夾、Email（SMTP，支援 STARTTLS／SMTPS，只用 rustls）、Webhook（JSON，可加 HMAC-SHA256 簽章與 PDF 附件）。失敗依退避時間重試（1、5、15、60 分鐘，之後每 4 小時），重啟後繼續；重啟前剛收完的傳真會補送。`config check` 會列出路由並遮蔽密碼。
- **ECM 接收**（T.30 錯誤更正）與 MMR（T.6）；**預設關閉**（`[trunks.fax] ecm = false`），等真實 gateway 驗證後再評估。
- 每個 trunk 可設定 DIS 宣告的解析度（`[trunks.fax] resolutions`）。
- PNG 頁面預覽與縮圖（函式庫，介面在 M4）。
- `result.json` 新增 ECM 資訊（`ecm`、PPR 次數、遺失的 frame 數）。

### [0.1.1-alpha] - 2026-10-21
**修正**
- **AudioCodes 送多頁傳真時只收到第 1 頁**（現場問題 F1）：
  - 原因：我們在 gateway 的 V.21 訊號還沒結束時就開始回應，半雙工的 gateway 會把這個回應丟掉。
  - 修正：改為等對方的訊號結束後才回應。
- 對方重送 MPS/EOP/EOM（或 CRP）時，會重新啟動 T.30 的 T2 計時器。
- 修正每一頁頂端多出一行壞行（以及一行空白）的問題，原因是第一個 EOL 之前的 idle 訊號被當成影像資料。
- 發佈的壓縮檔改以 `root:root` 儲存檔案擁有者。之前解壓後，擁有者會變成主機上不相干的帳號。

### [0.1.0-alpha] - 2026-10-21
**新增**
- 第一個現場試接版本。`sip-faxd serve` 可以透過 SIP + T.38 接收 VoIP gateway 送來的傳真（non-ECM、MH/MR、最高 14400 bit/s），並輸出多頁的 G4 TIFF 和 PDF。
- Trunk profile（`generic`、`cht-cs210`）、來源 IP 白名單、三種 T.38 建立方式、每通電話的 SIP/UDPTL 擷取、`config check`。
- x86_64 和 aarch64 的靜態 musl 執行檔，在 RHEL 9/10 解壓即可使用。
