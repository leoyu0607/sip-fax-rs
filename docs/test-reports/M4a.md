# M4a 測試報告：執行設定入 DB、帳號與 session、CLI

- 實作與執行：Sonnet（M4a）
- 計畫：`docs/test-plans/M4.md` §0、§1 K01–K13、§2 A01–A13、§7 M14、§8 R01–R05；規格：`docs/design/m4-web.md` §0、§1、§2、§7.4、§8、§9、§10；ADR-051
- 狀態：**待 Opus 審核**。在 worktree 分支 `worktree-agent-ab3102522e81d8901` 上 commit，沒有 push。沒有改 `docs/TODO.md`、`docs/ROADMAP.md`、`docs/DECISIONS.md`，也沒有碰 `crates/web`、`web-ui/`、M4d 的控制 socket／start／stop／log 輪替。
- 重要提醒：本分支從 `a5615a9`（M3f）建立，開始工作時先 fast-forward 到 `main` 的 `e45b246`（取得 M4 設計規格與測試計畫）。

## 1. 範圍與交付內容

| 項目 | 位置 |
|---|---|
| migration 0002（四種 DB）：`config_sections`、`config_audit`、`users`、`sessions`；M3 `settings` 的 `retention.*` 搬進 `retention` 區段並從 `settings` 刪除。SQL 單一實作（`AdminStore` trait，`FaxStore` 的 supertrait）；MySQL 的陳述式可重跑（建表 `IF NOT EXISTS`、資料搬移以 `NOT EXISTS` 保護）；沿用 M3b 的 migration 鎖 | `crates/store/migrations/*/0002_config_accounts.sql`、`crates/store/src/{admin,sqlstore_admin,migrate}.rs` |
| 設定的樂觀鎖（`version`）與稽核：`config_put`／`config_delete`／`config_audit_list`／`config_audit_add`；`StoreSource` trait（讓 daemon 的延遲連線 `Db` 與測試用的已開啟 store 共用） | 同上、`crates/store/src/store.rs` |
| 新 crate **`runtime-config`**：`RuntimeConfig`（八個區段）、逐欄位驗證（`FieldError{field, code, message}`）、秘密欄位加密（AES-256-GCM，`enc:v1:`）、`ConfigService`（載入、首次匯入、`update`、`delete_trunk`、`reorder_trunks`、`reload`、`import`、`export`、`inspect`、`status`、`pending_restart`）、TOML 解析／輸出 | `crates/runtime-config/` |
| 熱套用：fax-in 的 `Snapshot`（`ArcSwap`），每通來電在 INVITE 取一次；`FaxIn::apply_config`、`pending_restart`、`snapshot`；trunk 的 `register` 變更時先 un-REGISTER 再重新註冊；transport 的來源過濾器可替換（`sip-core` 的 `PolicyCell`）；投遞服務在路由或 `[delivery]` 變更時換掉引擎（`Reconfigurer`） | `crates/fax-in/src/{controller,config,session,t38session}.rs`、`crates/sip-core/src/transport/`、`apps/sip-faxd/src/{runtime,delivery_service}.rs` |
| 新 crate **`accounts`**：argon2id、密碼規則、登入鎖定與每 IP 限流（注入時鐘）、session 的建立／驗證／過期／撤銷、CSRF token、第一個管理員與 `initial-admin-password`、帳號管理（含「最後一個啟用中的 admin」保護與稽核）、`Role`／`Capability` 權限矩陣 | `crates/accounts/` |
| CLI：`user add/passwd/disable/enable/list`、`fax list/show/read/note/redeliver`、`config import [--force] [--file]`、`config export [--include-secrets]`、`config check` 一併檢查 DB（唯讀：不 migrate、不匯入、不建金鑰）；操作者記為 `cli:<作業系統帳號>` | `apps/sip-faxd/src/{admin_cli,commands,main}.rs` |
| `redeliver`：store 新增 `redeliver(fax_id, at)`（每個 target 的最新一筆是 `failed`／`giving_up` 才新增 `pending`，attempt + 1） | `crates/store/src/sqlstore_admin.rs` |
| M3 留下的項目：預覽與清整的競態（R01）、預覽快取容量上限（R02，`Preview::with_cache_limit`、`[storage] preview_cache_mb`）、投遞關閉超過寬限（R03）、執行中 DB 重啟（R04）、本地時區檔名（R05） | `crates/store/src/{preview,purge}.rs`、`apps/sip-faxd/src/delivery_service.rs` |
| 文件 | `conf/sip-faxd.toml.example`、`README.md`、`README.zh-TW.md`、`docs/database.md`、`docs/field-test.md` |
| 腳本 | `scripts/db-matrix.sh`：每種 DB 另跑 R04（`cargo test -p sip-faxd --bin sip-faxd r04`） |

