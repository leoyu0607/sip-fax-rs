# M4f 測試報告：資料庫分欄位設定、`db check`、設定檔只留啟動設定

- 實作：Opus（`apps/sip-faxd/src/settings.rs`、`crates/store/src/check.rs`、`apps/sip-faxd/src/commands.rs`、`web-ui/src/App.svelte`）；測試與本報告：Sonnet（依 Opus 的測試計畫）
- 計畫：`docs/test-plans/M4f.md`；ADR-060；共通規則 `docs/test-plans/M4.md` §0
- 分支：`m4f-config`
- 狀態：**Opus 審核通過**（2026-10-23，§9）

## 1. 環境

| 項目 | 內容 |
|---|---|
| 主機 | macOS（arm64），Docker Desktop |
| 單元測試（D01、D03、D04） | `apps/sip-faxd/src/settings.rs::m4f_tests`：以字串與暫存檔解析；權限以 `std::os::unix` 設定 |
| CLI 測試 | 真的 `sip-faxd` 執行檔（`CARGO_BIN_EXE_sip-faxd`）：`apps/sip-faxd/tests/m4f_cli.rs`（SQLite、關閉的 port、解析不到的主機；`serve` 是真的行程，Web 用 HTTP，來電是真的 UDP INVITE）。共用輔助程式在 `tests/m4f_support/mod.rs` |
| 伺服器資料庫 | `apps/sip-faxd/tests/m4f_server.rs`：只在 `SIP_FAXD_TEST_DB` 是伺服器、且 `SIP_FAXD_TEST_DB_CONTAINER` 有值時執行；帳號與資料庫以 `docker exec` 在該容器裡用 `psql`／`mysql`／`mariadb`／`sqlcmd` 建立（測試完成後刪除）。`scripts/db-matrix.sh` 與 CI 的 `db-matrix` job（新增一個 step）都會執行。沒有設定時印出 `skipped`，db-matrix 與 CI 的 step 會把 `skipped` 當失敗 |
| store 測試 | `crates/store/src/tests/check_tests.rs`：`check_url` 在 SQLite 與四種伺服器 DB（`db_case!`） |
| E2E（F02 真傳真） | `scripts/e2e-docker.sh`：Rocky 9 測試映像；`tools/fax-sim/tests/e2e_m4f.rs`，SpanDSP 以 T.38 真的送進容器內的 `sip-faxd`；Web API 以原生 TCP 發 HTTP 請求（不新增依賴） |
| E2E（F05） | `scripts/e2e-web.sh`：Playwright（Chromium）對容器內第三個 `sip-faxd` 實例 `c`（沒有任何中繼，Web 在 8081，主機 18081）；原有 16 個案例照舊 |

## 2. 案例對照表

測試函式以案例編號開頭。

