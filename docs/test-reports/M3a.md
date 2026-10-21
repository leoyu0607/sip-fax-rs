# M3a 測試報告：store crate（四種 DB 的紀錄、設定、清整）

- 實作與執行：Sonnet（M3a）
- 計畫：`docs/test-plans/M3.md` 第 1 節 S01–S11、第 4 節 R01–R08（store 層級）；規格：`docs/design/m3-store.md` §0、§1、§5、§7
- 狀態：**待 Opus 審核**。沒有 commit、沒有 push。`scripts/ci-local.sh` 完整通過（`ci-local: all checks passed`，含 `cargo deny check`、涵蓋率門檻、e2e-docker）；四種伺服器 DB 的完整套件另以 `scripts/db-matrix.sh` 實跑全部通過（見 §3）。
- 沒有碰 `crates/fax-in`、`crates/t30`、`tools/`、`docs/TODO.md`、`docs/DECISIONS.md`。

## 1. 交付內容
| 項目 | 位置 |
|---|---|
| `FaxStore` async trait（insert_receiving、update_pages、finalize、upsert_finished（backfill 用）、get、get_by_call_id、list（篩選＋keyset，上限 200）、set_read、set_note（樂觀鎖）、fix_stale_receiving、settings 四個方法、清整查詢、delivery_attempts 基本存取） | `crates/store/src/store.rs` |
| 模型：`NewFax`、`FaxFinal`、`FaxRecord`、`ListFilter`、`Cursor`／`PageRequest`／`Page`、`FaxStatus`、`HangupDirection`、`DeliveryStatus`、`RetentionPolicy`…（不依賴 fax-in，M3b 自行對映 `CallResult`） | `model.rs` |
| 工廠：`store::connect(url)`／`connect_with(url, ConnectOptions)`，依 `sqlite:`／`postgres(ql):`／`mysql:`／`mariadb:`／`mssql:`／`sqlserver:` 選後端 | `lib.rs` |
| **架構**：所有 SQL 邏輯只寫一份（`sqlstore.rs`），四個後端只實作 6 個方法的小 trait `Backend`（execute／query／insert_id／close…）。這是讓「同一份測試套件跑四種 DB」成立的關鍵（提議 ADR-A） | `db.rs`、`sqlstore.rs`、`backend/{sqlite,postgres,mysql,mssql}.rs` |
| MSSQL：tiberius（rustls）＋ bb8 連線池（取用時 `SELECT 1` 驗證，斷線後自動重連） | `backend/mssql.rs` |
| 各後端獨立的 migration（forward-only，編譯時嵌入），版本比程式新時拒絕（`SchemaTooNew`），`auto_migrate=false` 且過舊時 `SchemaOutdated` | `migrations/<backend>/0001_init.sql`、`migrate.rs` |
| 清整：`Purger`（檔案清整、紀錄清整、批次 500、dry-run、注入時鐘、`FileOps` 抽象可模擬刪檔失敗、`data_dir` 路徑安全檢查含 `..`／絕對路徑／symlink）、`seed_retention`／`load_retention`／`save_retention`（設定播種語意） | `purge.rs` |
| 注入時鐘 `Clock`／`SystemClock`／`ManualClock` | `clock.rs` |
| 測試 | `crates/store/src/tests/{mod,store_tests,purge_tests,unit_tests}.rs`、`backend/mssql.rs`、`error.rs` 內的單元測試 |
| 本機 DB 矩陣 | `scripts/db-matrix.sh`（Docker；`ci-local.sh` 以 `RUN_DB_MATRIX=1` 可選擇一併執行） |
| CI | `.github/workflows/ci.yml`：新增 `db-matrix` job、coverage job 加 `store ≥ 80`、build-musl 加「store 靜態連結」步驟、`release` 的 `needs` 加 `db-matrix` |
| 文件 | `docs/database.md`（資料表／欄位對照，繁中） |

