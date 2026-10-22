# 資料庫說明（M3a、M3b、M4a）

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

M4a 起清整設定（`retention.*`）不再放在這裡：migration 0002 把它們搬進 `config_sections` 的 `retention` 區段（缺少的 key 取預設值 90／365／`03:17`），並從 `settings` 刪除。`settings` 保留給其他零散設定，目前沒有使用中的 key。

## `config_sections`（M4a，migration 0002）

執行設定（ADR-051）。每個區段一列，`body` 是該區段的 JSON，由 Rust 型別驗證。

| 欄位 | 說明 |
|---|---|
| `name` | 主鍵：`sip`、`media`、`call`、`capture`、`delivery`、`retention`、`routes`（整份有序清單）、`trunk:<名稱>`（每個 trunk 一列）、`trunk_order`（trunk 的比對順序，名稱的 JSON 陣列；規格只列 `trunk:<name>`，順序是本實作補的） |
| `body` | JSON；秘密欄位是 `enc:v1:<base64(nonce‖密文‖tag)>`（AES-256-GCM，金鑰在 `conf/secret.key`，欄位路徑為附加驗證資料） |
| `version` | 樂觀鎖，從 1 起，每次修改加 1；儲存時帶上讀到的 version，不符回 Conflict |
| `updated_by`、`updated_at` | 最後修改者（使用者名稱、`cli:<作業系統帳號>`、`config-file`、`migration`）與時間 |

首次啟動時，資料庫沒有任何執行設定（`retention` 與 `trunk_order` 不算）就把設定檔的執行區段匯入；之後以資料庫為準。

## `config_audit`

每次修改一列：`id`、`section`、`action`（`create`、`update`、`delete`、`import`，以及帳號操作 `user_add`、`user_disable`、`user_enable`、`user_delete`、`user_update`、`password_reset`、`password_change`，此時 `section` 為 `user:<帳號>`）、`before_json`、`after_json`（秘密欄位一律是 `"***"`）、`changed_by`、`at`。修改與稽核列是兩個陳述式：中途當機可能少一列稽核，不會少修改。

## `users`

`id`、`username`（唯一，小寫 `a-z 0-9 . _ -`，最長 64）、`display_name`、`role`（`admin`／`agent`）、`password_hash`（argon2id PHC 字串，m=19456、t=2、p=1）、`locale`、`disabled`、`must_change_password`、`created_at`、`updated_at`、`last_login_at`。

## `sessions`

`id`、`token_hash`（token 的 SHA-256 十六進位，唯一；token 本身不存）、`user_id`（外鍵）、`created_at`、`last_seen_at`（閒置 8 小時從這裡算）、`expires_at`（絕對上限，登入後 7 天）、`ip`、`user_agent`（截斷）。重新啟動後仍有效。CSRF token 由 session token 推導（SHA-256），不存。

## `delivery_attempts`

投遞（資料夾、Email、Webhook）的嘗試紀錄（M3c 實作、M3f 接進 `sip-faxd serve`）。**每次嘗試一列**：第一次嘗試是 `attempt = 1`、`pending`、`next_try_at` = 現在；成功就改成 `done`；暫時性失敗時，該列改成 `failed`（保留 `last_error`），並新增 `attempt + 1` 的 `pending` 列（`next_try_at` 依 1、5、15、60 分鐘、之後每 4 小時退避）；用盡 `max_attempts` 的那一列改成 `giving_up`；永久性失敗（HTTP 4xx 除 408/425/429、SMTP 5xx、設定或來源檔案錯誤）只把該列改成 `failed`，不再排下一次。同一份傳真同一個 `target` 只會建立一次（冪等）。重新啟動後，`pending` 列繼續執行；啟動時 `[delivery] catch_up_hours` 內已結束、還沒有任何列的 `received`／`partial` 傳真會補建。投遞失敗不會改 `faxes` 的任何欄位。