| ID | 測試位置 | 驗證內容 | 結果 |
|---|---|---|---|
| D01 | `settings.rs::m4f_tests::d01_the_fields_build_the_same_url_as_the_one_written_by_hand` | 四種伺服器類型（postgres、mysql、mariadb、mssql）各自的預設 port（5432／3306／3306／1433）與明確 port；`trust_server_certificate` true／false（只有 true 會出現在 URL）；沒有密碼、沒有 user、IPv6 主機（加方括號）；sqlite 的 `path` 與預設路徑；每個組出的 URL 與手寫的 URL 字串相同，且 `store::backend_for_url` 解出正確類型；`url` 原樣使用，沒有 `[database]` 時是預設 SQLite，`auto_migrate`／`max_connections` 與欄位並存 | 通過 |
| D01 | `…::d01_awkward_user_names_passwords_and_database_names_decode_to_what_was_written` | 帳號含空白與 `@`、密碼 `p@ss:w/rd?#1%41&+ 中😀`、資料庫名稱含 `/`：組出的 URL 以獨立實作（`url` crate）解析，帳號、密碼、路徑解碼後與輸入相同；原文密碼不在 URL；`mask_url` 後只剩 `***` | 通過 |
| D03 | `…::d03_password_file_plain_and_with_line_ends_gives_the_first_line` | 一般檔、結尾 `\n`、`\r\n`、多行（取第一行）、含空白與中文：密碼正確，沒有 warning | 通過 |
| D03 | `…::d03_password_file_empty_or_missing_is_an_error_naming_the_file` | 空檔、只有換行、只有 `\r\n`、不存在、是目錄：全部報 `[database] password_file` 並指出檔名（空檔含 `empty`，不存在含 `cannot read`） | 通過 |
| D03 | `…::d03_password_file_readable_by_others_warns_but_works` | 0600、0400 無 warning；0644、0640、0604 成功組出 URL，各有一則 warning（含 `chmod 600` 與實際權限，不含密碼） | 通過 |
| D03 | `…::d03_a_relative_password_file_is_relative_to_the_executable_directory` | 相對路徑以執行檔所在目錄為準 | 通過 |
| D04 | `…::d04_every_wrong_combination_is_refused_with_the_field_named` | 26 種錯誤組合：`url`＋`type`、`url`＋分欄位、沒有 `type` 的分欄位、`password`＋`password_file`、未知／空的 `type`、缺 `host`／`name`（含全空白）、mssql 以外的 `trust_server_certificate`（postgres、mysql、mariadb、`false` 也算）、sqlite 的 host／user／password／name／port／trust、空的 `path`、伺服器用 `path`、有密碼沒有 user、未知欄位、port 超出範圍、`max_connections = 0`；每個訊息都含欄位名稱與設定檔名稱 | 通過 |
| D04 | `…::d04_an_error_never_contains_the_password` | 三種錯誤的訊息都不含密碼 | 通過 |
| D05 | `apps/sip-faxd/tests/m4f_cli.rs::d05_the_password_of_the_fields_never_shows_in_any_output_or_log` | 密碼 `Aa1@:/?#%41&+ 中😀x` 分欄位寫在連不上的 PostgreSQL 設定：`config check`、`db check`、`db check --url --password-stdin`、`db copy --to-config`、`db copy --to --to-password-stdin`、真的 `serve`（stderr 與 log 檔，level debug）、`password_file` 的 `config check`／`db check`、錯誤的 `type` 時的 `config check`／`serve`／`db check`（退出碼 2，指出 `[database] type`）。共 14 份輸出，皆不含原文、百分比編碼、`+`／`%20` 變體或開頭 `Aa1@`；有顯示 URL 的輸出含 `faxu:***@127.0.0.1`。**變異驗證**：把 `db check` 的標頭改成不遮蔽，此案例失敗（已還原） | 通過 |
| D02 | `apps/sip-faxd/tests/m4f_server.rs::d02_k01_a_password_with_every_awkward_character_works_field_by_field_and_in_a_file` | 在伺服器上建立密碼含 `@ : / ? # % & + 空白 中` 與 emoji 的帳號與資料庫（`docker exec`）：分欄位的 `db check` 成功；多一個字元的密碼被拒（對照，確認真的驗證了帳號）；`password_file` 寫法成功；手寫百分比編碼的 `--url` 成功；`--url`（無密碼）＋`--password-stdin` 成功 | 通過（PostgreSQL、MySQL、MariaDB、MSSQL；**PostgreSQL 的密碼不含 emoji**，見 §3、§4） |
| D02 | `…::d02_postgres_refuses_an_emoji_in_the_password_and_the_hint_says_so` | PostgreSQL 的密碼含 emoji 時 `db check` 退出碼 1，提示 SASLprep 限制；其他三種資料庫略過（emoji 已在上一案例通過） | 通過（PostgreSQL） |
| K01 | `m4f_cli.rs::k01_db_check_on_sqlite_succeeds_and_reports_type_version_and_schema` | SQLite：退出碼 0、`(sqlite)`、`connected in`、版本號是數字、`schema version: N (this sip-faxd needs N): up to date`（兩個 N 相同）、結尾 `ok`、stderr 空；`--url` 不需設定檔；沒有產生新檔案 | 通過 |
| K01 | `m4f_server.rs`（同 D02 的案例，後半） | 四種伺服器：`db migrate` 之前 `no tables yet`、複製之後 `up to date` 且以 `ok` 結尾；伺服器版本以數字開頭；URL 顯示 `<編碼後的 user>:***@` | 通過（四種） |
| K01 | `crates/store/src/tests/check_tests.rs::k01_check_reports_the_server_and_a_current_schema` | `check_url`：種類等於後端、`schema == required == latest_version`、伺服器版本以數字開頭、耗時小於上限、`schema_migrations` 列數不變 | 通過（SQLite 檔；db-matrix 四種） |
| K02 | `m4f_cli.rs::k02_db_check_reports_empty_older_and_newer_schemas_and_changes_nothing` | SQLite：①檔案不存在：退出碼 0、`does not exist`，**沒有建立檔案**；②空檔：`no tables yet`＋`db migrate`，`schema_migrations` 不存在（`store::test_schema_versions` 回 `None`）；③較舊（從 `schema_migrations` 刪掉最新版）：退出碼 0、`older`、`db migrate`、版本號正確，**資料夾所有檔案逐位元組不變**，`schema_migrations` 內容不變；④較新（記錄 `latest+5`）：退出碼 1、`NEWER`、`newer sip-faxd`、不以 `ok` 結尾，檔案逐位元組不變 | 通過 |
| K02 | `m4f_server.rs::k02_on_a_server_empty_older_and_newer_schemas_are_reported_and_nothing_is_changed` | 四種伺服器：空（不建立 `schema_migrations`）→ `db migrate` → 較新（退出碼 1）→ 較舊；每一步之後 `schema_migrations` 的版本清單與檢查前相同 | 通過（四種） |
| K02 | `check_tests.rs::k02_check_reports_older_newer_and_empty_schemas_and_writes_nothing` | `check_url` 的同樣三種狀態；`DROP TABLE schema_migrations` 後檢查回 schema 0，且之後 `current_version` 仍是 0（沒有被建立） | 通過（SQLite 檔；db-matrix 四種） |
| K03 | `m4f_cli.rs::k03_{postgres,mysql,mariadb,mssql}_closed_port_…`（4 個）、`k03_{postgres,mysql,mssql}_unknown_host_…`（3 個） | 關閉的 port（含 mariadb）：退出碼 1、`cannot use the database`、提示 `nothing listens on that host and port`、不以 `ok` 結尾、密碼不外洩；`no-such-host.invalid`：提示 `the host name cannot be resolved`；每個都在 20 秒內回來（實測：關閉的 port 約 10 秒，MSSQL 約 11 秒，無法解析約 0～11 秒） | 通過 |
| K03 | `m4f_server.rs::k03_on_a_server_a_wrong_password_a_missing_database_and_an_untrusted_certificate_are_told_apart` | 四種伺服器：錯密碼（提示帳號或密碼錯誤，且兩個密碼都不外洩）、不存在的資料庫（提示資料庫不存在；MySQL／MariaDB 對不存在的資料庫回 access denied，接受兩種提示）、關閉的 port（改用真的帳號的設定）；**MSSQL**：未設 `trust_server_certificate` 時提示含 `trust_server_certificate`，設了以後同一個帳號成功。每個都 20 秒內回來 | 通過（四種） |
| K03 | `commands.rs::tests::k03_connection_hints_tell_the_causes_apart` | 各驅動的真實錯誤訊息（含 MSSQL 的 `Cannot open database … The login failed.`）對應到正確提示；沒見過的訊息沒有提示 | 通過 |
| K03 | `check_tests.rs::k03_a_missing_sqlite_file_is_not_created_and_bad_targets_are_errors` | `check_url`：缺少的 SQLite 檔回 `NotFound` 且沒有建立；非資料庫 URL 回 `BadUrl`；上限 30 ms 時回 `Unavailable`（`no answer within`）；錯誤訊息不含密碼 | 通過 |
| K04 | `m4f_cli.rs::k04_password_stdin_is_refused_when_the_url_has_a_password_or_no_user_or_stdin_is_empty` | URL 已有密碼（`already has a password`，沒有輸出任何連線）、沒有 user（`needs a user`）、stdin 空行（`empty`）、沒有 `--url` 的 `--password-stdin`（clap 拒絕）、SQLite URL 加 `--password-stdin`；對照：正確的 URL 走到連線這一步，URL 顯示 `u:***@`，錯誤不含密碼 | 通過 |
| K04 | `m4f_server.rs`（D02 案例的中段） | 四種伺服器：URL 無密碼、stdin 送 `Aa1@:/?#%41&+ 中…` 連線成功；URL 已有密碼時退出碼 1 | 通過（四種） |
| P01 | `m4f_cli.rs::p01_to_config_with_fields_copies_the_same_as_to_with_the_full_url` | SQLite 來源（執行設定＋兩個帳號，22 列）：`--to <完整 URL>` 與 `--to-config`（分欄位的另一個設定檔）各複製一份：每個表的列數相同、`config_sections` 逐列相同、帳號 2 個；輸出含 `-> sqlite://…`、`verified`；來源不變 | 通過 |
| P01 | `m4f_server.rs`（D02 案例的後半） | 四種伺服器：`db copy --to <無密碼 URL> --to-password-stdin`（密碼從 stdin）與 `db copy --to-config`（分欄位，另一個資料庫）；每個表的列數與來源相同；輸出不含密碼 | 通過（四種） |
| P02 | `m4f_cli.rs::p02_wrong_combinations_of_the_copy_options_are_refused_and_nothing_is_written` | `--to`＋`--to-config`、兩者都沒有、`--to-password-stdin` 單獨（clap，退出碼 2）；`--to-password-stdin`＋`--to-config`（程式拒絕，退出碼 1，**見 §3 ①**）；URL 已有密碼、SQLite URL 沒有 user、stdin 空；`--to-config` 檔案不存在、不是 TOML、`[database]` 錯誤（指出 `[database] host`）、等於來源（`the target is the source database`）。每一個之後目標檔案都不存在、來源列數不變 | 通過 |
| F01 | `m4f_cli.rs::f01_the_example_file_has_only_startup_settings_checks_and_starts_the_first_time` | `conf/sip-faxd.toml.example` 解析後的最上層表剛好是 `database`、`log`、`service`、`storage`、`web`；只改路徑與 Web port 後 `config check` 退出碼 0，輸出「none in this file, they live in the database」，沒有 `runtime sections in this file`；第一次 `serve` 成功（`sip-faxd ready`、log 有 no-trunk warning、Web 對 `/` 回 200、資料庫檔案建立、log 沒有 ERROR） | 通過 |
| F02 | `m4f_cli.rs::f02_a_new_installation_starts_without_a_trunk_refuses_calls_and_takes_a_trunk_without_a_restart` | 設定檔沒有任何執行設定區段、資料庫有 schema 但沒有執行設定：`config check` 說「the defaults, without any trunk」；`serve` 的 log 有 no-trunk warning（含 `Settings -> Trunks`）；資料庫寫入預設的 `sip` 區段、沒有任何 `trunk:*`；來自 127.0.0.1 的 INVITE（預設 `unknown_source = "drop"`）3 秒內沒有任何回應；以 `root` 登入 Web，`GET /status` 的中繼數 0；`PUT /config/trunk:gw` 回 200，`/status` 中繼數 1；**同一個行程**對同一個 INVITE 回 1xx/2xx；log 只有一次 `sip-faxd ready` | 通過 |
| F02 | `tools/fax-sim/tests/e2e_m4f.rs::f02_a_new_installation_refuses_calls_then_a_trunk_added_in_the_web_receives_a_real_fax_without_a_restart` | Docker＋SpanDSP：無中繼的 `sip-faxd`（`unknown_source = "reject"`）；SpanDSP 閘道來電被 403 拒絕，沒有任何傳真列；以 CLI 建立管理員、登入 Web、`PUT /config/trunk:gw`；同一個行程收下一份 2 頁精細傳真：`assert_success`、`verify_files`（libtiff 與 poppler 逐像素）、紀錄 `received`／2 頁／中繼 `gw`；`GET /faxes` 1 筆；log 只有一次 `sip-faxd ready`、沒有 ERROR | 通過（約 100 秒） |
| F03 | `m4f_cli.rs::f03_an_old_file_with_runtime_sections_is_imported_once_into_an_empty_database` | 0.3.0 風格的設定檔（`[sip]`、`[call]`、`[capture]`、`[[trunks]]`、`[[routes]]`）＋空資料庫：`config check` 列出五個區段與 `trunk gw`；`serve` log 有「was imported into the database」、沒有 no-trunk warning、沒有「can be removed」；檔案裡的中繼收下來電（1xx/2xx）；資料庫有 `sip`、`call`、`trunk:gw`、`routes`，稽核（扣掉帳號）至少 4 列且 `by = config-file`，`sip` 區段含檔案裡的 bind | 通過 |
| F04 | `m4f_cli.rs::f04_a_file_with_runtime_sections_is_not_imported_again_and_the_log_lists_what_can_go` | 第一次啟動匯入後，把檔案裡的 `station_id`、`allowed_sources` 改掉：`config check` 有「runtime sections of the configuration file are not used」、列出 `sip, call, trunks, routes`、`remove them from the file`，顯示的是資料庫的值（`0287654321`）；`serve` log 同一行列出可刪除的區段，沒有再次匯入；**資料庫所有設定區段（名稱、內容、版本）與稽核列數與啟動前完全相同** | 通過 |
| F05 | `web-ui/e2e/setup.spec.ts`（6 個 Playwright 案例） | admin 與 agent、繁中與英文各一個案例：admin 看到 `setup.no_trunk_admin` 與前往 設定 → 中繼 的連結（`href="#/settings/trunks"`，點擊後到該頁、橫幅仍在），沒有 agent 的文字；agent 看到 `setup.no_trunk_agent`、沒有連結、沒有 admin 的文字；對照：有中繼的實例 a 沒有橫幅；admin 在 UI 新增第一個中繼：儲存前橫幅仍在、資料庫沒有中繼，儲存後 **admin 與另一個瀏覽器的 agent 的橫幅都消失，兩頁都沒有重新載入**（以 `window` 上的標記確認），重新整理後仍無橫幅；再從 UI 刪除這個中繼，兩頁的橫幅又出現（同樣沒有重新載入）；`finally` 會把實例 c 還原為沒有中繼 | 通過 |

