# M3c 測試報告：delivery crate（folder／email／webhook 投遞與重試）

- 實作與執行：Sonnet（M3c）
- 計畫：`docs/test-plans/M3.md` §0、§6 D01–D06；規格：`docs/design/m3-store.md` §0、§1.2、§9、§11
- 狀態：**待 Opus 審核**。已在 worktree 分支 commit，沒有 push。
- 沒有動 `crates/fax-in`、`apps/sip-faxd`、`crates/t4`、`docs/TODO.md`、`docs/DECISIONS.md`、`docs/ROADMAP.md`。對 `crates/store` 只加了兩個 delivery_attempts 的 API（§1.3）。

## 1. 交付內容
| 項目 | 位置 |
|---|---|
| `Routes`／`Route`／`DeliveryConfig`（`Folder`／`Email`／`Webhook`）：serde 反序列化 `[[routes]]`、`[[routes.deliveries]]`，`deny_unknown_fields`，`Routes::validate() -> Result<(), Vec<String>>`（供 `config check`，回報所有問題） | `crates/delivery/src/config.rs` |
| `Router::find(&FaxRecord) -> Option<&Route>`（`match` 是保留字，故名為 `find`） | `router.rs` |
| `DeliveryEngine`：`dispatch(fax_id)`、`run_due()`、`catch_up(since)`、`spawn() -> DeliveryHandle`（`notify_finalized(id)`、`notifier()`、`shutdown()`）；時鐘用 `store::Clock` 注入 | `engine.rs` |
| folder（原子寫入、範本、淨化、冪等） | `folder.rs`、`template.rs`、`source.rs` |
| email（lettre＋rustls；STARTTLS／SMTPS／明文；AUTH PLAIN/LOGIN；PDF（可加 TIFF）附件） | `email.rs` |
| webhook（reqwest＋rustls；JSON 或 multipart；HMAC-SHA256 header） | `webhook.rs` |
| 退避表 1、5、15、60 分鐘、之後每 4 小時；`max_attempts` 預設 10 | `retry.rs` |
| sip-faxd 接線範例（`lib.rs` 文件註解）與 §9 說明 | `crates/delivery/src/lib.rs` |
| store 新 API：`due_deliveries(now, limit)`、`update_delivery(id, status, last_error, next_try_at)`；`s13_delivery_queue` 行為測試 | `crates/store/src/{store,sqlstore}.rs`、`tests/store_tests.rs` |
| 本機 SMTP 測試容器腳本 | `scripts/smtp-test-servers.sh` |
| CI：`test`、`coverage` job 加 MailHog＋2 個 smtp4dev service；coverage 加 `delivery` ≥ 80% | `.github/workflows/ci.yml`、`scripts/ci-local.sh` |

### 1.1 投遞模型（與規格 §1.2 的對應）
一個「傳真 × 投遞目標」的每一次嘗試是 `delivery_attempts` 的一列（表就是完整歷史）：
- `dispatch` 為路由中每個 delivery 插入一列 `pending`（`attempt = 1`、`next_try_at = now`）。對同一個 `(fax, target)` 冪等。
- `run_due` 取 `status = pending 且 next_try_at <= now` 的列：
  - 成功：該列 → `done`。
  - 暫時性失敗：該列 → `failed`（寫 `last_error`），並**新增一列** `pending`、`attempt + 1`、`next_try_at = now + 退避`；`attempt` 已達 `max_attempts` 時該列改為 `giving_up`，不再新增。
  - 永久性失敗：該列 → `failed`，`next_try_at = NULL`，不再新增。
- `target` 欄位存 `folder:<dir>`、`email:<收件者>`、`webhook:<去除帳密與 query 的 URL>`：它同時是重啟後找回設定的鍵（以傳真重新比對路由，再找 `target` 相同的 delivery）。同一路由內 `target` 不可重複（`validate` 檢查）。
- 排程用資料庫為唯一事實來源：重啟後直接接續。先插入下一次嘗試、再關閉目前這列；若在兩步之間當機，會留下「已有後繼的 pending 列」，下次執行時被關閉為 `superseded`，不會重複投遞（至少一次，不會默默遺失）。
- 投遞設定改掉或刪除後，仍在排隊的列會被關閉為 `failed`（"no longer configured"），不會誤送到別處。

