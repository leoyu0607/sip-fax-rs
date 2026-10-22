# M3 設計規格：傳真紀錄、預覽、清整、投遞、ECM

- 撰寫：Opus（2026-10-21）
- 需求來源：`docs/GOALS.md` 的「傳真紀錄、預覽與清整」一節，含使用者已確認的決策
- 實作者依本規格施作。要偏離時，先在報告中提出擬議 ADR，由 Opus 合併。

## 0. 原則
- **DB 故障不得影響收傳真。** 寫 DB 失敗時，傳真檔和 `result.json` 照常寫出。DB 寫入放進記憶體佇列，以指數退避重試；重試仍失敗就記 error，並在下次啟動時由 `logs/calls/*/result.json` 回補（`sip-faxd db backfill`）。
- **四種 DB 共用同一份行為測試**：SQLite（預設，bundled）、PostgreSQL、MySQL/MariaDB、MSSQL。每個後端都要跑完全相同的 store 行為測試套件。
- **時間一律以 UTC 儲存**，介面再依時區顯示。各 DB 的欄位型別：

  | DB | 型別 |
  |---|---|
  | SQLite | `TEXT`（RFC 3339，精確到毫秒） |
  | PostgreSQL | `TIMESTAMPTZ` |
  | MySQL/MariaDB | `DATETIME(3)` |
  | MSSQL | `DATETIME2(3)` |

- **存取層**：
  - sqlx（rustls）負責 SQLite、PostgreSQL、MySQL。
  - tiberius（rustls）負責 MSSQL。
  - 依 ADR-003，統一在 `store::FaxStore` trait 之後。

## 1. 資料表

### 1.1 `faxes`（一通進線一筆）
| 欄位 | 型別（概念） | 說明（對應 GOALS 的需求欄位）|
|---|---|---|
| `id` | BIGINT 自增 PK | **ID** |
| `call_id` | VARCHAR(255) UNIQUE | SIP Call-ID，用來對照 `logs/calls/` |
| `trunk` | VARCHAR(64) | |
| `ani` | VARCHAR(64) NULL | **進線號碼 ANI**，見 §2 |
| `ani_source` | VARCHAR(16) NULL | 取自哪個 header：`pai`/`rpid`/`from` |
| `dnis` | VARCHAR(64) NULL | **撥打號碼 DNIS**，見 §2 |
| `dnis_source` | VARCHAR(16) NULL | `ruri`/`to`/`diversion` |
| `remote_ident` | VARCHAR(32) NULL | 對方傳真機的 TSI |
| `started_at` | 時間 | **進線時間**：收到 INVITE 的時間 |
| `answered_at` | 時間 NULL | 送出 200 的時間 |
| `ended_at` | 時間 NULL | **掛線時間**。接收中時為 NULL |
| `duration_sec` | INT NULL | **通話長度**：`ended_at − answered_at`，沒有接通時為 0 |
| `hangup_direction` | VARCHAR(8) NULL | **掛線方向**：`remote`、`local`、`system`，見 §3 |
| `hangup_code` | VARCHAR(32) NULL | **掛線原因**（固定代碼），見 §3 |
| `hangup_text` | VARCHAR(255) NULL | 掛線原因的中文說明 |
| `sip_status` | INT NULL | 拒接或失敗時的 SIP 狀態碼 |
| `t30_code` | INT NULL | T.30 結果碼 |
| `fax_status` | VARCHAR(12) | **傳真接收狀態**：`receiving`、`received`、`partial`、`failed`、`no_fax` |
| `pages` | INT | **傳真檔頁數**。接收中時代表**目前已收到的頁數** |
| `resolution` | VARCHAR(12) NULL | |
| `bit_rate` | INT NULL | |
| `compression` | VARCHAR(8) NULL | |
| `ecm` | BOOL | |
| `tiff_path` | VARCHAR(1024) NULL | **傳真檔路徑**（相對於 `data_dir`；寫入 DB 時改成相對路徑，避免換目錄後失效）|
| `tiff_size` | BIGINT NULL | **傳真檔大小**，單位 bytes |
| `pdf_path` | VARCHAR(1024) NULL | |
| `pdf_size` | BIGINT NULL | |
| `files_purged_at` | 時間 NULL | 檔案已被清整的時間（紀錄保留） |
| `is_read` | BOOL DEFAULT false | **已讀/未讀**，全體共用 |
| `read_changed_by` | VARCHAR(64) NULL | 最後一次切換的人 |
| `read_changed_at` | 時間 NULL | |
| `note` | TEXT NULL | **備註**，長度上限 4000 字 |
| `note_updated_by` | VARCHAR(64) NULL | |
| `note_updated_at` | 時間 NULL | |
| `created_at` / `updated_at` | 時間 | |