## 3. 發現並修正的產品問題

以下是測試發現的產品問題，都做了最小修正，並各有測試：

1. **`db copy --to-password-stdin --to-config FILE` 被靜默接受**（P02 發現）：`--to-password-stdin` 在 `--to-config` 時沒有意義，clap 的 `requires` 沒有擋下，程式照樣複製，stdin 的密碼被忽略。`commands::copy_target` 現在拒絕（退出碼 1，`--to-password-stdin goes with --to URL; a --to-config file has its own password`），目標不會被寫入。
2. **SQL Server 的「資料庫不存在」被提示成「帳號或密碼錯誤」**（K03 在 MSSQL 發現）：SQL Server 對不存在的資料庫回 `Cannot open database "x" requested by the login. The login failed.`，`connection_hint` 先比對到 `login failed`。現在先比對 `cannot open database`／`unknown database`（單元測試 `k03_connection_hints_tell_the_causes_apart` 用各驅動的真實訊息；MSSQL 實機案例通過）。
3. **PostgreSQL 的密碼含 emoji 連不上，且沒有說明**（D02 發現）：sqlx 的 SCRAM 客戶端依 RFC 4013 SASLprep 拒絕 emoji（`Failed to saslprep password: ProhibitedCharacter`），不論是分欄位或手寫 URL。這是函式庫的限制，**沒有修**（MySQL、MariaDB、MSSQL 的 emoji 密碼都通過）；改為新增提示（`connection_hint`：「PostgreSQL's password check (SCRAM, SASLprep) refuses a character of the password, such as an emoji…」）並以 `d02_postgres_refuses_an_emoji_in_the_password_and_the_hint_says_so` 驗證。計畫 D02 的 PostgreSQL 帳號因此用不含 emoji 的密碼（`Aa1@:/?#%41&+ 中x`，其餘字元都在）。