## 2. 指令
```sh
cargo test -p store                                   # SQLite（檔案 + :memory:），每個案例各跑兩次
scripts/db-matrix.sh [postgres mysql mariadb mssql]   # Docker 啟動伺服器，SIP_FAXD_TEST_DB=<url> 跑同一份套件
SIP_FAXD_TEST_DB=postgres://… SIP_FAXD_TEST_DB_CONTAINER=<容器> cargo test -p store   # 手動；有 CONTAINER 時 S11 的斷線測試才會執行
TZ=Asia/Taipei cargo test -p store                    # 客戶端時區變化（亦試過 UTC、America/New_York）
cargo llvm-cov -p store --fail-under-lines 80
cargo deny check
cargo zigbuild --release --target x86_64-unknown-linux-musl -p store --tests   # 見 §6.3
scripts/ci-local.sh
```
映像：`postgres:16`、`mysql:8.0`、`mariadb:11`、`mcr.microsoft.com/mssql/server:2022-latest`（`--platform linux/amd64`，在 Apple Silicon 上以模擬執行，仍通過）。容器皆以 `TZ=Asia/Taipei` 啟動，使伺服器端時區與 UTC 不同。

## 3. 案例 × 後端結果
每個「DB 案例」是一個測試函式，對每個目標各建一個全新資料庫（伺服器上 `CREATE DATABASE sipfax_<pid>_<n>`，結束後 DROP）後執行同一份程式碼。**全部通過**：