### 1.2 錯誤分類（自行決定的部分，請審核）
| 類別 | 條件 | 處理 |
|---|---|---|
| 暫時性 | 連線失敗、逾時；HTTP 5xx、408、425、429；SMTP 4xx、TLS／憑證錯誤；folder 目錄不存在（`create_dir = false`）、權限不足與其他 I/O 錯誤 | 依退避表重試 |
| 永久性 | HTTP 4xx（上列以外）與 3xx（**不跟隨轉址**）；SMTP 5xx（含 AUTH 失敗 535、收件者不存在）；來源檔案不存在／已清整／無 PDF 路徑；範本或位址設定錯誤 | `failed`，不重試 |
- 理由：永久錯誤重試 10 次（約 17 小時）只會製造雜訊；紀錄仍在 DB，M4 的介面可提供「重新投遞」。**代價**：管理員修正密碼或 URL 後，已標記 `failed` 的舊傳真不會自動重送（需要 M4 的手動重送，或補一個 CLI）。這是已知取捨，列於 §10。
- TLS 憑證驗證失敗歸為暫時性（管理員可能正在安裝 CA），會重試。

### 1.3 對 store 的修改
只加兩個方法，全部走既有的「單一 SQL＋薄驅動」架構，四種後端共用：
- `due_deliveries`：`status = 'pending' AND (next_try_at IS NULL OR next_try_at <= ?)`，排序 `CASE WHEN next_try_at IS NULL THEN 0 ELSE 1 END, next_try_at, id`（NULL 一律排最前）。
- `update_delivery`：更新狀態、`last_error`（上限 4000 字，超過回 `TooLong`）、`next_try_at`、`updated_at`（取注入時鐘）。
- 過程中發現的跨後端差異見 §6 #1（PostgreSQL 的 NULL 排序）。

## 2. 指令
```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
T4_ORACLE=skip cargo test --workspace --all-targets      # 同 CI；不設 T4_ORACLE 時 t4 的 oracle 測試在本機（沒有 libtiff/poppler）會失敗，與本變更無關
cargo deny check
# 需要 SMTP 容器的 D03 案例：
eval "$(scripts/smtp-test-servers.sh up)" && cargo test -p delivery --test email && scripts/smtp-test-servers.sh down
cargo llvm-cov -p delivery --fail-under-lines 80          # 有／沒有 SMTP 容器都過（見 §4）
scripts/db-matrix.sh                                       # store 套件（含新的 s13）在 PostgreSQL／MySQL／MariaDB／MSSQL
```
本機 Windows 11 執行；unix 專屬的權限測試另在 Linux 容器（`rust:latest`，非 root uid 1000）執行（§3 D02）。

## 3. 案例結果
所有測試都在真正的 SQLite 檔案資料庫、真正的本機監聽伺服器上執行；時間全部由 `store::ManualClock` 控制，沒有任何 `sleep` 等待邏輯（僅兩個背景 worker 測試以短輪詢等待檔案出現，上限 5 秒）。