另外觀察到、**沒有修**的小問題（供審核者決定要不要處理）：
- `serve` 在「沒有任何執行設定區段」的新安裝，log 仍寫「the runtime configuration of the configuration file was imported into the database」（實際寫入的是預設值）；訊息略有誤導。
- `[database] port = 70000` 的錯誤是 serde 的 `[database]: invalid value: integer 70000, expected u16`，有區段名稱，但沒有欄位名稱 `port`（其他欄位錯誤都有）。
- `db check` 對「空的 SQLite 檔（0 位元組）」開啟時 sqlx 會把檔案切到 WAL 模式，檔案變成 4096 位元組的標頭頁。沒有建立任何資料表（`schema_migrations` 不存在，測試以此斷言），已有資料表的檔案則逐位元組不變。
- 連線被拒（關閉的 port）在 sqlx 的 PostgreSQL／MySQL 路徑上表現為 `pool timed out`，約 10 秒才回來（MSSQL 約 11 秒），所以提示是較籠統的「nothing listens on that host and port, a firewall drops it, or the host is wrong」；仍在 15 秒上限內。
- `unknown_source = "drop"`（預設）的來電在 `info` 等級沒有任何 log，所以 F02 本機案例只能以「沒有回應」斷言。

為了測試而動的程式（不改行為）：
- `crates/store/src/lib.rs`：新增兩個 `#[doc(hidden)]` 測試輔助 `test_schema_versions`（唯讀列出 `schema_migrations`，沒有表時回 `None`）與 `test_forget_schema_version`（刪掉一個版本，模擬較舊的資料庫），與既有的 `test_record_schema_version`、`test_row_counts` 同類；`migrate::exists_sql` 改為 `pub(crate)`。
- `tools/interop/src/lib.rs`：`FaxdOptions` 新增 `no_trunk`（不寫 `[[trunks]]`）與 `web_port`（HTTP Web）；預設值與原行為相同。
- `scripts/e2e-web-container.sh`／`scripts/e2e-web.sh`：新增實例 `c`（沒有中繼，SIP 5062、Web 8081，主機 port 18081，`E2E_NOTRUNK_PORT`）；`web-ui/e2e/support.ts` 新增 `Instance = 'c'`。
- `scripts/db-matrix.sh`、`.github/workflows/ci.yml`：新增 M4f 的 step（見 §1）。