| 案例 | SQLite 檔案 | SQLite :memory: | PostgreSQL 16 | MySQL 8.0 | MariaDB 11 | MSSQL 2022 | 測試函式／證據 |
|---|---|---|---|---|---|---|---|
| S01 migration（空 DB、重複執行 0 個套用、7 個索引都存在、版本 99 → `SchemaTooNew`、移除後恢復） | 通過 | 通過 | 通過 | 通過 | 通過 | 通過 | `s01_migration`（索引數以各 DB 的 catalog 查詢驗證；connect() 對較新 DB 的拒絕只在非 :memory: 驗證，因為 :memory: 每條連線是新 DB，改驗 `migrate()` 拒絕） |
| S01b 空 DB 且 `auto_migrate=false` → `SchemaOutdated{db:0}`；migrate 後 0→1 | 通過 | 略過（:memory: 無法重連同一個 DB） | 通過 | 通過 | 通過 | 通過 | `s01b_*` |
| S02 接收中流程（insert → pages 1,2,3 → 退回無效 → finalize；欄位逐一斷言、duration 62 s、覆寫、重複 Call-ID、NotFound、`Receiving` 不得 finalize、過長值） | 通過 | 通過 | 通過 | 通過 | 通過 | 通過 | `s02_receiving_flow` |
| S03 時間精度（1970、.123、.999、2100、UTC+8 邊界、微秒輸入截斷成毫秒、區間篩選） | 通過 | 通過 | 通過 | 通過 | 通過 | 通過 | `s03_time_precision`；客戶端 `TZ=UTC`／`Asia/Taipei`／`America/New_York` 各跑一次 SQLite 全套件皆通過；伺服器容器本身是 `Asia/Taipei`（PostgreSQL `TIMESTAMPTZ`、MySQL `DATETIME(3)` 的結果與 UTC 一致） |
| S04 長字串／特殊字元（中文、emoji 與 BMP 外字元、`'";--`、`DROP TABLE` 注入字串放在 call_id／trunk／ani／dnis／備註、4000 個 ASCII／中文／emoji 字元、4001 → `TooLong`、LIKE 萬用字元 `% _ [` 當字面值） | 通過 | 通過 | 通過 | 通過 | 通過 | 通過 | `s04_text` |
| S05 `set_read`（切換、切回、他人覆蓋、重複設定仍記錄切換者／時間、NotFound、finalize 不洗掉已讀與備註、依 is_read 篩選） | 通過 | 通過 | 通過 | 通過 | 通過 | 通過 | `s05_read_flag` |
| S06 `set_note` 樂觀鎖（None 對已編輯、過期 token、他人修改（錯誤訊息含修改者）、凍結時鐘下版本仍遞增、微秒 token 容忍、NotFound） | 通過 | 通過 | 通過 | 通過 | 通過 | 通過 | `s06_note_lock` |
| S07 `list`（25 筆含 5 組同毫秒、每頁 10 筆走完 3 頁、無重複無遺漏、順序 `started_at DESC, id DESC`、cursor 文字往返、組合篩選 ani/dnis 前綴＋日期＋狀態＋已讀＋trunk、其他 trunk 不洩漏） | 通過 | 通過 | 通過 | 通過 | 通過 | 通過 | `s07_list_pagination` |
| S07b 頁大小上限（205 筆，要求 10000 → 200 筆＋cursor，第二頁 5 筆） | 通過 | 通過 | 通過 | 通過 | 通過 | 通過 | `s07b_page_cap` |
| S08 `fix_stale_receiving`（0 頁→failed、2 頁→partial，皆 `shutdown`／`system`，ended_at＝時鐘，duration 30，已完成者不動，第二次回 0） | 通過 | 通過 | 通過 | 通過 | 通過 | 通過 | `s08_stale_receiving` |
| S09 並行（10 個 task 各自 insert＋3 次 update＋finalize；備註 5 回合 × 2 個 task 同 token 競爭＝恰一成功一 `Conflict`；同一 Call-ID 6 個 task 同時 insert＝1 成功、其餘 `Duplicate`） | 通過 | 通過 | 通過 | 通過 | 通過 | 通過 | `s09_concurrency` |
| S10 settings（空 DB 播種 3 個 key；線上改值；「重啟」用舊設定檔再播種＝0 筆且不覆蓋；真的重新連線後同樣；新 key 只補缺的；非法值／`run_at`／`records<files` 被拒；表內垃圾值會回報） | 通過 | 通過 | 通過 | 通過 | 通過 | 通過 | `s10_settings` |
| S11（store 部分 a）`upsert_finished`＝`db backfill` 的核心：未知 Call-ID 插入、重播不重複且保留座席資料、只有 receiving 列時補完成 | 通過 | 通過 | 通過 | 通過 | 通過 | 通過 | `s11_backfill` |
| S11（store 部分 b）斷線與重連：`docker stop` 後寫入在 < 40 s 內回 transient 錯誤（`Unavailable`），不 panic；`docker start` 後同一個 store 物件不需重建即恢復（輪詢最久 120 s）；斷線前的資料完整、失敗的寫入沒有殘留、之後的寫入與 finalize 正常 | N/A | N/A | 通過 | 通過 | 通過 | 通過 | `s11b_outage_and_reconnect`（需要 `SIP_FAXD_TEST_DB_CONTAINER`；以寫鎖獨占，期間不會有其他案例打同一台伺服器；SQLite 沒有伺服器可停，標為 N/A） |
| S12（額外）`delivery_attempts` 資料表可寫可讀 | 通過 | 通過 | 通過 | 通過 | 通過 | 通過 | `s12_deliveries_table` |
| R01 邊界（保留天數前後各 1 秒；剛好等於 cutoff 不清；時鐘前進 1 秒後才清；檔案與紀錄兩種邊界） | 通過 | 通過 | 通過 | 通過 | 通過 | 通過 | `r01_boundary` |
| R02 檔案清整（TIFF、PDF、PNG 快取目錄刪除、`files_purged_at`＝注入時鐘、紀錄與路徑欄位保留、沒有檔案的語音來電不被標記、重跑為 0、檔案先前已消失視為成功） | 通過 | 通過 | 通過 | 通過 | 通過 | 通過 | `r02_file_purge` |
| R03 紀錄清整（`files_days=0` 時先刪殘留檔案與 PNG，再刪紀錄與其 delivery_attempts；較新的紀錄與其 delivery 不動） | 通過 | 通過 | 通過 | 通過 | 通過 | 通過 | `r03_record_purge` |
| R04 刪檔失敗（`FileOps` 注入 PermissionDenied：不設 `files_purged_at`、`errors` 有訊息、檔案仍在；紀錄清整時該列保留；權限恢復後下一次執行成功） | 通過 | 通過 | 通過 | 通過 | 通過 | 通過 | `r04_delete_failure` |
| R05 receiving／`files_days=0`／`records_days=0`／`records<files` | 通過 | 通過 | 通過 | 通過 | 通過 | 通過 | `r05_never_cases` |
| R06 路徑安全（`../`、`a/../../`、絕對路徑在 data_dir 外、`/etc/hostname`、空字串、指向 data_dir 外的 symlink：全部拒絕、外部檔案存活、`files_purged_at` 不設、紀錄清整也保留該列；`resolve_in_data_dir` 純函式另有 9 個壞輸入與 3 個好輸入） | 通過 | 通過 | 通過 | 通過 | 通過 | 通過 | `r06_path_safety` |
| R07 dry-run（files_due＝2、records_due＝1、檔案與標記完全未動；之後真實執行結果與預告相同） | 通過 | 通過 | 通過 | 通過 | 通過 | 通過 | `r07_dry_run` |
| R08 批次（1200 筆到期：檔案階段 3 批、紀錄階段 3 批，purged＝deleted＝1200，剩 0） | 通過 | 通過 | 通過 | 通過 | 通過 | 通過 | `r08_batches` |