## 2. 執行指令與結果

```sh
cargo fmt --all
cargo clippy --workspace --all-targets -- -D warnings          # 乾淨
T4_ORACLE=skip cargo test --workspace --all-targets            # 859 passed, 0 failed
cargo deny check                                               # advisories ok, bans ok, licenses ok, sources ok
NAME_PREFIX=m4a PORT_OFFSET=400 scripts/db-matrix.sh           # postgres、mysql、mariadb、mssql：store 套件 58 個案例＋R04，全部 PASSED
SIPFAX_ORACLE_DOCKER=1 cargo test -p accounts a02_openssl      # OpenSSL 3.5 的 ARGON2ID 與我們的雜湊一致
E2E_NO_BUILD=1 E2E_IMAGE=sip-fax-rs-test:rocky9-m3e scripts/e2e-docker.sh   # §6
E2E_NO_BUILD=1 scripts/musl-docker.sh                                       # §6
cargo llvm-cov -p <crate> --summary-only --ignore-filename-regex 'backend[\\/](mssql|mysql|postgres)\.rs'
```

### 2.1 涵蓋率（行）
| crate | 行涵蓋率 | 門檻 |
|---|---|---|
| store（排除三個伺服器驅動，ADR-045） | 90.78% | ≥ 80 |
| fax-in | 93.87% | ≥ 70 |
| accounts（新） | 93.64% | ≥ 80 |
| runtime-config（新） | 90.18%（補 `tests/validate.rs` 之後；補之前 79.43%） | ≥ 80 |
| sip-faxd 的新模組 | `admin_cli.rs` 88.5%、`settings.rs` 97.4%、`delivery_service.rs` 81.4%；`runtime.rs` 在補上 daemon 層級測試前量到 6.7%（`serve` 子行程被 kill，不寫涵蓋率），補上 `runtime::tests` 後未重量 | ≥ 80（見 §7 未解事項） |

## 3. 案例結果

「單元／整合」= 在 SQLite 檔案上跑；「四種 DB」= store 層案例由 `db_case!` 在 PostgreSQL 16、MySQL 8.0、MariaDB 11、MSSQL 2022 全部跑過（db-matrix 全綠）。`accounts`／`runtime-config` 的邏輯測試只在 SQLite 上跑（SQL 都在 store，由四種 DB 的 store 案例涵蓋）。