## 4. 與計畫不同之處

- **D02 的 PostgreSQL 密碼不含 emoji**（§3 ③）；其餘三種含 emoji。
- **F02 本機案例**用完全沒有執行設定的設定檔（SIP 綁預設的 `0.0.0.0:5060`，所以同檔案的 `serve` 案例以一把鎖序列化；5060 被占用時測試會失敗並顯示原因）。**Docker 案例**需要知道 SIP port 與 capture 目錄，所以測試設定檔仍有 `[sip]`、`[media]`、`[call]`、`[capture]`，只是沒有任何 `[[trunks]]`；資料庫在啟動時沒有執行設定，匯入的結果沒有中繼。拒絕來電的方式：本機案例用預設的 `drop`（沒有回應），Docker 案例用 `unknown_source = "reject"`（403），因為 SpanDSP 閘道在 `drop` 時只會等到逾時。
- **F02 本機案例的「收傳真」**只驗證加了中繼後同一行程對 INVITE 回 1xx（不是真的傳真）；真的傳真由 Docker 案例涵蓋，符合分工。
- **F05**：橫幅在每一頁頂端（不只傳真列表）；測試在列表頁與設定頁都有看。原有 16 個 Playwright 案例沒有改，全部照樣通過。
- **K03 的 MySQL／MariaDB「資料庫不存在」**：這兩種伺服器對沒有權限的不存在資料庫回 access denied，所以接受「資料庫不存在」或「帳號或密碼錯誤」兩種提示（PostgreSQL、MSSQL 嚴格要求前者）。
- **K02 伺服器案例的順序**：空 → migrate → 較新 → 較舊（因為「較新」的測試輔助會先 migrate）。
- **P01 在伺服器**：四種資料庫都做（不只 PostgreSQL），因為已經有帳號與資料庫。
- **K03 的耗時門檻**用 20 秒（任務指示）；產品的上限是 15 秒。
- 本機 `m4f_cli` 的 `serve` 案例都設 `stop_timeout_secs = 0`，避免收下來電後 `SIGTERM` 等 300 秒。