索引：
- `(started_at DESC)`
- `(is_read, started_at DESC)`
- `(fax_status)`
- `(ani)`
- `(dnis)`
- `(files_purged_at)`

### 1.2 其他資料表
- `settings(key VARCHAR(64) PK, value TEXT, updated_by, updated_at)`
  - 存放可以線上調整的設定。M3 先放清整天數，M4 的 Web 介面會修改這張表。
  - 首次啟動時，用設定檔的值初始化；之後**以 DB 為準**，設定檔的值只在 DB 沒有這個 key 時才採用。
- `delivery_attempts(id, fax_id FK, target VARCHAR(255), kind VARCHAR(16), attempt INT, status VARCHAR(12), last_error TEXT, next_try_at, created_at, updated_at)`
  - `status` 的值：`pending`、`done`、`failed`、`giving_up`。
- `schema_migrations(version INT PK, applied_at)`

### 1.3 Migration
- 各 DB 各有一套 SQL 檔，放在 `crates/store/migrations/<backend>/NNNN_name.sql`，編譯時嵌入程式。
- 有兩種執行方式：
  - `sip-faxd db migrate`：手動執行。
  - 啟動時自動檢查。設定 `[database] auto_migrate = true`，預設開啟。
- 只能往前升級。版本號比程式新的 DB，一律拒絕啟動。

## 2. ANI / DNIS 擷取規則
- 每個 trunk 可以設定：
  - `ani_sources = ["pai", "rpid", "from"]`（預設值）
  - `dnis_sources = ["ruri", "to"]`（預設值；可再加 `diversion`）
- 依序取第一個存在且 user 部分不是空的來源：
  - URI 是 `tel:` 時，取它的號碼。
  - `sip:`/`sips:` 時，取 user 部分，並移除 `;` 之後的參數。
- 正規化：
  - 移除視覺分隔符號（`-`、`.`、空白、括號）。
  - `strip_plus` 預設 false。
  - 可以設定 `ani_strip_prefix` / `dnis_strip_prefix`，例如移除國碼。
- 對方隱藏號碼（`anonymous` 或 `Privacy: id`）時，`ani` 存 NULL，`ani_source` 存 `anonymous`。
- 中華電信 CS210 在哪個 header 帶號碼還不知道，所以**預設值以 AudioCodes 的實測為準**：AudioCodes 的號碼在 From 和 Request-URI 中。CS210 試接取得資料後再調整 `cht-cs210` 預設組。

## 3. 掛線方向與原因
| `hangup_code` | 方向 | 條件 | `fax_status` |
|---|---|---|---|
| `completed` | 依實際 BYE 的發送方 | T.30 成功（code=0）| `received` |
| `peer_hangup` | remote | 對方在 T.30 完成前送了 BYE，且已收到 ≥1 頁 | `partial` |
| `peer_hangup_no_fax` | remote | 對方在沒有任何頁面時掛斷，含 T.38 建立前就掛斷 | `no_fax` |
| `t30_failed` | local | T.30 失敗（附 `t30_code`）。有頁面時狀態為 `partial`，沒有頁面時為 `failed` | `partial` / `failed` |
| `no_fax_signal` | local | 接通但一直沒有傳真訊號，例如一般電話打進來 | `no_fax` |
| `t38_rejected` | system | 對方拒絕 T.38，而 G.711 passthrough 還不支援 | `no_fax` |
| `caller_cancelled` | remote | 接聽前就收到 CANCEL | `no_fax` |
| `rejected` | system | 我方拒接，例如 4xx/5xx、無媒體埠（503）。附 `sip_status` | `no_fax` |
| `ack_timeout` / `session_expired` / `max_duration` | system | 依名稱 | 依頁數判斷 |
| `shutdown` | system | 服務停止 | 依頁數判斷 |
| `internal_error` | system | 其他 | 依頁數判斷 |