| ID | 案例（測試名稱開頭） | 結果 |
|---|---|---|
| D01 | `router::d01_exact_prefix_trunk_default_and_none`、`d01_default_route_catches_the_rest`（精確、前綴、trunk、AND 條件、default、沒有符合、沒有 DNIS）；`config::d01_*`（TOML 解析、未知欄位／kind 被拒、validate：缺條件、default 非最後、重複 target…）；`routing_folder::d01_engine_uses_first_matching_route_and_all_its_deliveries`（每條路由多個 deliveries，實際投遞到各目錄）、`d01_no_matching_route_and_unfinished_faxes_queue_nothing`、`d01_invalid_configuration_is_rejected_by_the_engine` | 通過 |
| D02 | `folder::d02_*`（兩種格式與範本檔名、格式選擇、自動建立目錄、`create_dir=false` 時失敗可重試、目錄其實是檔案、目標被佔用時失敗且不留 `.part`、冪等且不覆蓋不同內容、惡意欄位值不逃出目錄、來源問題為永久且先不寫任何檔）；`template::d02_*`（全部 placeholder、UTC 偏移、缺值、語法錯誤、路徑淨化）；`source::d02_*`；`routing_folder::d02_folder_missing_directory_retries_until_it_exists`（失敗→+1 分鐘重試→目錄出現後成功，位元組相同）、`d02_folder_auto_creates_directory_by_default`、`d02_folder_template_with_utc_offset_and_traversal_attempt`、`d02_folder_missing_source_file_is_a_permanent_failure`、`d02_background_worker_*`、**`d02_folder_permission_denied_is_retried`**（唯讀目錄 0o555 → 失敗重試、無殘留 `.part` → chmod 後成功；`#[cfg(unix)]`） | Windows 本機全部通過；權限案例在 Windows 不編譯，**改在 Linux 容器（非 root）實跑通過**（`routing_folder` 12 個測試，含此案例）。CI 的 ubuntu runner 以非 root 執行，會跑這個案例 |
| D03 | `email::d03_mailhog_plain_message_fields_and_attachment_bytes`（MailHog：寄件者、2 位收件者含顯示名稱、SMTP envelope、主旨、內文各欄位；附件經 `mail-parser`（獨立於 lettre）解出，**PDF bytes 與原檔完全相同**，檔案含全部 256 種位元組與單獨一行「.」）、`d03_mailhog_tiff_attachment_and_custom_names`、`d03_starttls_with_auth_and_attachment_bytes`（smtp4dev：STARTTLS＋AUTH，要求帳密）、`d03_starttls_wrong_password_is_a_permanent_failure`、`d03_starttls_untrusted_certificate_is_refused_unless_insecure`、`d03_smtps_implicit_tls`、`d03_scripted_server_receives_exactly_the_built_message`、`d03_smtp_error_classes`（450／451／452 暫時、550／553／554 永久）、`d03_connection_refused_is_transient`、`d03_missing_pdf_is_permanent_and_nothing_is_sent`、`d03_build_message_rejects_bad_input_and_neutralises_header_injection`、`template::d03_*` | 本機以 MailHog＋2 個 smtp4dev 容器全部通過（11 個）。沒設 `SIP_FAXD_TEST_SMTP*` 時，需要容器的 5 個案例印出 `skip` 並通過，其餘 6 個（腳本化 SMTP 伺服器）照跑 |
| D04 | `webhook::d04_webhook_posts_json_with_record_fields_and_signature`（逐欄檢查 JSON、header、**簽章由測試內獨立手寫的 HMAC-SHA256（RFC 2104＋`sha2` 雜湊）重算比對**，並用另一把 key 證明比對非空洞）、`d04_test_hmac_matches_rfc4231_vectors`（獨立實作自己先通過 RFC 4231 向量 2、1、6）、`d04_webhook_without_secret_has_no_signature_header`、`d04_webhook_multipart_attaches_pdf_and_signs_the_whole_body`（手動解析 multipart；PDF bytes 相同；簽章涵蓋整個 body）、`d04_webhook_multipart_without_pdf_file_fails_permanently`、`d04_webhook_status_classes_decide_retry`（500／502／503／429／408 重試；400／401／404／410／302 不重試）、`d04_webhook_connection_refused_is_transient_and_error_hides_url_secrets`（目標與錯誤訊息不含帳密與 token）、`d04_webhook_does_not_follow_redirects`；`webhook::d04_*`、`config::d04_*` | 通過 |
| D05 | `retry::d05_schedule_is_1_5_15_60_then_4h_and_gives_up_after_max_attempts`（對 10 次嘗試逐一驗證 `next_try_at` 精確到毫秒，並在「差 1 毫秒」時確認不會被取出；第 10 次後 `giving_up`；再過 30 天也不再送；傳真紀錄不變）、`d05_max_attempts_is_configurable`、`d05_recovers_on_a_later_attempt`、`d05_connection_failure_is_retried_until_the_server_returns`、**`d05_restart_continues_from_the_database`**（SQLite 檔案：關閉並重新開啟 DB、重建 engine，從第 3 次接續；重送 dispatch 不重複）、`d05_permanent_error_is_not_retried`、`d05_pending_row_with_successor_is_closed_not_run_twice`（當機中間態）、`d05_orphans_are_closed_cleanly`、`d05_background_worker_picks_up_due_retries_by_polling`、`retry::d05_backoff_table` | 通過 |
| D06 | `routing_folder::d06_partial_follows_the_route_setting`（partial 在 `deliver_partial=false`／`true` 的路由各自的行為；`failed`／`no_fax` 永不投遞）、`d06_catch_up_queues_missed_faxes_once`、`webhook::d04_webhook_without_secret_has_no_signature_header`（partial 事件） | 通過 |
| S（store） | 新增 `s13_delivery_queue`（到期篩選、NULL 排序、limit、update、錯誤長度）；其餘 37 個既有 store 案例不變 | 四種後端全部通過（§5） |

