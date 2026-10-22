# M4e 測試報告：資料庫搬移 `sip-faxd db copy`

- 實作：Opus（`crates/store/src/copy.rs`、`apps/sip-faxd/src/commands.rs::db_copy`）；測試與本報告：Sonnet（依 Opus 的測試計畫）
- 計畫：`docs/test-plans/M4e.md`；ADR-059；共通規則 `docs/test-plans/M4.md` §0
- 分支：`m4e-db-copy`
- 狀態：待 Opus 審核（§9）

## 1. 環境

| 項目 | 內容 |
|---|---|
| 主機 | macOS（arm64），Docker Desktop |
| store 測試 | 本機直接跑（SQLite 檔案）；`scripts/db-matrix.sh` 在 Docker 起 PostgreSQL 16、MySQL 8.0、MariaDB 11、MSSQL 2022（linux/amd64 模擬），以 `SIP_FAXD_TEST_DB=<url>` 跑整套 store 測試（70 個，含本次 8 個） |
| CLI 測試 | 真的 `sip-faxd` 執行檔（`CARGO_BIN_EXE_sip-faxd`），兩個 SQLite 檔；Web 用 `[web] scheme = "http"`，以 `reqwest` 對真的伺服器發請求 |
| E2E（C12） | `scripts/e2e-docker.sh`：Rocky 9 測試映像；腳本另起一個 `postgres:16` 容器（隨機 host port），測試容器以 `host.docker.internal` 連線（`SIP_FAXD_E2E_PG_URL`）；傳真由 SpanDSP 的 `t38_terminal` 真的以 T.38 送進容器內的 `sip-faxd` |

## 2. 案例對照表

測試函式以案例編號開頭。比對方式：兩邊各以原始查詢（`SELECT <每個欄位> … ORDER BY id`，經 `store::db::Backend`，不經 `FaxStore`）讀出全部七個表，與**測試自己寫入的預期值**逐欄位比對（來源、目標各比一次，所以兩邊共同的解碼錯誤不會被掩蓋）。

資料集（`copy_tests.rs` 的 `seed_rows`）：七個表都有資料；`faxes` 有「所有欄位填滿且字串在欄位長度上限」（`call_id` 255、`trunk` 64、`ani`／`dnis` 64、各 `*_source` 16、`remote_ident` 32、`hangup_*` 8／32／255、`fax_status` 12、`resolution` 12、`compression` 8、路徑 1024、`*_by` 64、`note` 4000 字加 emoji）、「接收中、可為 NULL 的欄位全 NULL」、「emoji／CJK／SQL 特殊字元／換行」、「空字串備註」、「已清整」等列；時間含毫秒；整數含 `i32` 上限、2^53+1 的 BIGINT 與負數；布林真假；`config_sections` 含 `enc:v1:` 密文與 64 字元的名稱；`users` 含停用與必須改密碼的帳號；`sessions` 含 NULL 的 ip／user agent 與 255 字元的 user agent。每個識別欄位表都先插入再刪除幾列，讓 `id` 有缺口（最大 id ≠ 筆數）。測試另外斷言資料集本身每個欄位至少有一次非 NULL，`faxes` 每個可為 NULL 的欄位至少有一次 NULL。

「矩陣」案例（`copy_case!`）：本機 SQLite → SQLite；設了 `SIP_FAXD_TEST_DB` 時兩個方向都跑（新的 SQLite 檔 → 伺服器 DB，以及新的伺服器 DB → SQLite 檔）。