- 「依頁數判斷」的意思：pages ≥ 1 為 `partial`，pages = 0 為 `failed`。
- 從來源 IP 白名單擋掉的電話（drop 或 reject）**不寫入 DB**，只記 log，避免掃描流量灌爆資料表。
- 對照邏輯放在 `fax_in::summary::classify(&CallResult) -> Summary`，是純函式，以表格驅動測試逐列驗證。

## 4. 接收中狀態
- `CallEvent::Started` 時 INSERT 一筆紀錄，`fax_status = 'receiving'`、`pages = 0`。
- 新增 `CallEvent::PageReceived { call_id, pages, at }`：T.30 每接受一頁（回 MCF）就送出這個事件，store 隨即 UPDATE `pages`。
- `CallEvent::Finished` 時，由 classify 計算最終欄位並 UPDATE。
- 啟動時，所有還是 `receiving` 的紀錄都改成：
  - `fax_status = failed` 或 `partial`（依頁數）
  - `hangup_code = shutdown`
  - `hangup_text` 說明「服務中斷時仍在接收」

  這樣就不會有永遠卡在接收中的紀錄。

## 5. 已讀/未讀與備註（store API）
- `set_read(id, read: bool, by: &str)`：無條件設定狀態，並更新 `read_changed_by` 和 `read_changed_at`。
- `set_note(id, text, by, expected_updated_at: Option<…>)`：以 `note_updated_at` 做樂觀鎖。不一致時回傳 `Conflict`，讓 M4 的介面提示「備註已被別人修改」。
- `list(filter, page)`
  - 可依以下條件篩選：`is_read`、`fax_status`、日期區間、`ani`/`dnis` 的前綴搜尋、`trunk`。
  - 依 `started_at DESC` 排序，以 keyset 方式分頁，每頁上限 200 筆。
- M4 的 Web 介面開啟一份傳真時，**不自動標記已讀**；要由介面呼叫 `set_read`，這部分在 M4 決定。

## 6. PNG 預覽（第一次開啟時才產生）
- `t4::png`：純 Rust 的 PNG 編碼器，使用 `png` crate（純 Rust，MIT/Apache），需要寫 ADR。輸出兩種影像：
  - **整頁**：1-bit 灰階 PNG。standard 解析度（98 dpi）的頁面，垂直方向要複製每一行成兩倍，讓頁面比例正確；fine 解析度不用處理。
  - **縮圖**：寬 300 px 的 8-bit 灰階，用面積平均法縮小。
- （ADR-057 起）收完傳真時就產生，放在 TIFF／PDF 旁：`<name>-p<page>.png`、`<name>-p<page>-thumb.png`；舊傳真沒有 PNG 時，第一次開啟才轉出並寫在同一處。原本的快取位置 `data_dir/cache/png/<fax id>/` 不再使用：
  - 用原子性的寫入（`.part` 寫完後 rename）。
  - 同一頁同時有多個請求時，只轉檔一次。
- 呼叫介面是 `preview::page_png(store, id, page, kind) -> Result<PathBuf>`。以下情況回明確錯誤：頁碼超出範圍、檔案已經被清整、TIFF 損壞。
- 清整檔案時，同時刪除這些 PNG（以及舊版的 `cache/png/<id>/`）。

## 7. 清整機制
- 設定項目（存在 `settings` 表；首次啟動時從 `[retention]` 初始化）：
  - `retention.files_days`：預設 90。0 表示永遠不刪。
  - `retention.records_days`：預設 365，必須 ≥ `files_days`。0 表示永遠不刪。
  - `retention.run_at`：預設 `"03:17"`，伺服器本地時間。