## 5. 涵蓋率

`cargo llvm-cov -p store --fail-under-lines 80 --summary-only --ignore-filename-regex 'backend[\\/](mssql|mysql|postgres)\.rs'`

| 項目 | 數字 | 門檻 |
|---|---|---|
| `store` 全部（行） | **81.53%**（2983 行中未涵蓋 551） | ≥ 80% |
| `check.rs`（行） | 91.84%（98 行中未涵蓋 8；原本在 `store` 自己的測試裡是 0%，所以新增了 `check_tests.rs`） | — |
| `web-ui`（statements） | **91.65%** | ≥ 70% |

`web`、`fax-in`、`delivery` 與協定 crate 這次沒有改程式，沒有重跑涵蓋率。

## 6. 執行指令

```sh
cargo test -p sip-faxd --bin sip-faxd m4f_tests k03          # D01、D03、D04、K03 提示（單元）
cargo test -p sip-faxd --test m4f_cli                        # D05、K01–K04、P01、P02、F01–F04（約 1 分鐘）
cargo test -p store check_tests                              # K01–K03（store 層）
scripts/db-matrix.sh [postgres mysql mariadb mssql]          # store（含 check_tests）、R04、M4f 伺服器案例（m4f_server）
E2E_ONLY="--test e2e_m4f" scripts/e2e-docker.sh              # F02（真傳真，約 100 秒；含 tools/ 的 fmt、clippy、t4 oracle）
scripts/e2e-web.sh                                           # 22 個 Playwright 案例（原有 16 個＋F05 的 6 個，約 4 分鐘）
cd web-ui && npm run lint && npm run check && npm run coverage
cargo llvm-cov -p store --fail-under-lines 80 --summary-only --ignore-filename-regex 'backend[\\/](mssql|mysql|postgres)\.rs'
```