### 3.1 執行設定（K）
| ID | 結果 | 測試 |
|---|---|---|
| K01 | 通過 | `runtime-config/tests/config.rs::k01_first_start_imports_the_whole_file_…`（逐欄位相等、trunk 與路由順序、稽核 `by = config-file`）；`sip-faxd/tests/m4a_cli.rs::k01_k02_a01_serve_…`（真實 `serve`） |
| K02 | 通過 | `k02_the_second_start_keeps_the_database_…`；`serve` 版本檢查 log 的 warning 列出 `sip`、`call`、`trunks`、`routes`，沒有第二次匯入、DB 值與 version 不變 |
| K03 | 通過 | `k03_import_without_force_refuses_…`；CLI 版 `k03_k04_config_import_export_round_trip_on_the_command_line`（無 `--force` exit 1 且 DB 不變；`--force` 覆蓋、刪除多餘 trunk、稽核 `cli:tester`） |
| K04 | 通過 | `k04_export_then_import_…`、CLI 版（export → import --force 到空 DB → 再 export，文字完全相同；預設不含秘密，`--include-secrets` 才含）。不含秘密的匯出若有 `register.enabled` 的 trunk，重新匯入會因缺密碼被拒絕（見 §5） |
| K05 | 通過 | `k05_invalid_sections_…`（12 種錯誤的 field／code、DB／記憶體／稽核／applier 都沒動）、`k05_a_change_that_breaks_another_section_…`、`runtime-config/tests/validate.rs`（7 個測試，每個區段與 trunk 每個欄位的代碼） |
| K06 | 通過 | store：`k06_config_put_is_an_optimistic_lock`（四種 DB，兩個寫入者搶同一 version 只有一個成功）；service：`k06_two_administrators_…`（輸家拿到目前 version） |
| K07 | 通過 | fax-in `hot_apply.rs::k07_a_change_applies_to_the_next_call_and_not_to_the_call_in_progress`（進行中的來電在舊的 30 秒上限到 BYE，新來電用新的 5 秒上限與新的 ANI 順序；虛擬時間精確到毫秒）、`k07_the_source_filter_follows_the_trunks`、`k07_an_invalid_configuration_is_refused_…`；daemon 層 `runtime::tests::k07_changes_through_the_service_reach_fax_in_and_delivery`；投遞 `delivery_service::tests::k07_new_routes_apply_…`；`k07_reload_picks_up_…` |
| K08 | 通過 | `k08_settings_that_need_a_restart_…`（`sip.bind`、`media.port_range` 標示 `restart_required`、`pending_restart` 列出、新的 service 啟動後消失）、fax-in `k08_settings_fixed_at_start_…`、daemon 層同一組 |
| K09 | 通過 | `hot_apply.rs::k09_changing_the_registration_…`（真實 UDP 的假 registrar：未變更不送任何 REGISTER；換帳號時先收到舊帳號的 `Expires: 0`，再收到新帳號的 REGISTER；關閉註冊時 un-REGISTER 且 `registrations()` 為空） |
| K10 | 通過 | `k10_secrets_are_encrypted_in_the_database_and_never_shown`（DB 內為 `enc:v1:`；測試內以 `ring` 直接解密，不經 `SecretBox`；密文搬到別的欄位解不開；API 視圖只有 `{set, ref}`；稽核是 `***`；所有 `config_sections`／`config_audit` 列都搜過明文）、`k10_keeping_a_secret_with_the_set_marker_survives_reordering` |
| K11 | 通過 | `k11_a_lost_key_costs_the_secrets_and_nothing_else`（金鑰遺失：新金鑰建立、其他區段完整、三個秘密列為待重填、重新輸入後恢復、大小錯誤的檔案移到 `.bad-<時間>`、完全無金鑰時改秘密被拒絕但改其他區段正常）；真實 `serve`：`k11_serve_starts_without_secret_key_…` |
| K12 | 通過 | `k12_without_the_database_…`（`update`／`get_section`／`reload` 回 Unavailable，快照不變，恢復後可修改）、`k12_a_start_without_the_database_runs_on_the_file_…`（檔案設定當快照，`retry_bootstrap` 之後匯入） |
| K13 | 通過（四種 DB） | `admin_tests::k13_retention_settings_move_into_the_section`、`k13b_partial_and_empty_settings`（在 v1 的 DB 塞值再 migrate；缺 key 取預設；沒有任何 retention 設定就不建區段；重跑搬移陳述式不覆蓋） |