| ID | 測試位置 | 驗證內容 | 結果 |
|---|---|---|---|
| C01 | `crates/store/src/tests/copy_tests.rs::c01_sqlite_to_sqlite_copies_every_row_and_column_with_the_same_ids` | 上述資料集 SQLite → SQLite：來源讀回與寫入值相同；`CopyReport` 的來源／目標種類、schema 版本、表名順序、每表列數正確，progress 回呼與報告一致；目標每表每欄與來源、與預期值相同；`id` 保留（缺口還在，最大 id 7、只有 5 列）；來源複製後不變 | 通過 |
| C02 | `copy_tests.rs::c02_matrix_copy_keeps_nulls_booleans_milliseconds_unicode_and_limit_length_values` | 同 C01，於 SQLite → {PostgreSQL、MySQL、MariaDB、MSSQL} 與 {四者} → SQLite 共八個方向 | 通過（四種 DB 各 2 個方向） |
| C03 | `copy_tests.rs::c03_new_ids_after_the_copy_continue_above_the_copied_ones`（矩陣） | 複製後以目標的 `FaxStore`／`AdminStore` 新增傳真、投遞、稽核、帳號、session：每個新 `id` 都大於複製來的最大值（PostgreSQL `setval`、MSSQL identity、MySQL auto-increment）；舊的列逐欄位不變，每個識別欄位表剛好多一列 | 通過 |
| C04 | `copy_tests.rs::c04_a_target_with_data_in_any_table_is_refused_and_left_alone`（矩陣） | 目標七個表分別預先放一列（`faxes`、`delivery_attempts`、`settings`、`config_sections`、`config_audit`、`users`、`sessions`）：每次回 `Invalid`、訊息含 `not empty` 與第一個非空的表名；目標逐欄位不變、來源不變；清空目標後同一個來源複製成功 | 通過 |
| C05 | `copy_tests.rs::c05_an_older_or_newer_source_is_refused_and_the_target_is_not_written` | 來源 schema 版本 1：`SchemaOutdated`，訊息含 `db migrate`；來源記錄版本 99：`SchemaTooNew`，訊息含 `newer`；兩者目標七表都是空的，且以「不存在的 SQLite 檔」當目標時該檔案沒有被建立（來源檢查在開啟目標之前）；來源的資料列還在 | 通過（僅 SQLite，計畫未要求矩陣） |
| C06 | `copy_tests.rs::c06_a_row_the_target_refuses_rolls_everything_back`（矩陣） | 讓目標拒絕最後一個表（`sessions`）的列：目標是伺服器 DB 時，來源 `user_agent` 改成 300 字元（超過 `VARCHAR(255)`／`NVARCHAR(255)`）；目標是 SQLite 時（無長度限制）在目標建 `BEFORE INSERT` 觸發器 `RAISE(ABORT)`。回錯誤、訊息含 `copying table sessions` 與 `from id <該批第一個 id>`；**目標七個表都是空的**（前六個表已寫入的列也被 rollback）；排除原因後在同一個目標重試成功，逐欄位與來源相同 | 通過 |
| C07 | `apps/sip-faxd/tests/m4e_cli.rs::c07_db_copy_refuses_while_the_service_runs_the_same_database_and_a_bad_url` | 真的 `serve` 在跑（PID 檔被鎖）：退出碼 1、`the service is running (pid …)`、`stop it first`，目標檔案沒有被建立、沒有輸出 `copying`；`--to` 與設定檔相同：`the target is the source database`；同一檔案換個寫法（`/./`）：因目標非空而拒絕；錯誤格式（`ftp://…`、`not a url`、`oracle://…`、空字串）：退出碼 1、`--to:`、無輸出；每次來源列數不變、目標沒有被建立；服務停止後同一個指令成功 | 通過 |
| C08 | `m4e_cli.rs::c08_secrets_copy_as_ciphertext_and_decrypt_with_the_same_key_only` | 來源有 trunk 註冊密碼、Email 密碼、webhook 金鑰（皆 `enc:v1:`）：`config_sections` 與 `config_audit` 兩邊完全相同，目標沒有任何明文；目標的設定檔放在同一個目錄（同一份 `secret.key`）：`config check` 退出碼 0、沒有 `cannot be decrypted`，`config export --include-secrets` 有三個明文且與來源的輸出完全相同；對照：換另一把 key 時 `config check` 退出碼 1、訊息含 `cannot be decrypted`，export 沒有任何明文 | 通過 |
| C09 | `m4e_cli.rs::c09_the_web_api_on_the_copied_database_keeps_accounts_sessions_read_flags_and_notes` | 以 CLI 建立帳號（`user add`）、真的 `serve` 登入取得 session cookie、停止、`db copy`、在目標上啟動 `serve`：log 沒有 `created the administrator`；舊 cookie 對 `GET /auth/me` 回 200 且是 `alice`；對照：不存在的 cookie 回 401；`alice`、`bob` 以原密碼登入 200，錯誤密碼 401 且無 cookie；`GET /faxes` 三筆，`id` 與來源相同，已讀旗標與 `read_changed_by`、備註（含 emoji）與 `note_updated_by` 正確，其他兩筆備註為 null；`GET /faxes/{id}` 同 | 通過 |
| C10 | `copy_tests.rs::c10_batch_boundaries_copy_every_row_in_order` | 先斷言批次常數（每批 INSERT 55 列、投遞 100 列、讀取分頁 500）；傳真（每筆一列投遞）0、1、54、55、56、110、499、500、501、1001、5000 筆：列數、順序（`id` 1..n 無缺漏）與每個欄位都與預期值和來源相同 | 通過（僅 SQLite，計畫未要求矩陣） |
| C11 | `m4e_cli.rs::c11_output_has_the_rows_per_table_masks_the_password_and_tells_the_next_step` | 七個表每行的列數等於來源的列數（七個表都有資料）；`done: N rows from sqlite into sqlite (schema version 2)` 的 N 是總和、`verified`；`copying <來源> -> <目標>`；最後的 `next:` 提示含 `[database] url`、目標 URL、`secret.key`、`start the service`；目標為 `postgres://faxuser:Sup3rS3cret@127.0.0.1:1/faxdb`（連不上）時進度行顯示 `faxuser:***@`，stdout／stderr 都沒有密碼，退出碼 1 且訊息含 `db copy:`。（成功路徑下伺服器 URL 的遮蔽另由 C12 驗證） | 通過 |
| C12 | `tools/fax-sim/tests/e2e_m4e.rs::c12_a_real_fax_moves_from_sqlite_to_postgresql_with_read_note_previews_and_ids` | 見下 | 通過（約 100 秒） |
| C13 | `copy_tests.rs::c13_the_copy_lists_exactly_the_columns_of_the_schema`（`db_case!`） | 各 DB 建好 schema 後以 `pragma table_info`／`information_schema.columns` 列出所有（表、欄位）：資料表集合（扣掉 `schema_migrations`）等於 `copy::table_names()`；欄位集合等於 `copy::TABLES`，也等於測試資料集的欄位表；`TABLES` 與測試資料集的欄位順序、型別、identity 都相同。對照（變異）：從 `TABLES` 刪掉 `note_updated_at` 時 C13、C01、C02、C03、C04、C06 全部失敗 | 通過（四種 DB 與 SQLite） |