## 7. 本機驗證結果

| 指令 | 結果 |
|---|---|
| `cargo fmt --all --check`、`cargo clippy --workspace --all-targets -- -D warnings`、`cargo deny check` | 通過 |
| `T4_ORACLE=skip cargo test --workspace --all-targets --no-fail-fast` | 1102 通過、4 ignored、0 失敗 |
| `scripts/db-matrix.sh`（四種） | store、M4f 伺服器案例全過；MySQL 的 R04（既有案例，與 M4f 無關）四次執行中有兩次逾時失敗（`attempts never reached the expected state`），單獨重跑通過，疑似不穩定 |
| `E2E_ONLY="--test e2e_m4f" scripts/e2e-docker.sh`（含 tools/ fmt、clippy、t4 oracle） | 通過（F02 約 116 秒） |
| `scripts/e2e-web.sh` | 最後一次 22 個中 21 通過、**1 個失敗：`access.spec.ts` U18（`toBeFocused`）**；同一案例在前一次執行通過，F05 的 6 個案例都通過。尚未查原因（可能是不穩定） |
| `npm run lint`／`check`／`coverage` | 通過；statements 91.65% |
| `cargo llvm-cov -p store …` | 81.53% |

**沒有執行**：完整 `scripts/e2e-docker.sh`、`scripts/ci-local.sh`、musl 建置。

## 8. 已知限制