### 3.2 帳號（A）
| ID | 結果 | 測試 |
|---|---|---|
| A01 | 通過 | `accounts::a01_…`（檔案 0600（unix 才斷言）、16 字元、log 擷取不含密碼、第一次登入 `must_change_password`、改密碼前檔案仍在、改完刪檔、舊密碼失效）；真實 `serve` 版在 `m4a_cli.rs::k01_k02_a01_…` |
| A02 | 通過 | `a02_the_hash_is_argon2id_…`（`$argon2id$v=19$m=19456,t=2,p=1$`）、`a02_an_independent_implementation_…`（`rust-argon2` 互驗兩個方向）、`a02_openssl_argon2id_agrees_…`（Docker 內 OpenSSL 3.5 `kdf ARGON2ID` 算出同樣的雜湊，另有不同密碼的反向對照；以 `SIPFAX_ORACLE_DOCKER=1` 啟用，本機已跑過）。**偏離**：Rocky 映像沒有 `argon2-cffi` 也不能 pip，改用 OpenSSL 參考實作與 `rust-argon2` |
| A03 | 通過 | `a03_password_rules_…`（太短、與帳號相同（含大小寫）、太長、10 個多位元組字元剛好通過、不合法帳號名；失敗時沒有建立任何帳號） |
| A04 | 通過 | `a04_five_failures_lock_…`（第 6 次密碼正確仍被拒、沒有開 session、到期前 1 秒仍鎖、後 1 秒可登入、成功後計數歸零）、`a04_lockout_does_not_depend_on_the_account_existing` |
| A05 | 通過 | `a05_unknown_account_and_wrong_password_look_the_same`（不存在、密碼錯、已停用：錯誤值、代碼、訊息完全相同）、`a05_response_times_…`（各 50 次取中位數，差 < 20%，兩條路徑都真的算 argon2） |
| A06 | 通過 | `a06_an_address_may_try_twenty_times_a_minute`（第 21 次 `rate_limited`；另一個 IP 不受影響；61 秒後恢復）。HTTP 429 的轉換屬於 M4b |
| A07 | 通過 | `a07_idle_timeout_…`、`a07_absolute_limit_…`（邊界前後 1 秒；每 6 小時使用一次仍在 7 天整斷）、`a07_tokens_are_random_and_only_their_hash_is_stored` |
| A08 | 部分 | `a08_logout_disable_and_password_reset_…`：session 都失效、不能重新登入、稽核有操作者。**WebSocket 連線關閉屬於 M4b**；`Authenticated.session_id` 與 `AuthService` 已提供所需資訊 |
| A09、A10 | M4b | cookie 屬性與 CSRF 檢查在 HTTP 層；M4a 提供 `COOKIE_NAME`、`CSRF_HEADER`、`csrf_for`、`AuthService::csrf_ok`（`a07_tokens_…` 測了正反向） |
| A11 | 通過（矩陣）／M4b（HTTP） | `a11_the_permission_matrix_…`（表格驅動，規格 §2.1 每一列 × 兩種角色，並檢查每個 `Capability` 都在表內）、`a11_require_refuses_…` |
| A12 | 通過 | `a12_the_last_enabled_admin_…`（停用、降級、刪除都回 `last_admin`，列完全沒變；有第二個 admin 後可以；停用的 admin 不算）。HTTP 409 的轉換屬於 M4b |
| A13 | 通過 | `m4a_cli.rs::a13_user_commands_…`（真實執行檔：`--password-stdin`（含 CRLF）、預設角色、重複／弱密碼／壞名稱／壞角色都 exit 1 且不建立、list、disable 結束 session、最後一個 admin 受保護、passwd、稽核 `cli:tester`） |

### 3.3 CLI（M）與 M3 留下的項目（R）
| ID | 結果 | 測試 |
|---|---|---|
| M14 | 通過 | `m4a_cli.rs::m14_fax_commands_…`（list 篩選與 limit、show 含投遞、read／unread 記 `cli:tester`、note 設定／取代／清除／超長被拒且不動、redeliver：只重排 failed／giving_up，done 與 pending 不動，再問一次沒事，未知 id exit 1）；store 層 `admin_tests::m14_redeliver`（四種 DB） |
| R01 | 通過（四種 DB） | `preview_tests::r01_purge_during_conversion_leaves_no_png`（用 fault hook 讓轉檔停在 `.part` 寫完之後，此時完整跑一次清整，再放行：請求回 `Purged`、沒有孤兒 PNG；反向：沒有清整時同樣的請求成功且 PNG 留著）。修正內容：轉檔後再確認一次紀錄；轉檔因目錄被刪而出 I/O 錯誤時也改判 `Purged`；清整標記後再掃一次快取目錄 |
| R02 | 通過（四種 DB） | `preview_tests::r02_cache_limit_evicts_least_recently_used`（依最後使用時間淘汰；快取命中會更新時間；60 秒保護窗內的檔案不刪；上限是軟性的；空目錄一併刪除；被淘汰的頁面再要時重新轉檔）。時間以注入的時鐘與 `set_modified` 控制 |
| R03 | 通過 | `delivery_service::tests::r03_an_attempt_beyond_the_grace_…`（webhook 伺服器收到請求後不回應；`shutdown` 在寬限 300 ms 後放棄並 abort；該列仍是 `pending` attempt 1；下一次啟動同位址的伺服器回 200：恰好一列 `done`、第二個伺服器收到恰好一次） |
| R04 | 通過（四種 DB） | `delivery_service::tests::r04_a_database_restart_loses_and_duplicates_nothing`：`docker restart` 資料庫容器的同時收 6 份傳真；全部記錄下來、每份恰好一個 `done`、資料夾裡恰好 2 × 6 個檔案。由 `scripts/db-matrix.sh` 對 postgres／mysql／mariadb／mssql 各跑一次；沒有設定環境變數時直接略過並印出原因 |
| R05 | 通過（注入偏移） | `r05_file_names_use_the_local_time_of_the_server`（`ServiceOptions.utc_offset_secs = +8h`：檔名是本地時間，且與 UTC 版本不同）。**沒有**在 `TZ=Asia/Taipei` 的容器裡以真實時區跑過（見 §7） |