### C12 細節

1. `FaxdOptions` 起一個 SQLite 的 `sip-faxd serve`；SpanDSP 以 T.38 送 2 頁精細傳真（TSI `0000000001`）；等到最終紀錄（`received`、2 頁）。
2. 直接對 SQLite 設為已讀（`amy`）並寫備註（`回電給客戶 📞 ref#42`），讀出整列 `FaxRecord` 當基準；記錄磁碟上每頁的整頁與縮圖 PNG（簽名、位元組）、使用者數與稽核列。
3. SIGTERM 停止（log 無 ERROR），`sip-faxd db copy --to postgres://postgres:***@host.docker.internal:PORT/postgres`：退出碼 0；輸出裡密碼被遮成 `***`、stdout／stderr 都沒有明文密碼；`from sqlite into postgres`、`verified`、`next: set [database] url`；PostgreSQL 的 `faxes` 剛好 1 列。
4. 以同一個資料目錄、同一個 SIP port 在 PostgreSQL 上啟動新的 `serve`：舊傳真的整列 `FaxRecord` 與 SQLite 基準 `assert_eq`（每個欄位）；已讀、`read_changed_by`、備註、`note_updated_by` 正確；使用者數不變（沒有第二個 admin）、複製前的每一列稽核都在；PNG 檔案位元組沒變，以 PostgreSQL 的紀錄呼叫 `Preview::page_png`（兩頁 × 整頁／縮圖）回傳的就是原本的檔案、`conversions() == 0`（沒有重新轉檔）。
5. 再收一份 1 頁傳真（TSI `0000000002`）：`assert_success`＋`verify_files`（libtiff 與 poppler 逐像素）；新紀錄 `id = 舊 id + 1`、未讀、無備註；舊傳真在新傳真寫入後仍與基準相同；列表剛好 2 筆；新服務的 log 沒有 `created the administrator`、沒有 ERROR、沒有資料庫密碼。

## 3. 發現並修正的產品問題