- **檔案清整**：
  - 對象是 `ended_at < now − files_days` 且 `files_purged_at IS NULL` 的紀錄。
  - 刪除 TIFF、PDF 和 PNG 快取，然後設定 `files_purged_at`。
  - 刪檔失敗（權限問題等）時，不設定 `files_purged_at`，記 error，下次再重試。
  - 檔案本來就不存在時，視為成功。
- **紀錄清整**：
  - 對象是 `ended_at < now − records_days` 的紀錄。
  - 先刪除殘留的檔案，再刪除紀錄和它的 `delivery_attempts`。
- `receiving` 狀態的紀錄永遠不會被清整。
- 每批處理 500 筆，避免長時間鎖表。
- 手動指令：`sip-faxd purge [--dry-run]`，執行後列出各類筆數。
- 刪除檔案之前，要確認路徑位於 `data_dir` 之內（防止 path traversal），不在範圍內的就拒絕刪除並記 error。

## 8. 每個 trunk 的解析度設定
- `[trunks.fax] resolutions = ["standard", "fine"]`（預設值）。
- DIS 只宣告列出的解析度。設定為空，或包含不支援的值時，`config check` 回報錯誤。

## 9. 投遞
- **路由**：
  - 設定檔用 `[[routes]]` 指定比對條件：`dnis`（精確值）、`dnis_prefix`、`trunk`，以及 `default = true`。
  - 依序取第一條符合的路由；每條路由可以有多個 `deliveries`。
  - M4 再評估是否把路由改存在 DB，到時要寫 ADR。
- **投遞方式**：
  - **folder**：把 PDF 和 TIFF 複製到指定目錄，檔名可以用範本，例如 `{date}_{ani}_{id}.pdf`。寫入方式是原子性的。
  - **email**：lettre，搭配 rustls、STARTTLS 或 SMTPS，支援 AUTH。附件是 PDF，主旨和內文可以用範本。
  - **webhook**：HTTP POST JSON，內容是紀錄的欄位。可以選擇帶 HMAC-SHA256 簽章 header，也可以選擇以 multipart 附上 PDF。
- 只有在 `fax_status` 是 `received` 或 `partial` 時才投遞。是否投遞 partial，由路由的設定決定。
- **重試**：
  - 每一次投遞都寫進 `delivery_attempts`。
  - 失敗時以指數退避重試（1、5、15、60 分鐘，之後每 4 小時一次），上限 `max_attempts`，預設 10。超過上限就設為 `giving_up`。
  - 重新啟動後，從 DB 接著重試。
- 投遞失敗不影響傳真紀錄本身。

## 10. ECM（T.30 錯誤更正）與 MMR
- DIS 依設定宣告 ECM（`[trunks.fax] ecm = true`，預設開啟）。啟用 ECM 時，同時宣告 MMR（T.6）。
- 接收端要處理：
  - 64 或 256 bytes 的 frame
  - 每個 partial page 最多 256 個 frame
  - FCD 和 RCP
  - PPS-NULL、PPS-MPS、PPS-EOP、PPS-EOM
  - 用 PPR 回報遺失 frame 的 bitmap，等對方重送
  - 4 次 PPR 之後依 T.30 處理 CTC/CTR
  - EOR
  - RNR/RR，用於流量控制，我方通常不需要送
- 取得 ITU-T T.30 原文（ITU 網站可以免費下載），核對 DIS、DCS、ECM 相關 FCF 的 bit 位置。核對結果寫進報告，包含 M2c 留下的待查項目：V.33 和掃描時間碼。

## 11. 設定範例（新增部分）
```toml
[database]
url = "sqlite://data/sip-faxd.db"        # postgres://…  mysql://…  mssql://user:pass@host:1433/db
auto_migrate = true
max_connections = 5

[retention]                               # 首次啟動時寫入 settings 表，之後以 DB 為準
files_days = 90
records_days = 365
run_at = "03:17"

[[trunks]]
# …
[trunks.fax]
resolutions = ["standard", "fine"]
ecm = true
[trunks.numbers]
ani_sources = ["pai", "rpid", "from"]
dnis_sources = ["ruri", "to"]

[[routes]]
dnis_prefix = "2557"
[[routes.deliveries]]
kind = "folder"
dir = "/srv/fax-inbox"
filename = "{date}_{ani}_{id}.pdf"
```