測試數量：delivery 單元 30、`email` 11、`retry` 8、`routing_folder` 11（Linux 12）、`webhook` 8，共 68（Linux 69）；store 38（含 s13）。`T4_ORACLE=skip cargo test --workspace --all-targets`：48 個測試執行檔、702 個測試、0 失敗（含 SMTP 容器）。

### 3.1 變異檢查（測試真的抓得到錯嗎）
手動注入 8 個錯誤，每個都被測試抓到：退避 15→16 分鐘（`d05_backoff_table`）；HMAC 少簽第一個位元組（`d04_signature_known_vector`）；檔名淨化放行 `/`（`d02_sanitize_*`、`d02_folder_hostile_values_*`）；`max_attempts` 的 `>=` 改 `>`（`d05_max_attempts_*`、`d05_schedule_*`）；前綴比對 `starts_with`→`contains`（`d01_exact_prefix_*`）；移除 HTTP 408 的暫時分類（`d04_classify_status_table`）；冪等判斷改成「檔名存在就算成功」（`d02_folder_is_idempotent_*`）；`deliver_partial` 檢查被關掉（`d06_partial_*`）。（`cargo test` 在第一個失敗的測試執行檔就停止，所以每個變異只列出最先失敗的那個執行檔的結果。）

## 4. 涵蓋率（`cargo llvm-cov -p delivery`）
| 條件 | 行涵蓋率 |
|---|---|
| 含 SMTP 容器（MailHog＋2 個 smtp4dev） | **96.67%**（1713 行，漏 57） |
| 不設任何 SMTP 容器 | **95.91%**（1713 行，漏 70）；`email.rs` 84.4% |
各檔（含容器）：config 98.99、email 97.92、engine 90.68、error 83.33、folder 93.77、retry 100、router 100、source 98.77、template 100、webhook 100。門檻 80%，兩種情況都過，所以 `ci-local.sh` 不需要容器也能通過閘門。未涵蓋的主要是 engine 的「DB 錯誤向上傳遞」分支與任務 panic 的記錄。

## 5. store 在四種資料庫（`scripts/db-matrix.sh`，Docker）
| 後端 | 結果 |
|---|---|
| PostgreSQL 16 | 38 通過 |
| MySQL 8.0 | 38 通過 |
| MariaDB 11 | 38 通過 |
| MSSQL 2022 | 38 通過 |
SQLite（檔案與記憶體）在一般 `cargo test` 通過。第一次跑 PostgreSQL 時 `s13` 失敗，根因見 §6 #1，修正後重跑全部四種都通過。

## 6. 過程中的失敗與根因
1. **PostgreSQL 的 `ORDER BY next_try_at` 把 NULL 排在最後**，其他三種排在最前（`s13_delivery_queue` 在 PostgreSQL 失敗：預期 `[3,2,1]` 實得 `[2,1,3]`）。修正：排序加 `CASE WHEN next_try_at IS NULL THEN 0 ELSE 1 END` 前置鍵，四種後端一致。這正是「同一份測試跑四種資料庫」該抓到的問題。
2. `cargo deny check` 失敗：`quoted_printable 0.5.2`（lettre 的相依）授權為 0BSD，不在允許清單。0BSD 是寬鬆授權（OSI 核准、比 BSD 更寬鬆）；在 `deny.toml` 的 `allow` 加入 `"0BSD"`（附註來源）。見 §8 ADR-C。
3. 簽章測試一開始沒有失敗但 clippy 報 `unused_assignments`（`webhook.rs` 測試的 `pos` 變數）：改寫。
4. 單元測試兩個失敗：(a) 文字模式只把控制字元換成空白但保留 `\n`，使 `trunk = "x\r\nBcc: ..."` 的值能夠在內文以外的位置插入換行——改成「變數值」一律把所有控制字元換成空白，只有範本本身的字面換行保留（內文可多行）；(b) 我手算的淨化結果少一個底線，修正預期值（行為本身正確：無路徑分隔符、無 `..`）。
5. smtp4dev 的 TLS 測試：`dangerous_accept_invalid_certs(true)` 不會略過主機名稱檢查（連到 `127.0.0.1`，憑證是容器 ID）。`tls_insecure = true` 現在同時設定 `dangerous_accept_invalid_hostnames`。
6. smtp4dev 預設 `SmtpAllowAnyCredentials = true`，「密碼錯誤」案例因此被接受。啟動參數加 `ServerOptions__SmtpAllowAnyCredentials=false`（腳本與 CI 都已設定）。
7. 本機 Windows 的 Docker 無法綁定 8025／18025 等埠（Windows 保留埠範圍），本機腳本改用 31xxx。CI 不受影響。
8. 本機 `cargo test --workspace --all-targets` 在沒有設 `T4_ORACLE=skip` 時 `t4` 的 7 個 oracle 測試失敗（缺 libtiff／poppler），與本變更無關；CI 本來就設 `T4_ORACLE=skip`。
9. 第一次 Linux 容器執行誤用了 `--offline` 與 Git Bash 的路徑轉換（`-w /work` 被改成 `C:/Program Files/Git/work`）；改用 `MSYS_NO_PATHCONV=1` 後正常。（工具問題，非程式問題。）