| 欄位 | 說明 |
|---|---|
| `id` | 主鍵 |
| `fax_id` | 外鍵，對應 `faxes.id` |
| `target` | 穩定的目標識別字，不含密碼或 token：`folder:<目錄>`、`email:<收件者，以逗號相連>`、`webhook:<URL，去除帳密與 query>`；同一路由內不重複，重啟後靠它找回設定 |
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
- **並行保護（M3b）**：migration 開始前先取得資料庫鎖（PostgreSQL `pg_advisory_lock`、MySQL/MariaDB `GET_LOCK('sipfaxd_migrate:<schema>')`、MSSQL `sp_getapplock`、SQLite 的 `BEGIN EXCLUSIVE`，最多等 60 秒），取得鎖之後才重新讀版本號；所以兩個實例同時第一次啟動時，只有一個會套用，另一個等它做完後發現沒事可做。鎖屬於連線，程式中途掛掉也不會留下鎖。
- **失敗後可重跑（M3b）**：SQLite、PostgreSQL、MSSQL 的 DDL 有交易，migration 失敗會整份復原。MySQL/MariaDB 的 DDL 無法復原，所以它的 migration 檔每個陳述式都必須可重複執行（`CREATE TABLE IF NOT EXISTS`，索引寫在資料表定義裡；`mysql_migrations_are_safe_to_run_again` 測試會檢查），失敗後直接再跑一次即可。
- 手動指令：`sip-faxd db migrate`（輸出 `schema version 0 -> 1 (applied 0001)` 或 `already at schema version 1`）。

## 寫入流程與失敗處理（M3b）

| 時間點 | 動作 | 內容 |
|---|---|---|
| 來電對應到 trunk（`CallEvent::Started`） | INSERT | `fax_status = receiving`、`pages = 0`、`trunk`、`ani`／`dnis` 及來源、`started_at` |
| T.30 每確認一頁（回 MCF／RTP，ECM 與 non-ECM 相同；`CallEvent::PageReceived`） | UPDATE `pages` | 只往前、只作用於 `receiving` 的紀錄 |
| 通話結束（`CallEvent::Finished`） | 以 `fax_in::summary::classify` 的結果補齊 | `fax_status`、`hangup_*`、`sip_status`、`t30_code`、`duration_sec`、`ended_at`、`remote_ident`、`resolution`、`bit_rate`、`compression`、`ecm`、檔案路徑與大小；INSERT 若遺失，這一步會一併補上 |
| 伺服器啟動 | 修正殘留的 `receiving` | 依頁數改成 `failed` 或 `partial`，`hangup_code = shutdown` |

- 來源 IP 不在任何 trunk 白名單的電話（`unknown_source` 為 `drop` 或 `reject`）**不寫入**；`unknown_source = "log"` 的電話照常處理，`trunk` 為 `unknown`。
- 檔案路徑寫入前改成相對於 `data_dir`；對不上的絕對路徑（例如別台主機的 `result.json`）會從 `faxes/` 目錄起算，找不到就維持原樣（清整時會被路徑檢查拒絕）。
- 欄位超過長度上限時，寫入前先截斷（`call_id` 255、`trunk` 64、`ani`／`dnis` 64、`remote_ident` 32、`hangup_text` 255、路徑 1024），不會因為一個過長的 header 讓整筆紀錄寫不進去。
- **資料庫故障不影響收傳真**：寫入放進記憶體佇列，由一個 worker 依序處理。暫時性錯誤（連線被拒、逾時、連線中斷）以指數退避重試：1、2、4、8…秒，上限 60 秒，每個動作最多 8 次。8 次都失敗時記 error、進入「降級」狀態：之後的動作各只試一次，佇列不會堆積，成功一次就恢復。非暫時性錯誤（例如欄位限制）不重試。佇列上限 10 000，超過時丟棄 INSERT 與頁數更新（最終寫入不丟）。
- 最終寫入失敗時，若 capture 沒有留下 `result.json`（`[capture] keep = "failed"` 或 capture 關閉），會把它寫到 `[capture] dir/<call-id>/result.json`。
- **`sip-faxd db backfill`**：掃描 `logs/calls/*/result.json`，資料庫沒有的就 `upsert`；只有 `receiving` 的就補完成；已經完成的不動。可以重複執行。0.1.1 以前的 `result.json` 沒有號碼，ANI／DNIS 改取 `from`／`to` 的 user（來源記為 `from`／`to`）。
- 伺服器啟動時資料庫連不上：照樣啟動、照樣收傳真，log 記 `database not reachable`；版本比程式新、`auto_migrate = false` 而版本過舊、URL 或密碼錯誤則拒絕啟動。

### 掛線原因（`hangup_code`）