## 4. 失敗與根因（過程中）
| 症狀 | 根因 | 處理 |
|---|---|---|
| MySQL 重跑測試 `mysql_migrations_are_safe_to_run_again` 失敗 | 0002 含資料搬移的 `INSERT`／`DELETE`，原測試只允許 `CREATE TABLE IF NOT EXISTS` | 測試改成也允許「以 `NOT EXISTS` 保護的 INSERT」與「刪 `retention.%` 的 DELETE」 |
| migration 檔註解裡的 `;` 讓 `split_statements` 多切出一個壞陳述式 | 既有限制（M3a：檔案內不可有 `;`） | 改註解 |
| `s01d` 在 v2 之後失敗（版本斷言、SQLite 整個 session 一起 rollback） | 測試假設 DB 在 v1 | 改成在真實 migration 之後加 v3 的 probe |
| K11：`routes` 區段整個載入失敗 | 解不開的秘密被換成空字串，`Routes::validate` 拒絕空密碼，一個遺失的 HMAC key 就讓所有投遞路由消失 | 改成非空的佔位字串 `***undecryptable-secret***`（對方會因認證失敗拒絕），匯出時略過佔位字串 |
| K07（reload）：`watch::Sender::send` 在沒有訂閱者時不更新值 | tokio 的行為 | 全部改 `send_replace` |
| K07：`BYE` 計數 6 | 沒回 200 的 BYE 會重送 | 測試改看每個 call-id 第一個 BYE 的時間 |
| `config check` 在新資料庫上若 `auto_migrate` 會順手建表 | 沿用 `Db::store()` | 檢查改用 `auto_migrate = false` 的專用連線 |
| 工具問題：用錯拼法的路徑（`開發用` vs `開発用`）寫檔，檔案落在另一個目錄（複製了一個 sip-fax-rs 空殼樹） | 兩個不同的使用者目錄並存 | 以 junction `C:\w4a` 指到 worktree 後統一使用；已把誤寫的 4 個檔案移回、刪除（該目錄樹現在沒有檔案） |

## 5. 對規格的偏離與擬議 ADR

偏離：
1. **`[capture]` 整段需要重啟**（規格 §1.3 說立即）：capture 寫入器與 transport 的 tap 在啟動時建立，其 `enabled/dir/keep/max_bytes/udptl` 固定。介面會把它列在 `pending_restart`。其餘 `[call]` 欄位中 `fax_session`、`reinvite_timeout_secs`、`max_491_retries` 也需要重啟（端點設定）；`send_ringing`、`answer_delay_ms`、`max_duration_secs`、`station_id` 立即。
2. **新增 `trunk_order` 區段**（JSON 名稱陣列）：規格只有 `trunk:<name>`，但 trunk 的比對順序是設定的一部分（K01 要求順序相等）。
3. **API 的秘密欄位為 `{"set": true, "ref": "<路徑>"}`**（規格只說「已設定／未設定」）：`ref` 讓重新排序路由／投遞時秘密跟著項目走，不會互換。
4. **解不開的秘密以佔位字串代替**（見 §4），而不是讓整個區段失效。
5. **A02 的獨立對照**改用 OpenSSL 3.5 與 `rust-argon2`（原計畫的 `argon2-cffi` 無法安裝）。
6. 稽核列與修改是兩個陳述式（DB 抽象沒有一般交易）：當機時可能少一列稽核，不會少修改。
7. 登入鎖定與 IP 限流的狀態在記憶體：重啟歸零、多個實例不共用。
8. 投遞：`DeliveryService` 現在永遠啟動（路由可以事後新增），沒有路由時引擎空轉；`shutdown` 超過寬限時 abort 工作而不是丟下不管（R03）。