## 7. 依賴與 `cargo deny`
新增（根 `[workspace.dependencies]`）：
| crate | 版本／feature | 用途 |
|---|---|---|
| `lettre` | 0.11，`default-features = false`，`builder`、`smtp-transport`、`tokio1-rustls-tls`（rustls＋webpki-roots＋ring） | SMTP |
| `reqwest` | 0.13，`default-features = false`，`rustls`、`json`（rustls＋aws-lc-rs＋平台憑證驗證） | webhook |
| `hmac` 0.13、`sha2` 0.11 | | HMAC-SHA256（兩者本來就在 lock 檔中） |
測試用（只在 `crates/delivery` 的 dev-dependencies）：`axum 0.8`（`http1`、`tokio`；本機 HTTP 伺服器）、`mail-parser 0.11`（獨立解析郵件）、`toml`、`tempfile`。
- `cargo deny check`：advisories ok、bans ok、licenses ok、sources ok（需要 §6 #2 的 `0BSD`）；僅有既有的 duplicate 警告（新增 `base64 0.23` 與 sqlx 的 0.22 並存，等）。
- `cargo tree -p delivery`：沒有 `native-tls`、`openssl`。TLS 為 rustls；lettre 用 ring、reqwest 用 aws-lc-rs（ADR-038 已接受兩者並存）。

## 8. 提議的 ADR（供 Opus 合併到 `docs/DECISIONS.md`）
- **ADR-A：投遞採「每次嘗試一列」的資料庫佇列，不另設記憶體排程。** 規格 §1.2 的 `attempt`／`status`／`next_try_at` 直接當佇列；重試新增下一列，失敗列保留 `last_error`。重啟只需要重新查詢；當機中間態以「已有後繼即視為 superseded」處理。取代方案（單列就地更新 `attempt`）會失去每次嘗試的歷史。
- **ADR-B：投遞錯誤分暫時／永久。** 暫時性依 1、5、15、60 分鐘、之後每 4 小時重試至 `max_attempts`（預設 10）→ `giving_up`；永久性（HTTP 4xx 除 408／425／429、3xx 不跟隨、SMTP 5xx、來源檔案不存在、設定錯誤）直接 `failed`。取捨見 §1.2。
- **ADR-C：依賴 lettre 0.11（`tokio1-rustls-tls`）與 reqwest 0.13（`rustls`），並在 `deny.toml` 允許 `0BSD`。** `quoted_printable`（0BSD）由 lettre 引入。無 OpenSSL／native-tls。
- **ADR-D：webhook 簽章為 `HMAC-SHA256(secret, 實際送出的 body 位元組)`，header 預設 `X-Sipfax-Signature: sha256=<hex>`；multipart 時簽整個 body。** 接收端以原始 body 驗證，不需重組 JSON。multipart 由本 crate 自行組裝（沒有用 reqwest 的 multipart feature），才能簽署確切的位元組。
- **ADR-E：`delivery_attempts.target` 為穩定識別字（`folder:<dir>`／`email:<收件者>`／`webhook:<URL 去除帳密與 query>`），不含密碼或 token。** 同一路由內需唯一；是重啟後找回設定的鍵。
- **ADR-F：folder 投遞預設自動建立目錄（`create_dir = true`）；設為 false 時目錄不存在視為暫時性失敗並重試。** 另：同名但內容不同的檔案不覆蓋，改加 `_1`、`_2`；內容相同視為已投遞（冪等）。
- **ADR-G：檔名與郵件文字範本。** placeholder `{date}{time}{ani}{dnis}{id}{trunk}{pages}{call_id}{status}{duration}{started_at}{ended_at}{resolution}{tsi}`；檔名模式把非 `[字母數字 - _ . + ( )]` 一律換成 `_`、去掉開頭的點與 `..`、長度上限 200 位元組；文字模式把變數值中的控制字元換成空白（防 header injection）。`{date}`／`{time}` 預設為 UTC，可用 `EngineOptions.utc_offset_secs` 改成本地時區。