套件總數 37 個測試（含不連 DB 的單元測試）；`cargo test -p store`（SQLite）37 通過；`db-matrix` 對每個伺服器 37 通過（耗時：PostgreSQL 34 s、MySQL 35 s、MariaDB 33 s、MSSQL 57 s）。證據檔：`target/db-matrix-<backend>.log`。

### 3.1 測試是否有牙齒（人工變異）
以下變異各自讓對應案例失敗（SQLite 上實測，之後還原並確認檔案相同）：keyset 條件 `id < ?` 改 `<=`（S07、S07b 失敗）；檔案清整 `ended_at < ?` 改 `<=`（R01 失敗）；LIKE 前綴不跳脫（S04 失敗）；`set_note` 不讓版本遞增（S06 失敗）；清整查詢拿掉「不含 receiving」（R05 失敗）。

## 4. 涵蓋率（SQLite 執行）
`cargo llvm-cov -p store --fail-under-lines 80` → **TOTAL lines 82.87 %**（門檻 80，通過）。

| 檔案 | 行涵蓋率 |
|---|---|
| sqlstore.rs | 98.11 % |
| purge.rs | 96.05 % |
| model.rs | 97.83 % |
| backend/sqlite.rs | 97.73 % |
| lib.rs | 93.06 % |
| error.rs | 93.75 % |
| migrate.rs | 86.75 % |
| db.rs | 85.25 % |
| backend/mssql.rs | 57.89 %（只有 URL 解析等純函式；連線與查詢靠 db-matrix） |
| backend/postgres.rs | 21.74 %（SQLite 執行下幾乎不走；靠 db-matrix） |
| backend/mysql.rs | 17.86 %（同上） |

**誠實說明**：80 % 這個數字是 SQLite 單跑的結果，伺服器驅動程式（約 11 % 的行）在這個 gate 下幾乎沒被涵蓋，它們的行為靠 db-matrix 驗證，而 CI 不量測 db-matrix 的涵蓋率。如果 Opus 要求「每個驅動都被量到」，需要在 db-matrix job 也跑 `cargo llvm-cov`（可做，但本階段沒做）。

## 5. 原始失敗與根因（過程中遇到的所有失敗）
| # | 現象 | 根因與處置 |
|---|---|---|
| 1 | 第一次 PostgreSQL 全套件有 3 個案例失敗（`expected to read 5 bytes, got 0 bytes at EOF`） | 測試問題，不是 store：S11 的斷線測試 `docker stop` 伺服器時，其他案例仍在同一台伺服器上跑。改為所有案例持 `DB_LOCK` 讀鎖、S11b 持寫鎖獨占。 |
| 2 | MySQL／MariaDB：`call_id` 解碼失敗 `Rust type String (as SQL type VARCHAR) is not compatible with SQL type VARBINARY`（22／19 個案例失敗） | `call_id` 用 `utf8mb4_bin` 定序（Call-ID 區分大小寫），MySQL 對 `_bin` 定序的欄位回報 binary 旗標，sqlx 對映成 VARBINARY。**驅動層解法**：文字欄位先試 `String`，失敗改讀 `Vec<u8>` 再轉 UTF-8（`backend/mysql.rs::text`）。這是唯一一個需要後端專屬變通的地方。 |
| 3 | 本機 `cargo zigbuild`（Homebrew zig 0.16.0）編不過任何含 C 的依賴：`ar: error: unable to open …/libsqlite3.a`（libsqlite3-sys、aws-lc-sys 都一樣） | 本機工具問題：zig 0.16 的 ar 與 cargo-zigbuild 0.22 不相容；CI 使用 zig 0.14.1。改在 Rocky 9 容器內以 zig 0.14.1 驗證，成功（見 §6.3）。**注意**：M3b 之後 sip-faxd 會連結 store，本機 `scripts/ci-local.sh` 的 musl 建置步驟在這台 Mac 上會因此失敗，需要改用 zig 0.14.x（例如 `brew install zig@0.14` 或 zvm）。 |
| 4 | tiberius 0.12.3＋sqlx 0.8.6 時 `cargo deny check` 失敗 | 見 §6.1。 |
| 5 | `scripts/ci-local.sh` 第一次在 clippy 失敗（測試程式碼 4 個 lint：`assert_eq!` 對字面 bool、`x || x==`、`1 * DAY`、`needless_range_loop`） | 修正測試程式碼；其中兩個斷言原本幾乎是空的（`count >= 0`、`> || ==`），改成精確斷言（表內筆數 0／1；版本嚴格遞增）。 |
| 6 | S03 初版斷言少算一筆 | 測試寫錯（微秒截斷那筆也落在區間內）；修正期望值。 |

