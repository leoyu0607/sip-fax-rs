# Changelog

English | [繁體中文](#繁體中文)

All notable changes to this project are documented here. Versions follow [Semantic Versioning](https://semver.org/); `-alpha` releases are field-test builds.

## [Unreleased]

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