- PostgreSQL 的密碼不能含 emoji 等 SASLprep 禁止的字元（sqlx 限制，§3 ③）。
- 伺服器案例（D02、K02、K03 的伺服器部分、P01 的伺服器部分）需要 Docker 容器（`docker exec`）；在沒有容器的環境（例如直接連現有資料庫）會略過。
- MSSQL 在 Apple Silicon 上以 linux/amd64 模擬執行，M4f 伺服器案例約 40 秒，整個 MSSQL 的 db-matrix 約 4 分鐘。
- 關閉的 port 的耗時（約 10 秒）取決於 sqlx 的 `acquire_timeout`；環境有防火牆丟棄封包時則是 15 秒上限。
- F05 的橫幅在 Playwright 只在 Chromium 驗證。
- F02 的本機案例在主機的 UDP 5060 上執行；主機上有別的 SIP 軟體占用 5060 時會失敗。

## 9. Opus 審核

2026-10-23，**通過**。

**檢查內容**
- 計畫 §1 的 17 個案例都有對應的測試，位置與報告 §2 相同；沒有 `#[ignore]`。
- 沒有空斷言或過度 mock：CLI 與伺服器案例都執行真的 `sip-faxd`，連線真的資料庫（db-matrix 四種），F02 以 SpanDSP 真的送傳真。斷言檢查的是具體輸出（退出碼、提示文字、密碼不出現、資料庫內容逐列或逐位元組不變），不是「有回應」而已。D05 做過變異驗證。
- 與計畫不同之處（§4）都有理由，可以接受。PostgreSQL 的 emoji 密碼是 sqlx 的 SASLprep 限制，以提示加上專門案例處理是合理的。
- 涵蓋率：store 81.53%（門檻 80%），web-ui statements 91.65%（加上 U20、U21 後為 91.69%）。
- CI：`m4f-config` 的 run 39810797081（重跑 `e2e-web` 後）、39824400230 全綠。前者第一次失敗是 GitHub 安裝套件時 packages.microsoft.com 回 403，與程式無關。
- U18（`toBeFocused`）：GitHub CI 的 `e2e-web` 連續兩次通過，沒有重現。視為偶發，之後再失敗時記錄並修正。

**審核時修正**
- §3 觀察 ①：新安裝（設定檔沒有執行設定）時，log 原本寫「the runtime configuration of the configuration file was imported」。改為「the database had no runtime configuration: the defaults were written to it」（`runtime.rs::log_boot`），F02 本機案例加上斷言。
- `settings.rs::read_password_file` 在非 unix 平台的未使用變數警告（Windows 上 `clippy -D warnings` 會失敗）。
- 新增 `.gitattributes`（`eol=lf`）：在 Windows checkout 後，腳本是 CRLF，在 Docker 裡無法執行（`bash`）。
- `docs/database.md`、IT 手冊 §3.4：寫明 sip-faxd 需要自己專用的資料庫（TODO 的待辦）。

**§3 其他觀察的處理**
- ② `port = 70000` 的錯誤沒有欄位名稱：訊息仍指出 `[database]` 與原因（`expected u16`），可以接受，不修。
- ③ 0 位元組的 SQLite 檔被 `db check` 切成 WAL：沒有建立資料表，不影響之後的 `db migrate`，不修。
- ④ 關閉的 port 約 10 秒才回來：在 15 秒上限內，提示文字涵蓋可能原因，不修。
- ⑤ `unknown_source = "drop"` 在 info 等級沒有 log：與既有行為一致（避免掃描流量洗版），不修；IT 手冊 §3（來源 IP）已說明對接時改成 `log` 可看到被丟棄的來源。

**Windows 本機執行的注意事項**（不影響 CI）
- Hyper-V 保留了 18004–18103 等 TCP port，`scripts/e2e-web.sh` 預設的 18080 無法綁定，要設 `E2E_HTTP_PORT`、`E2E_HTTPS_PORT`、`E2E_NOTRUNK_PORT`（例如 28080／28443／28081）。
- 主機記憶體不足時，Linux 全 workspace 測試與 Web E2E 不要同時執行。