## 6. 依賴與靜態連結（ADR-003／ADR-004 的驗證）
### 6.1 版本選擇與 `cargo deny`
- **sqlx 0.9.0**（`runtime-tokio`、`tls-rustls-ring-webpki`、`sqlite`（bundled）、`postgres`、`mysql`、`chrono`，`default-features = false`）＋ **tiberius 0.13.0**（`rustls`、`chrono`、`tds73`，`default-features = false`）＋ bb8 0.9。
- 先用過 sqlx 0.8.6＋tiberius 0.12.3（它們符合工作區 `rust-version = 1.85`），但 `cargo deny check` 有 9 個 advisory 失敗：`rsa 0.9.10`（Marvin，無修補版，來自 sqlx-mysql 0.8）、`rustls 0.21.12`／`rustls-webpki 0.101.7`（tiberius 0.12 的 rustls 0.21：數個憑證驗證與 CRL 解析弱點，RUSTSEC-2026-0382 等）、`rustls-pemfile` 無人維護、`webpki-roots 0.26.11`（RUSTSEC-2026-0381）。升級到 sqlx 0.9＋tiberius 0.13 後 **`cargo deny check`：advisories ok, bans ok, licenses ok, sources ok**（沒有新增 ignore，沒有 openssl-sys／native-tls）。
- 代價：sqlx 0.9 的 `rust-version` 是 1.94、tiberius 0.13 是 1.88，高於工作區宣告的 `rust-version = 1.85`（實際 toolchain 釘在 1.99.1，沒有問題；CI 用 `stable`）。**建議 Opus 把根目錄 `[workspace.package] rust-version` 調到 1.94**（我只被允許改 `[workspace.dependencies]`，沒有動）。
- sqlx 0.9 的 API 變動：動態 SQL 字串需包 `sqlx::AssertSqlSafe(...)`。store 只送「寫死的範本＋以 `i64` 格式化的 id 清單」，其餘值一律用綁定參數，見 `backend/*.rs`。

### 6.2 TLS 與密碼學後端
- sqlx 走 `rustls` ＋ **ring**；tiberius 0.13 的 `tokio-rustls` 使用預設的 **aws-lc-rs**（無法從我們這邊關閉，因為是它對 tokio-rustls 的預設 feature）。結果依賴樹中兩個 rustls crypto provider 並存（ring、aws-lc-sys）。兩者都能在 musl 靜態連結（§6.3），沒有 OpenSSL。代價是多一個 C/asm 依賴要編譯（aws-lc-sys；CI 的 zig 0.14.1 在 Rocky 容器內約 12 分鐘冷建置，`rust-cache` 之後正常）。如果不想要兩個 provider，替代方案是等 tiberius 提供 ring 的 feature，或 fork/patch；列為 open issue。
- MSSQL 預設 `encrypt=required` 並驗證憑證；自簽憑證須明確加 `trust_server_certificate=true`。

### 6.3 musl 靜態
- 在 `sip-fax-rs-test:rocky9` 容器（linux/amd64）內安裝 zig 0.14.1＋`pip install cargo-zigbuild`，執行 `cargo zigbuild --release --target x86_64-unknown-linux-musl -p store --tests`：成功；產出的測試執行檔 `file` 顯示 **`ELF 64-bit LSB executable, x86-64, … statically linked`**、`ldd`：`not a dynamic executable`；並在容器內**實際執行**該執行檔（SQLite 目標，37 個測試）：**37 通過**。
- 因 sip-faxd 在 M3b 之前不連結 store，此驗證放進 CI 的 `build-musl`（x86_64 與 aarch64 皆建置並檢查 `statically linked`；x86_64 還會執行該執行檔）。**這個 CI 步驟（`jq` 解析 `--message-format=json`）我無法在本機完整演練，只驗證了其中的建置與靜態檢查部分，需 push 後觀察第一次 CI。**

