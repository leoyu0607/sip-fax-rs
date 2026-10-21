# 資料庫說明（M3a）

傳真紀錄、設定與投遞紀錄都存在資料庫。支援 SQLite（預設，內建於執行檔）、PostgreSQL、MySQL/MariaDB、MSSQL；四種資料庫的資料表結構相同，行為由同一份測試套件驗證（見 `docs/test-reports/M3a.md`）。

- 程式碼：`crates/store`（`FaxStore` trait，`store::connect(url)` 依 URL 開頭選擇後端）
- 規格：`docs/design/m3-store.md`
- Migration 檔：`crates/store/migrations/<backend>/NNNN_name.sql`，編譯時嵌入執行檔，只能往前升級

## 連線 URL

| 資料庫 | URL 範例 |
|---|---|
| SQLite（預設） | `sqlite://data/sip-faxd.db`（目錄不存在會自動建立）、`sqlite::memory:` |
| PostgreSQL | `postgres://user:pass@host:5432/dbname` |
| MySQL | `mysql://user:pass@host:3306/dbname` |
| MariaDB | `mariadb://user:pass@host:3306/dbname`（等同 `mysql://`） |
| MSSQL | `mssql://user:pass@host:1433/dbname?encrypt=required&trust_server_certificate=false` |

- 帳號、密碼含特殊字元時請做 percent-encoding（例如 `@` 寫成 `%40`）。
- MSSQL 的 `encrypt` 可為 `off`、`on`、`required`（預設）、`not_supported`；自簽憑證可加 `trust_server_certificate=true`。
- 所有後端都用 rustls，不使用 OpenSSL。PostgreSQL / MySQL 的 TLS 選項沿用 sqlx（例如 `?sslmode=require`）。
- 資料庫需先建好（空的即可），程式只建立資料表，不建立資料庫。
- 建議的資料庫設定：MySQL/MariaDB 使用 `utf8mb4`；MSSQL 使用任何預設定序皆可（欄位為 `NVARCHAR`）。

## 時間

所有時間以 **UTC、毫秒精度**儲存，讀回時完全相等；顯示時區由介面決定。

| 資料庫 | 型別 |
|---|---|
| SQLite | `TEXT`，RFC 3339，例如 `2026-10-21T08:30:00.123Z`（固定寬度，可直接字串排序） |
| PostgreSQL | `TIMESTAMPTZ` |
| MySQL/MariaDB | `DATETIME(3)`（程式一律寫入 UTC 時間，不受伺服器或連線時區影響） |
| MSSQL | `DATETIME2(3)` |

## 使用者需求欄位對照

下表是需求原文的欄位對應到 `faxes` 資料表的哪個欄位。

| 需求欄位 | `faxes` 欄位 | 說明 |
|---|---|---|
| ID | `id` | 自增主鍵 |
| （對照用） | `call_id` | SIP Call-ID，唯一；用來對照 `logs/calls/` 的擷取記錄 |
| 進線號碼 ANI | `ani`、`ani_source` | `ani_source` 記錄取自哪個 header：`pai`、`rpid`、`from`，對方隱藏號碼時為 `anonymous`（此時 `ani` 為 NULL） |
| 撥打號碼 DNIS | `dnis`、`dnis_source` | `dnis_source`：`ruri`、`to`、`diversion` |
| 進線時間 | `started_at` | 收到 INVITE 的時間 |
| （接通時間） | `answered_at` | 送出 200 OK 的時間；沒有接通為 NULL |
| 掛線時間 | `ended_at` | 接收中時為 NULL |
| 掛線方向 | `hangup_direction` | `remote`（對方掛斷）、`local`（我方掛斷）、`system`（逾時、錯誤、拒接） |
| 掛線原因 | `hangup_code`、`hangup_text`、`sip_status`、`t30_code` | `hangup_code` 為固定代碼（`completed`、`peer_hangup`、`t30_failed`…，見規格第 3 節）；`hangup_text` 為中文說明；拒接時 `sip_status` 為 SIP 狀態碼；T.30 失敗時 `t30_code` 為結果碼 |
| 通話長度 | `duration_sec` | 單位秒，`ended_at − answered_at`（四捨五入到秒）；沒有接通為 0 |
| 傳真接收狀態 | `fax_status` | `receiving`（接收中）、`received`（完整收到）、`partial`（部分頁面）、`failed`（失敗）、`no_fax`（沒有傳真訊號） |
| 傳真檔路徑 | `tiff_path`、`pdf_path` | 相對於 `data_dir` 的路徑（換目錄後仍有效） |
| 傳真檔大小 | `tiff_size`、`pdf_size` | bytes |
| 傳真檔頁數 | `pages` | 接收中時為目前已收到的頁數 |
| 備註（座席輸入） | `note`、`note_updated_by`、`note_updated_at` | 上限 4000 字；以 `note_updated_at` 做樂觀鎖，兩人同時改時後者會收到衝突提示 |
| 已讀／未讀 | `is_read`、`read_changed_by`、`read_changed_at` | 全體共用，一份傳真只有一個狀態；可隨時切回未讀 |
| 清整（檔案已清除） | `files_purged_at` | 檔案清整後設定，紀錄保留 |

## `faxes`

