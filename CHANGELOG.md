# Changelog

English | [繁體中文](#繁體中文)

All notable changes to this project are documented here. Versions follow [Semantic Versioning](https://semver.org/); `-alpha` releases are field-test builds.

## [Unreleased]
- Delivery is wired into `sip-faxd serve`: `[[routes]]` / `[delivery]` in the configuration, folder / e-mail / webhook deliveries with persistent retries, start-up catch-up, `config check` lists the routes with secrets hidden.
- M3 in progress: fax records in a database (SQLite / PostgreSQL / MySQL·MariaDB / MSSQL), live "receiving" status, PNG preview, retention/cleanup, delivery (folder / email / webhook), ECM and MMR.

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
- M3 開發中：
  - 傳真紀錄寫入資料庫（SQLite / PostgreSQL / MySQL·MariaDB / MSSQL）
  - 接收中狀態
  - PNG 預覽
  - 清整機制
  - 投遞（資料夾 / Email / Webhook）：已接進 `sip-faxd serve`（設定檔 `[[routes]]`、`[delivery]`，失敗持久化重試，啟動時補排，`config check` 列出路由並遮蔽密碼）
  - ECM 與 MMR

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