## 7. 對規格的偏離與補充
| 項目 | 偏離／補充 | 理由 |
|---|---|---|
| `settings` 欄位名稱 | `key` → `name`（`value` 不變） | `key` 是 MySQL 保留字 |
| `schema_migrations` | 多一個 `name` 欄位 | 便於除錯 |
| `faxes` 索引 | `(started_at DESC)` 改為 `(started_at DESC, id DESC)`；另加 `(ended_at)` | keyset 排序含 id；清整查詢走 `ended_at` |
| `call_id` 定序 | MySQL `utf8mb4_bin`、MSSQL `Latin1_General_100_BIN2` | SIP Call-ID 區分大小寫；其他欄位仍用 DB 預設的不分大小寫定序 |
| 欄位長度 | 在 Rust 端先檢查（UTF-16 單位，最嚴格者），超過一律回 `TooLong` | 否則 PostgreSQL／MySQL strict／MSSQL 報錯而 SQLite 默默接受，四個後端行為不一 |
| `set_note(…, expected_updated_at)` | `None` 代表「期待從未編輯過」；新版本時間戳保證嚴格大於舊值（時鐘凍結或同毫秒也成立） | 否則同毫秒的兩次編輯無法區分 |
| `update_pages` | 只往前（`pages < new`）、只作用於 `receiving` | 佇列重送／亂序時不倒退 |
| `finalize` | 保留 `is_read`／`note*`；`duration_sec` 缺省時自行計算（四捨五入到秒） | 使用者資料不可被覆蓋 |
| `upsert_finished` | 規格沒寫；補上供 `db backfill`／M3b 使用 | S11 的 store 層級需求 |
| 清整資格 | 檔案清整只針對**有檔案路徑**的紀錄 | 語音來電（`no_fax`）沒有檔案，不應被標記「檔案已清除」 |
| 紀錄清整遇到刪不掉的殘留檔 | 該列保留，下次再試 | 否則會留下沒有紀錄指向的孤兒檔 |
| 同步檔案操作 | `Purger` 在 async 函式中直接呼叫 `std::fs`（每個檔案一次） | 每天一次的背景工作，且每批最多 500 筆；若 Opus 在意可改 `spawn_blocking` |
| S11「收傳真不受影響」「`db backfill` CLI」 | **不在 M3a**：寫入佇列、指數退避、`sip-faxd db backfill` 屬於 M3b（fax-in 側） | 本階段只提供 transient 錯誤分類（`Error::is_transient`）與 `upsert_finished` |
| 本階段未做 | `sip-faxd db migrate／purge` CLI、`[database]`／`[retention]` 設定檔解析、README 與 `conf/sip-faxd.toml.example` 同步 | 分別屬於 M3b（sip-faxd／fax-in）；我不被允許改這些檔案。`docs/database.md` 已先寫好欄位說明（驗收條件之一） |

## 8. 提議的 ADR（供 Opus 合併到 `docs/DECISIONS.md`）
- **ADR-A：store 以「單一 SQL 實作＋四個薄驅動」實現 `FaxStore`**。取代「每個方言各寫一個 Repository」。方言差異集中在：`?` 佔位符改寫（`$n`／`@Pn`）、`LIMIT`／`TOP`、`RETURNING`／`OUTPUT INSERTED`／`last_insert_id`、時間型別編碼、整數寬度與 MySQL `_bin` 文字欄位解碼。好處：一份行為、一份測試；風險：不能使用任何方言專屬 SQL（目前不需要）。
- **ADR-B：sqlx 0.9 ＋ tiberius 0.13（修訂 ADR-003 的版本）**。理由見 §6.1（0.8／0.12 的相依有未修補的 advisory）。附帶：根 `rust-version` 需提高到 1.94。
- **ADR-C：接受 ring 與 aws-lc-rs 並存**（sqlx 用 ring、tiberius 預設 aws-lc-rs），兩者皆可靜態連結 musl，無 OpenSSL。
- **ADR-D：時間欄位統一 UTC 毫秒；MySQL 以 naive UTC 綁定**，不使用 `TIMESTAMP`（有 2038 與連線時區轉換問題）。
- **ADR-E：資料庫版本比程式新時拒絕啟動；migration 不提供降版。**（規格已有，建議補入決策紀錄。）