擬議 ADR（請 Opus 合併）：
- **ADR-052：M4a 新增依賴**。`arc-swap`（設定快照）、`argon2` 0.5（RustCrypto，純 Rust）、`ring` 0.17（AES-256-GCM、SHA-256、隨機數；本來就在依賴樹，現在直接依賴）、`base64`、`serde_path_to_error`（驗證錯誤帶欄位路徑）；僅測試用 `rust-argon2`（獨立實作）。皆純 Rust 或 ring，`cargo deny check` 通過。`profile.dev` 對 argon2／blake2 設 `opt-level = 3`，否則除錯建置的雜湊要好幾秒。
- **ADR-053：執行設定的儲存與熱套用細節**。上面偏離 1–4：`trunk_order`、`{"set","ref"}` 秘密標記、AAD = `<區段>/<欄位路徑>` 的 AES-GCM、佔位字串、`[capture]` 與端點參數需重啟、快照在 INVITE 取一次（含 `station_id`、`max_duration`）、transport 來源過濾器可替換、投遞引擎在路由變更時整個換掉。
- **ADR-054：預覽快取的競態與上限**。轉檔後重驗、清整標記後再掃快取、LRU（以檔案修改時間為最後使用時間）、60 秒保護窗、軟性上限。

## 6. E2E 與 musl
- `scripts/e2e-docker.sh`（`E2E_NO_BUILD=1`、`E2E_IMAGE=sip-fax-rs-test:rocky9-m3e`）：E2E_RESULT_PLACEHOLDER
- `scripts/musl-docker.sh`（`E2E_NO_BUILD=1`）：MUSL_RESULT_PLACEHOLDER

## 7. 未解事項（誠實清單）
1. **Linux 專屬斷言沒有在 Linux 上跑過**：`conf/secret.key`、`initial-admin-password` 的 0600 權限（`#[cfg(unix)]`）只在 Windows 主機上編譯，沒有執行。需要在 Docker 內跑 `cargo test -p accounts -p runtime-config -p sip-faxd`（映像連不上網路取得 crates，這次沒做）。
2. **R05** 只驗證了注入的 +8 小時偏移；沒有在 `TZ=Asia/Taipei` 的容器裡驗證 `Local::now()` 的偏移讀取（程式碼只有一行 `Local::now().offset()`）。偏移在引擎建立時讀一次，之後夏令時間變動不會更新（台灣沒有夏令時間）。
3. **A08 的 WebSocket、A09、A10、A11 的 HTTP 部分、A06 的 429、A12 的 409** 要等 M4b；M4a 只提供底層與代碼。
4. **`sip-faxd/src/runtime.rs` 涵蓋率**沒有在補上 `runtime::tests` 後重量（之前量到 6.7% 是因為被 kill 的 `serve` 子行程不寫涵蓋率）。該測試本身通過，覆蓋 `Runtime::start`、`LiveApplier`、`attach/detach`；啟動時 DB 不可達的重試迴圈（每 10 秒）沒有自動化測試（`k12_a_start_without_the_database…` 在 service 層驗證了 `retry_bootstrap`）。
5. **`config import` 之後執行中的服務要重啟或等 M4d 的 `reload`** 才載入；M4a 提供 `ConfigService::reload()`，沒有接任何觸發點。
6. 互動式密碼輸入（無 `--password-stdin`、關閉回顯）沒有測試；只測了 stdin 路徑。Windows 上不會關閉回顯，也沒有 0600。
7. Webhook URL 本身（含 userinfo 或 query 的 token）沒有加密，只在顯示時遮蔽（`mask_http_url`）；只有 `password`／`secret` 欄位加密。
8. 預覽快取上限只做在 `Preview`（含 `[storage] preview_cache_mb` 的解析）；`serve` 目前沒有使用 `Preview`（M4b 會），所以還沒有任何地方呼叫 `with_cache_limit_mb`。
9. 不含秘密的匯出（預設）無法直接重新匯入有註冊的 trunk（缺密碼會被驗證擋下），需要先補 `password`。這是刻意的（不匯入半套設定），但「當備份還原」不方便。
10. K09 的 registrar 不做 digest 驗證，所以沒有驗證「新密碼真的被用來認證」，只驗證了帳號（AoR）、順序與 `Expires`。
11. SpanDSP 的 E2E（真實傳真通話中改設定）沒有做；K07 在 controller（虛擬時間）與 daemon（真實 socket、無通話）兩層驗證。
12. `main.rs` 的改動集中在 `mod` 宣告、`Command`／`ConfigAction` 的新分支與 `run()` 的啟動／關閉段落，與 M4d 合併時預期在這幾處衝突。