## 9. 對規格的偏離與補充
| 項目 | 偏離／補充 | 理由 |
|---|---|---|
| 一次嘗試一列 | 規格只說「每一次投遞都寫進 delivery_attempts」，我採「一次嘗試一列」，`failed` 表示該次嘗試失敗（不一定終結），`giving_up` 才是用盡 | §1.1 |
| `Router::match` | 命名為 `Router::find` | `match` 是保留字 |
| `filename` | 範本不含副檔名亦可；末尾的 `.pdf`／`.tif`／`.tiff` 會被忽略，每種格式自動加上對應副檔名（規格範例 `{date}_{ani}_{id}.pdf` 可直接使用） | 同時複製 PDF 與 TIFF |
| 路由驗證 | `default = true` 必須是最後一條；不可與 `dnis`／`dnis_prefix`／`trunk` 併用；路由需要至少一個 delivery；一條路由內多個條件為 AND | 避免永遠不會比對到的路由 |
| email AUTH＋明文 | `security = "none"` 同時設帳密會被 `validate` 拒絕 | 避免在明文連線送出密碼 |
| 沒做 | webhook 的 `tls_insecure`／自訂 CA 檔（HTTPS 使用作業系統的信任庫）；email 的 HTML 內文；單一路由內不同 delivery 的獨立 `max_attempts`；路由改存 DB（規格留給 M4） | 範圍 |

## 10. 未解事項（誠實清單）
1. **CI 變更沒有在 GitHub 上實跑過**：`test`／`coverage` job 新增的 `services`（mailhog、兩個 smtp4dev）、等待步驟、以及 job 層級 `env`，我只以相同映像、相同環境變數在本機 Docker 驗證了測試本身。可能出問題的地方：services 容器的埠對映（`2525:25` 等）、`mailhog/mailhog` 是 amd64 映像（ubuntu runner 沒問題）、smtp4dev 啟動時間（等待步驟最多 2 分鐘）。YAML 沒有用 linter 驗證（本機沒有 yaml 工具）。第一次 CI 請留意 `test`、`coverage` 兩個 job。
2. **權限不足案例在 Windows 不執行**：`#[cfg(unix)]`；已在 Linux 容器（非 root）實跑通過。以 root 執行時該案例會自行略過並印出訊息。
3. **沒有驗證 musl 靜態連結**：`delivery` 還沒被 `sip-faxd` 依賴，所以現有的 `build-musl` job 不會編譯它；lettre（ring）、reqwest（aws-lc-rs）在 musl 的行為沒有驗證。reqwest 的 `rustls` feature 使用 `rustls-platform-verifier`（在 Linux 讀取系統信任庫）；RHEL 9／10 的行為與內部 CA 情境沒有實測。接線之後請跑 `scripts/ci-local.sh`（含 musl）確認。
4. **永久失敗不會自動恢復**（§1.2）：管理員修正設定後，已 `failed` 的舊傳真不會重送；需要 M4 的「重新投遞」按鈕或 CLI。`giving_up` 同理。
5. **沒有測試 HTTPS webhook**（沒有 TLS 測試伺服器）；HTTP 路徑與 reqwest 的 TLS 設定是分開的，HTTPS 只靠 reqwest 本身。email 的 STARTTLS／SMTPS 有實測（自簽憑證，`tls_insecure`）。
6. email 的「憑證不受信任會被拒絕」測試只能證明預設拒絕自簽憑證（錯誤訊息含 certificate），沒有分別驗證「主機名稱不符」與「簽發者不明」兩種情況。
7. **at-least-once**：當機或逾時可能造成同一封傳真重複投遞一次（folder 因內容相同而冪等；email／webhook 不是）。webhook 帶 `attempt` 欄位與 `id`，接收端可去重；沒有提供 idempotency key header。
8. `DeliveryEngine::run_due` 內任務 panic 只記錄 error，該列會一直是 `pending` 而被反覆取出（沒有針對 panic 的退避）。目前所有程式路徑都沒有 `unwrap` 於外部輸入，但沒有測試。
9. 並行：同一個資料庫由多個 sip-faxd 實例同時跑 worker 時，可能重複取出同一列（沒有 claim／鎖）。目前假設單一實例；如要支援，需要 `UPDATE ... WHERE status = 'pending'` 式的認領。
10. 規格 §7 的紀錄清整會刪除 `delivery_attempts`；若 worker 當時正在執行該傳真的投遞，更新會回傳「沒有列被更新」，目前忽略該結果（只影響已被清整的資料）。
11. `catch_up(since)` 需要 sip-faxd 在啟動時呼叫（範例已在 `lib.rs`）；沒有它，「finalize 成功但 dispatch 前當機」的傳真不會被投遞。
12. 本機沒有 `cargo deny` 與 `cargo llvm-cov` 時我自行安裝了（`cargo install --locked`）；版本分別為 cargo-deny 0.20.2、最新的 cargo-llvm-cov。