| `hangup_code` | 方向 | 條件 | `fax_status` |
|---|---|---|---|
| `completed` | 依實際送 BYE 的一方（`result.json` 的 `bye_by`） | T.30 成功 | `received` |
| `peer_hangup` | remote | 對方在 T.30 完成前掛斷，已收到 ≥1 頁 | `partial` |
| `peer_hangup_no_fax` | remote | 沒有任何頁面就掛斷，含 T.38 建立前 | `no_fax` |
| `t30_failed` | local | T.30 失敗（`t30_code`）；含 ECM 對方放棄部分 frame | 有頁面 `partial`，否則 `failed` |
| `no_fax_signal` | local | 接通且 T.38 已建立，但我方的 DIS 一直沒有回應（T.30 結果碼 1 或 2，且沒有頁面），例如一般電話 | `no_fax` |
| `t38_rejected` | system | 對方拒絕 T.38（488／415／606）或其他失敗回應（附 `sip_status`），G.711 passthrough 尚未支援 | `no_fax` |
| `caller_cancelled` | remote | 接聽前收到 CANCEL | `no_fax` |
| `rejected` | system | 我方拒接（488、420、503…，附 `sip_status`） | `no_fax` |
| `ack_timeout`、`session_expired`、`max_duration`、`shutdown`、`internal_error` | system | 依名稱 | 有頁面 `partial`，否則 `failed` |

### ANI／DNIS

`[trunks.numbers]`：`ani_sources`（預設 `pai`、`rpid`、`from`）、`dnis_sources`（預設 `ruri`、`to`，可加 `diversion`）依序取第一個有號碼的來源；`tel:` 取號碼、`sip:` 取 user 並去掉 `;參數`；`-`、`.`、空白、括號一律移除；`strip_plus` 去掉前導 `+`；`ani_strip_prefix`／`dnis_strip_prefix` 去掉國碼之類的前綴（不帶 `+` 的前綴也能比對帶 `+` 的號碼；整個號碼都被吃掉時不去除）。`diversion` 取最後一筆（RFC 5806：最舊的一筆，也就是原本撥的號碼）。對方隱藏號碼（From 為 `anonymous`、`Privacy: id`、Remote-Party-ID 的 `privacy=full`）時 `ani` 為 NULL、`ani_source = anonymous`。AudioCodes 的號碼在 From 和 Request-URI（預設值即可）；中華電信 CS210 尚未確認，拿到第一份擷取後再調整。

## 清整

- **檔案清整**：`ended_at` 早於「現在 − `files_days`」、還沒清過、有檔案路徑的紀錄。刪除 TIFF、PDF 與旁邊的 PNG 預覽（`<name>-p<n>.png`、`<name>-p<n>-thumb.png`，ADR-057；舊版的 `data_dir/cache/png/<id>/` 也一併刪除），然後設定 `files_purged_at`；紀錄保留。檔案本來就不存在視為成功；刪除失敗則不設定 `files_purged_at`，下次再試。
- **紀錄清整**：`ended_at` 早於「現在 − `records_days`」的紀錄。先刪除殘留的檔案，再刪除紀錄與它的 `delivery_attempts`；殘留檔案刪不掉的紀錄會保留。
- `receiving` 狀態的紀錄永遠不清整。每批 500 筆。
- **排程（M3b）**：`sip-faxd serve` 每天在 `retention.run_at`（伺服器本地時間）執行；每次等待前都重新從資料庫讀取 `run_at`，所以 Web 介面（M4）改了時間，隔天起生效。手動：`sip-faxd purge [--dry-run]`，輸出 `purge: files of N record(s) removed (K file(s)), M record(s) deleted (a + b batch(es)), S skipped`；`--dry-run` 輸出 `purge (dry run): files of N record(s) are due, M record(s) are due for deletion; nothing was deleted`。有錯誤時結束碼為 1。
- **設定來源**：`[retention]` 屬於 `config_sections` 的 `retention` 區段（見上）；第一次啟動時匯入，之後以資料庫為準，改設定檔不會生效。
- 刪除檔案前會確認路徑位於 `data_dir` 之內（含 `..` 與 symbolic link 檢查），不在範圍內的拒絕刪除並記 error。
- **預覽與清整的競態（M4a）**：清整標記 `files_purged_at` 之後會再掃一次該傳真的 PNG；預覽轉檔結束後會再確認一次紀錄，已清整就刪掉剛寫的 PNG 並回 `Purged`（410），所以清整與轉檔不論誰先都不會留下孤兒 PNG。
- ~~**預覽快取上限（M4a）**~~：ADR-057 取消。PNG 和 TIFF／PDF 一樣依 `files_days` 保存；`[storage] preview_cache_mb` 不再使用。