## 8. 給 M4b／M4d 的 API 說明

### 8.1 設定：`runtime_config::ConfigService`（`Runtime.config`，`Arc`）
```rust
// 讀
fn current(&self) -> Arc<RuntimeConfig>;                    // 新來電使用的快照
fn subscribe(&self) -> watch::Receiver<Arc<RuntimeConfig>>;
async fn get_section(&self, name: &str) -> Result<SectionView, UpdateError>;   // {name, version, body}；秘密為 {"set":true,"ref":..}；version 0 = 從未儲存（body 為預設值）
async fn list_sections(&self) -> Result<Vec<SectionView>, UpdateError>;       // 固定區段在前，trunk 依比對順序
async fn audit(&self, limit: u32, before_id: Option<i64>) -> Result<Vec<ConfigAuditRow>, UpdateError>;
fn status(&self) -> ConfigStatus;       // source(Database|FileFallback)、金鑰狀態、解不開的秘密、versions、pending_restart
fn pending_restart(&self) -> Vec<String>;   // 例如 ["sip.bind","media.port_range"]；重啟後（新的 mark_started）消失
// 寫（M4b 的 PUT /config/{section}、DELETE /config/trunk:{name}；M4d 的 reload）
async fn update(&self, section: &str, body: serde_json::Value, expected_version: i32, by: &str)
    -> Result<UpdateOutcome, UpdateError>;
async fn delete_trunk(&self, name: &str, expected_version: i32, by: &str) -> Result<UpdateOutcome, UpdateError>;
async fn reorder_trunks(&self, order: Vec<String>, by: &str) -> Result<UpdateOutcome, UpdateError>;
async fn reload(&self) -> Result<ReloadOutcome, UpdateError>;   // 重新讀 DB，有差異才套用（控制 socket 的 reload）
```
- `section`：`sip`、`media`、`call`、`capture`、`delivery`、`retention`、`routes`、`trunk:<名稱>`。`expected_version`：讀到的 version；新 trunk 或從未儲存的區段用 `0`。
- `body` 的秘密欄位：字串 = 新值；`{"set":true,"ref":"…"}` = 保留原值（原樣送回即可）；`null`／`{"set":false}` = 清除。
- `UpdateOutcome { section, version, restart_required, pending_restart, warnings, applied: ApplyReport }`；`restart_required` 是本區段需要重啟的欄位（M4b 回應的「需要重啟」旗標），`pending_restart` 是全部。
- `UpdateError` → HTTP：`Invalid(Vec<FieldError>)` → 422（`FieldError{field, code, message}`，`field` 為從區段根算起的路徑如 `t38.max_version`、`routes[1].deliveries[0].url`，空字串 = 整個區段；代碼：`required`、`invalid`、`wrong_type`、`unknown_field`、`out_of_range`、`empty`、`too_long`、`duplicate`、`not_last`、`reversed`、`unknown_preset`、`mismatch`、`removed`、`secret_key`）；`Conflict{current}` → 409（`current` 是 DB 目前的 version）；`NotFound` → 404；`UnknownSection` → 404；`Unavailable` → 503（DB 連不上，記憶體快照不變）；`Internal` → 500。
- 套用：`set_applier(Arc<dyn ConfigApplier>)`；daemon 的實作是 `sip-faxd/src/runtime.rs` 的 `LiveApplier`（呼叫 `FaxIn::apply_config` 與 `Reconfigurer::reconfigure`）。`FaxIn` 另有 `snapshot()`、`pending_restart()`、`registrations()`（M4b `/status`）。
- CLI／備份：`import(&RuntimeConfig, force, by)`、`export(include_secrets) -> String`（TOML）、`inspect()`。
- 啟動：`bootstrap()`（DB 沒有執行設定就匯入檔案的；DB 連不上就用檔案設定，`retry_bootstrap()` 恢復後套用）。`Runtime::start(&AppConfig, Arc<Db>)` 把這些與帳號、金鑰一起做好。