**沒有找到產品錯誤。** 八個矩陣方向、C06 的 rollback（含 MySQL 的 `autocommit = 0`、MSSQL 的 `IDENTITY_INSERT`、PostgreSQL 的 `OVERRIDING SYSTEM VALUE`＋`setval`）與 C12 的 PostgreSQL 都在第一次完整執行時通過。為確認測試不是空的，另做了一次變異：刪掉 `TABLES` 中的 `note_updated_at`，C13、C01、C02、C03、C04、C06 失敗（C05、C10 與該欄位無關而通過；之後已還原）。

為了測試而動的程式（不改行為）：
- `crates/store/src/copy.rs`：`Table` 與 `TABLES` 改為 `pub(crate)`，讓 C13 比對欄位清單。
- `crates/store/src/lib.rs`：新增 `#[doc(hidden)] pub async fn test_row_counts(url)`（與既有的 `test_record_schema_version` 同類），供 CLI／E2E 測試以唯讀方式數出七個表的列數。

## 4. 與計畫不同之處

- **C04**：七個表都測了，但 `delivery_attempts`（需要父列 `faxes`）與 `sessions`（需要父列 `users`）因外鍵無法單獨放資料，所以訊息指出的是第一個非空的表（`faxes`、`users`），測試以此為預期；其餘五個表指出的就是該表。
- **C05、C10** 只跑 SQLite（計畫的驗收只要求 C02、C03、C04、C06 在四種 DB 上通過）。
- **C06 的 SQLite 目標**：SQLite 沒有欄位長度限制，改用目標上的 `BEFORE INSERT` 觸發器拒絕一列（伺服器 DB 為目標時用過長的文字）。拒絕發生在最後一個表（`sessions`），所以前六個表的 rollback 也被驗證；另加「排除原因後重試成功」。
- **C07、C08、C09、C11 的 CLI 測試兩邊都是 SQLite**（伺服器 DB 之間的搬移由 store 矩陣與 C12 涵蓋）。C11 的「密碼顯示為 `***`」在 CLI 測試以「連不上的 PostgreSQL URL」驗證進度行與錯誤訊息，成功路徑則在 C12 驗證。
- **C09 的 session 驗證**：以真的登入取得 cookie，沒有在 DB 直接插入 session；Web 以 HTTP（不是 HTTPS）執行，與其他 CLI 測試相同。
- **C12 在複製後使用同一個資料目錄與 SIP port**；`[capture]` 是執行期設定（存在 DB），複製後的服務沿用第一個服務的 capture 目錄，所以第二份傳真的 `result.json` 從該目錄讀取。
- C12 的 PostgreSQL 由 `scripts/e2e-docker.sh` 自動起一個容器（`E2E_PG_URL=…` 可改用現成的空資料庫、`E2E_NO_PG=1` 不起；沒有 `SIP_FAXD_E2E_PG_URL` 時測試直接失敗並說明，不會靜默略過）。CI 的 `e2e-spandsp` job 新增 `services: postgres` 與 `--add-host=host.docker.internal:host-gateway`、`SIP_FAXD_E2E_PG_URL`（尚未在 GitHub 上實際跑過）。
- sip-faxd 新增 dev-dependency `reqwest`（C09 的 HTTP 請求）；`sip-faxd` 的 CLI 測試檔以 `#![cfg(unix)]` 限定（PID 檔 flock 的行為）。

## 5. 涵蓋率

`cargo llvm-cov -p store --fail-under-lines 80 --summary-only --ignore-filename-regex 'backend[\\/](mssql|mysql|postgres)\.rs'`

| 項目 | 數字 | 門檻 |
|---|---|---|
| `store` 全部（行） | **83.08%**（2819 行中未涵蓋 477） | ≥ 80% |
| `copy.rs`（行） | 83.78%（370 行中未涵蓋 60；區域 85.65%） | — |

`copy.rs` 未涵蓋的主要是只在伺服器 DB 上執行的分支（PostgreSQL `setval`、MSSQL `IDENTITY_INSERT`、MySQL `autocommit`），它們由 db-matrix 與 C12 執行（涵蓋率排除伺服器驅動，ADR-045），以及驗證失敗（`verification failed`）的錯誤路徑。

## 6. 執行指令