## 9. 分支保護
`scripts/branch-protection.json` **沒有修改**：新 job 的檢查名稱是由 `name: db-matrix (${{ matrix.db }})` 產生，預期為下列四個，但我無法在 push 前確認 GitHub 實際顯示的名稱。第一次 CI 跑完後請確認，再加入 `contexts`：
```
"db-matrix (postgres)", "db-matrix (mysql)", "db-matrix (mariadb)", "db-matrix (mssql)"
```
（`release` job 的 `needs` 已加入 `db-matrix`。）使用者需要重新套用 branch protection。

## 10. Open issues／風險
1. **CI 的 `db-matrix` job 未在 GitHub 上實跑過**。本機已用相同映像、相同 URL 形式、相同環境變數驗證套件；但 services 容器的健康檢查指令引號、`docker ps --filter ancestor=…` 找容器、以及 S11 在 Actions runner 上的 `docker stop/start` 需要第一次 CI 觀察。若 S11 在 CI 上不穩，可把 `SIP_FAXD_TEST_DB_CONTAINER` 拿掉（該案例會略過斷線部分並印出訊息）。
2. MySQL／MariaDB 上的 DDL 不是交易式，migration 中途失敗會留下半套結構，重跑會因 `CREATE TABLE` 已存在而失敗，需人工處理。SQLite／PostgreSQL／MSSQL 可改成交易式 migration（目前未做）。同時兩個程式實例第一次 migrate 同一個資料庫也沒有鎖保護（已寫入 `docs/database.md`）。
3. 伺服器驅動的行數涵蓋率不在 80 % gate 內（§4）。
4. 斷線測試只測了「容器停止／啟動」（連線被重設／拒絕）；沒有測封包黑洞（防火牆 DROP）情境，該情境會靠 sqlx 10 s 取用逾時與 tiberius 連線逾時回 transient。
5. 本機 zig 0.16 問題（§5 #3）。
6. `cargo deny` 仍有 14 則 `multiple-versions` 警告（沒有失敗），其中一部分來自 store 新增的相依。
7. 工作區 `rust-version` 需調整（§6.1）。
8. `ListFilter` 目前只支援單一 `fax_status`；M4 若要多選，需擴充。

## Opus 審核結論（2026-10-21）
**結論：通過。**

整合時把 root 的 `rust-version` 提高到 1.94（sqlx 0.9 需要）。MSRV 提高後，clippy 會啟用 let-chain 和 `is_multiple_of` 這兩類 lint，整個 workspace 已用 `clippy --fix` 機械性修正。之後重新執行 `scripts/ci-local.sh`，全部通過。

已檢查：
- **跨後端一致性**：37 個行為測試在 SQLite（檔案和 `:memory:`）、PostgreSQL、MySQL、MariaDB、MSSQL 上都通過。S11 實際停掉、再啟動 DB 容器，驗證能自動恢復。
- **測試有抓錯能力**：5 次 mutation 都被測試抓到。
- **清整的路徑安全檢查**：除了要求路徑在 `data_dir` 內，也拒絕 `..` 和 symlink，比規格要求的更嚴。
- **依賴改版**：發現 sqlx 0.8 和 tiberius 0.12 的相依套件有未修補的 advisory，主動升到 0.9 和 0.13，cargo-deny 零例外通過。這是正確的判斷（ADR-037）。

需要追蹤（列入 M3b）：
1. **migration 的並行保護**：兩個實例同時第一次啟動時沒有鎖。要依各 DB 加上 advisory lock（`pg_advisory_lock`、`GET_LOCK`、`sp_getapplock`、SQLite 的 exclusive transaction）。
2. **MySQL/MariaDB 的 DDL 不是交易性的**：migration 失敗時可能留下半套 schema。要讓 migration 腳本可以安全重跑（`IF NOT EXISTS`），或在報錯時提供明確的修復說明。
3. **本機 musl 建置**：M3b 讓 sip-faxd 連結 store 之後，本機的 zig 0.16 無法建置 C 相依（CI 用的是 0.14.1）。要把 `ci-local.sh` 的 musl 步驟改到 Docker 裡執行，或固定 zig 的版本。
4. **db-matrix 的必要檢查**：GitHub 第一次跑完後，確認檢查名稱，再加進 branch protection。

可接受的：
- ring 和 aws-lc-rs 並存（ADR-038）。代價是執行檔會變大，M6 再評估能否只保留一個。
- 涵蓋率只量測 SQLite（82.9%）。其他後端的行為由 db-matrix 負責驗證。