### 8.2 帳號：`accounts::AuthService`（`Runtime.auth`，`Arc`）
```rust
async fn login(&self, username, password, ip: &str, user_agent: Option<&str>) -> Result<LoginOk, LoginError>;
//   LoginOk { token (cookie `sipfax_session` 的值), csrf (X-Sipfax-Csrf 的值), user: UserInfo, expires_at }
//   LoginError::code(): invalid_credentials(401) | account_locked(423/429) | rate_limited(429, retry_after_secs) | unavailable(503)
async fn authenticate(&self, token: &str) -> Result<Authenticated, SessionError>;   // 每個請求一次；會更新 last_seen
//   SessionError::code(): unauthenticated | session_expired | account_disabled | unavailable
fn Authenticated::require(&self, cap: Capability) -> Result<(), Forbidden>;         // 403 `forbidden`
fn AuthService::csrf_ok(session_token: &str, header: Option<&str>) -> bool;         // 改變狀態的請求都要檢查
async fn logout(&self, token: &str);  async fn change_password(&self, user_id, old, new, ip, ua) -> Result<LoginOk, AccountError>;  // 舊 session 全部失效並回傳新的
async fn set_password(&self, username, new, must_change, by);  create_user(&NewAccount, by);  update_user(username, role, disabled, display_name, locale, by);  delete_user(username, by);  list_users();  get_user();
async fn ensure_initial_admin(&self, file: PathBuf);  async fn purge_expired_sessions(&self);
```
- `AccountError::code()`：`password_too_short`、`password_too_long`、`password_same_as_username`、`wrong_password`、`bad_username`、`bad_role`、`username_taken`、`not_found`、`last_admin`（409）、`unavailable`。
- `Capability`：`ViewFaxes`、`EditFaxMeta`、`ViewDeliveries`、`Redeliver`、`ViewConfig`、`EditConfig`、`ManageUsers`、`ViewAudit`、`ViewStatus`；`Role::can(cap)`。M4b 每個路由對應一個 capability。
- session 預設：閒置 8 小時、絕對 7 天（`AuthConfig`；`[web] session_idle_hours` 由 M4b 傳入）。帳號停用／刪除／重設密碼已經刪掉該使用者全部 session；WebSocket 要關連線時用 `Authenticated.session_id` 與使用者 id 自行追蹤。
- 稽核：帳號操作寫入 `config_audit`（`section = "user:<帳號>"`）。

### 8.3 其他 store API（M4b）
`FaxStore::redeliver(fax_id, at)`（`POST /faxes/{id}/redeliver`）、`Preview::with_cache_limit_mb(cfg.storage.preview_cache_mb)`／`with_clock`／`trim_cache()`、`Error::Purged` → 410；`StoreSource` trait（`Db` 實作）。

### 8.4 給 M4d
- `Runtime`（`apps/sip-faxd/src/runtime.rs`）持有 `config`、`auth`，`attach(fax, reconfigurer)`／`detach()`；`serve` 關閉時先 `detach()` 再 `Arc::try_unwrap` 取回 `FaxIn` 呼叫 `shutdown()`。控制 socket 的 `reload` 直接呼叫 `config.reload()`；`status` 的待重啟清單用 `config.pending_restart()` 或 `fax.pending_restart()`。
- `FaxIn` 現在以 `Arc<FaxIn>` 在 `run()` 裡共用；`DeliveryService::start_default` 不再是 `Option`。M4d 的 `stop` 流程要沿用這兩點。

## 9. 異動的檔案
- 新增 crate：`crates/runtime-config/`、`crates/accounts/`。
- store：`migrations/*/0002_config_accounts.sql`、`src/{admin,sqlstore_admin,store,lib,migrate,sqlstore,preview,purge}.rs`、`src/tests/{admin_tests,mod,store_tests,unit_tests,preview_tests}.rs`、`Cargo.toml`。
- fax-in：`src/{controller,config,session,t38session}.rs`、`tests/{hot_apply,t38session}.rs`、`Cargo.toml`。
- sip-core：`src/transport/{mod,policy,tcp,udp}.rs`、`tests/policy_cell.rs`。
- sip-faxd：`src/{main,settings,db,commands,delivery_service,admin_cli,runtime}.rs`、`tests/{db_cli,m4a_cli}.rs`、`Cargo.toml`。
- 根：`Cargo.toml`（workspace 依賴、`profile.dev`）、`Cargo.lock`、`scripts/db-matrix.sh`、`conf/sip-faxd.toml.example`、`README.md`、`README.zh-TW.md`、`docs/database.md`、`docs/field-test.md`、本報告。