```sh
cargo test -p store copy_tests                          # C01–C06、C10、C13，本機 SQLite
SIP_FAXD_TEST_DB="postgres://…" cargo test -p store copy_tests   # 同上，伺服器 DB 為另一端（兩個方向）
scripts/db-matrix.sh [postgres mysql mariadb mssql]     # 整套 store 測試 + R04，四種伺服器 DB（每種數分鐘）
cargo test -p sip-faxd --test m4e_cli                   # C07、C08、C09、C11
E2E_ONLY="--test e2e_m4e" scripts/e2e-docker.sh         # C12（約 2 分鐘，加上 fmt／clippy／t4 oracle 約 5 分鐘）
E2E_ONLY="--test e2e_m4e" E2E_SKIP_CHECKS=1 scripts/e2e-docker.sh   # 只跑 C12
cargo llvm-cov -p store --fail-under-lines 80 --summary-only --ignore-filename-regex 'backend[\\/](mssql|mysql|postgres)\.rs'
```

`scripts/e2e-docker.sh`（不帶參數）與 `scripts/ci-local.sh` 會一併跑 C12。

## 7. 本機驗證結果

| 指令 | 結果 |
|---|---|
| `cargo fmt --all --check`、`cargo clippy --workspace --all-targets -- -D warnings` | 通過 |
| `T4_ORACLE=skip cargo test --workspace --all-targets` | 通過（不設 `T4_ORACLE=skip` 時本機的 `f08_pdf_is_valid_for_poppler…` 因沒有 poppler 失敗，與本次無關，`ci-local.sh` 本來就設 skip） |
| `scripts/db-matrix.sh`（postgres、mysql、mariadb、mssql 各執行一次） | 四種都 PASSED：store 70 個測試（含本次 8 個）、doctest、R04 |
| `cargo llvm-cov -p store …` | 83.08%，通過 |
| `cargo test -p sip-faxd --test m4e_cli` | 4 個通過（約 11 秒） |
| `E2E_ONLY="--test e2e_m4e" scripts/e2e-docker.sh`（含 `tools/` 的 fmt、clippy、t4 oracle） | 通過；C12 約 100 秒 |

**沒有執行**：完整的 `scripts/ci-local.sh`、`cargo deny`、其他 crate 的涵蓋率門檻、musl 建置、完整的 `scripts/e2e-docker.sh`（本次只跑 `--test e2e_m4e`）、`scripts/web-openssl-docker.sh`、`scripts/e2e-web.sh`；CI 上新增的 PostgreSQL service（`e2e-spandsp`）尚未在 GitHub 實際跑過。

## 8. 已知限制

- C12 只測 SQLite → PostgreSQL；MSSQL 為目標的端到端沒有跑（store 矩陣有）。
- C09 對複製後的服務只驗證到 Web API 層（HTTP）；沒有在瀏覽器（Playwright）中驗證。
- MSSQL 在 Apple Silicon 上以 linux/amd64 模擬執行，整套 store 測試約 85 秒。

## 9. Opus 審核

**結論：通過。**（2026-10-22）

- **空斷言／過度 mock**：沒有。store 測試在兩邊都以原始查詢逐表、逐欄位比對測試自己寫入的值；CLI 測試執行真的執行檔並對真的 HTTP 伺服器登入；C12 是 SpanDSP 真的送傳真。
- **C06** 讓最後一個表（`sessions`）的最後一列被拒，證明前面六個表也一起 rollback，並在同一個目標重試成功，設計正確。
- **C13** 以每種資料庫的 `information_schema`（SQLite 用 `pragma table_info`）比對欄位清單；報告中的突變測試（拿掉 `note_updated_at`）證實會失敗，可以擋住之後 migration 新增欄位卻忘了更新清單。
- **涵蓋率**：store 83.08%（門檻 80%），`copy.rs` 83.78%。四種伺服器 DB 的 db-matrix 都通過。
- **偏離**（§4）都合理：外鍵讓 C04 無法只在子表放資料；C05、C10 只在 SQLite（驗收標準沒有要求矩陣）。
- **待辦**：CI 的 `e2e-spandsp` 新增的 PostgreSQL service 要在 GitHub 上第一次跑過確認；MSSQL 作為目標只有 store 矩陣，沒有端到端（可接受，MSSQL 的路徑已在矩陣雙向驗證）。
- 審核時補文件：資料庫存的是相對於 `data_dir` 的檔案路徑，換機器時要連 `data/faxes/` 一起搬（`docs/database.md`、`docs/manual/it.md`）。