## 11. sip-faxd 接線說明
完整範例在 `crates/delivery/src/lib.rs` 的文件註解。重點：
1. 設定：daemon 設定裡放 `routes: delivery::Routes`（`#[serde(flatten)]`，或直接用 `Vec<Route>` 欄位）；`config check` 呼叫 `routes.validate()`（回傳所有錯誤字串）。
2. 啟動：store 連線後 `DeliveryEngine::new(store.clone(), &routes, data_dir, clock, EngineOptions::default())?`；接著 `engine.catch_up(now - 1 天)`；再 `engine.spawn()` 取得 `DeliveryHandle`。`EngineOptions.utc_offset_secs` 可設為本地時區偏移（影響 `{date}`／`{time}`）。
3. fax-in：`store.finalize(..)` 成功後呼叫 `handle.notify_finalized(record.id)`（不阻塞、不會失敗；`handle.notifier()` 可取得可複製的 `UnboundedSender<i64>`）。DB 寫入失敗時不會通知，由下次啟動的 `catch_up` 補上。
4. 關閉：`handle.shutdown().await`（等進行中的嘗試結束）。
5. 時鐘：傳入和 store 相同的 `Arc<dyn store::Clock>`（`store::SystemClock`）。
6. 需要合併時處理：`Cargo.toml` 根目錄 `[workspace.dependencies]` 與 M3b 加入的 `store`／`delivery` path 依賴可能有小衝突；`docs/database.md` 的 `delivery_attempts` 說明建議補上「每次嘗試一列」與 `target` 的格式。

## 12. 異動檔案
- 新增：`crates/delivery/src/{config,email,engine,error,folder,retry,router,source,template,webhook}.rs`、`crates/delivery/tests/{common/mod.rs,email.rs,retry.rs,routing_folder.rs,webhook.rs}`、`scripts/smtp-test-servers.sh`、`docs/test-reports/M3c.md`
- 修改：`crates/delivery/{Cargo.toml,src/lib.rs}`、`crates/store/src/{store.rs,sqlstore.rs}`、`crates/store/src/tests/store_tests.rs`、`Cargo.toml`（`[workspace.dependencies]`）、`Cargo.lock`、`deny.toml`（`0BSD`）、`.github/workflows/ci.yml`、`scripts/ci-local.sh`

## Opus 審核結論（2026-10-21）
**結論：通過。**

- 依賴只開 rustls，`deny.toml` 新增的 `0BSD` 屬寬鬆授權，可接受。HMAC 用測試內手寫實作獨立重算，email 附件以 `mail-parser` 解析後比對 bytes，符合「獨立對照」要求。
- 接受 ADR-A～G（合併為 ADR-047、048）。
- 接線（`catch_up`、`notify_finalized`）與 musl 靜態連結驗證在 M3f 完成。手動重新投遞列入 M4。at-least-once 與單實例假設已寫進 ADR-047。