| 欄位 | 型別（概念） | NULL | 說明 |
|---|---|---|---|
| `id` | BIGINT 自增 | 否 | 主鍵 |
| `call_id` | VARCHAR(255) | 否 | 唯一；區分大小寫（MySQL 用 `utf8mb4_bin`，MSSQL 用 `BIN2`） |
| `trunk` | VARCHAR(64) | 否 | trunk 名稱 |
| `ani`、`ani_source` | VARCHAR(64)、VARCHAR(16) | 是 | |
| `dnis`、`dnis_source` | VARCHAR(64)、VARCHAR(16) | 是 | |
| `remote_ident` | VARCHAR(32) | 是 | 對方傳真機的 TSI |
| `started_at` | 時間 | 否 | |
| `answered_at`、`ended_at` | 時間 | 是 | |
| `duration_sec` | INT | 是 | |
| `hangup_direction` | VARCHAR(8) | 是 | |
| `hangup_code` | VARCHAR(32) | 是 | |
| `hangup_text` | VARCHAR(255) | 是 | |
| `sip_status`、`t30_code` | INT | 是 | |
| `fax_status` | VARCHAR(12) | 否 | |
| `pages` | INT | 否 | 預設 0 |
| `resolution` | VARCHAR(12) | 是 | |
| `bit_rate` | INT | 是 | |
| `compression` | VARCHAR(8) | 是 | MH / MR / MMR |
| `ecm` | BOOL | 否 | 預設 false |
| `tiff_path`、`pdf_path` | VARCHAR(1024) | 是 | |
| `tiff_size`、`pdf_size` | BIGINT | 是 | |
| `files_purged_at` | 時間 | 是 | |
| `is_read` | BOOL | 否 | 預設 false |
| `read_changed_by` | VARCHAR(64) | 是 | |
| `read_changed_at` | 時間 | 是 | |
| `note` | TEXT（MSSQL：`NVARCHAR(MAX)`） | 是 | |
| `note_updated_by` | VARCHAR(64) | 是 | |
| `note_updated_at` | 時間 | 是 | |
| `created_at`、`updated_at` | 時間 | 否 | |

索引：`(started_at DESC, id DESC)`、`(is_read, started_at DESC)`、`(fax_status)`、`(ani)`、`(dnis)`、`(files_purged_at)`，另加 `(ended_at)` 供清整查詢使用。

過長的欄位值在寫入前就會被拒絕（回 `TooLong` 錯誤），四種資料庫的行為一致，不會有的截斷、有的報錯。

## `settings`

可線上調整的設定。**首次啟動時以設定檔的值初始化，之後以資料庫為準**：設定檔的值只在資料庫沒有該 key 時才採用，所以修改設定檔後重啟不會蓋掉線上調整的值。

| 欄位 | 說明 |
|---|---|
| `name` | 主鍵，例如 `retention.files_days` |
| `value` | 文字值 |
| `updated_by`、`updated_at` | 最後修改者與時間（由設定檔初始化時為 `config`） |

與規格（`key`、`value`）的差異：`key` 是 MySQL 的保留字，所以欄位名稱改為 `name`。

目前使用的 key：

| key | 預設 | 說明 |
|---|---|---|
| `retention.files_days` | 90 | TIFF、PDF、PNG 預覽保留天數；0 = 永遠不刪 |
| `retention.records_days` | 365 | 資料表紀錄保留天數；0 = 永遠不刪；必須 ≥ `files_days`（任一為 0 除外） |
| `retention.run_at` | `03:17` | 每天執行清整的時間（伺服器本地時間，格式 `HH:MM`） |

## `delivery_attempts`

投遞（資料夾、Email、Webhook）的嘗試紀錄，邏輯在 M3c 實作，資料表已先建好。

| 欄位 | 說明 |
|---|---|
| `id` | 主鍵 |
| `fax_id` | 外鍵，對應 `faxes.id` |
| `target` | 目標（目錄、位址或 URL） |
| `kind` | `folder`、`email`、`webhook` |
| `attempt` | 第幾次嘗試 |
| `status` | `pending`、`done`、`failed`、`giving_up` |
| `last_error` | 最近一次的錯誤訊息 |
| `next_try_at` | 下次重試時間 |
| `created_at`、`updated_at` | |

## `schema_migrations`

| 欄位 | 說明 |
|---|---|
| `version` | 主鍵，migration 版本號 |
| `name` | migration 名稱 |
| `applied_at` | 套用時間 |

- 啟動時（`auto_migrate = true`，預設）或手動執行時套用尚未套用的 migration。
- 資料庫的版本**比程式新**時一律拒絕連線（`database schema version N is newer than supported version M`），避免舊版程式破壞新版資料。
- `auto_migrate = false` 且資料庫版本較舊時拒絕連線，錯誤訊息會提示執行 `sip-faxd db migrate`。
- 不要同時讓兩個程式實例對同一個資料庫做第一次 migration；請先手動 migrate 一次。

## 清整

- **檔案清整**：`ended_at` 早於「現在 − `files_days`」、還沒清過、有檔案路徑的紀錄。刪除 TIFF、PDF 與 PNG 預覽快取（`data_dir/cache/png/<id>/`），然後設定 `files_purged_at`；紀錄保留。檔案本來就不存在視為成功；刪除失敗則不設定 `files_purged_at`，下次再試。
- **紀錄清整**：`ended_at` 早於「現在 − `records_days`」的紀錄。先刪除殘留的檔案，再刪除紀錄與它的 `delivery_attempts`；殘留檔案刪不掉的紀錄會保留。
- `receiving` 狀態的紀錄永遠不清整。每批 500 筆。
- 刪除檔案前會確認路徑位於 `data_dir` 之內（含 `..` 與 symbolic link 檢查），不在範圍內的拒絕刪除並記 error。
