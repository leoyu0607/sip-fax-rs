# M4b 測試報告：web crate（REST、WebSocket、HTTP/HTTPS 與自簽憑證、安全 header）

- 實作：Opus（M4b ①–⑤）；測試與本報告：Sonnet（M4b ⑥）
- 計畫：`docs/test-plans/M4.md` §0、§2 A01–A13 的 HTTP 部分、§3 W01–W11、§4 V01–V06、§5 H01–H08；規格：`docs/design/m4-web.md` §2–§5；ADR-056
- 分支：`m4b-web`
- 狀態：**待 Opus 審核**（§9 由 Opus 填寫）

## 1. 範圍與交付內容

| 項目 | 位置 |
|---|---|
| A10／A11 表格驅動：規格 §2.1 與 §3 的每一條路由 × {座席、管理員、未登入}（含 `/config`、`/users`、`/status`、`/events`、`/faxes`）；被拒的請求都檢查狀態碼、錯誤代碼，並比對「使用者、設定區段、稽核、傳真列與投遞」的完整快照沒有變動；A10 對每一條會改變狀態的路由送出六種壞的 CSRF／content type | `crates/web/tests/permissions.rs`（4 個） |
| A01、A03、A04、A05（HTTP 計時）、A07、A08（管理員重設密碼）的 HTTP 版（注入時鐘） | `crates/web/tests/sessions.rs`（7 個） |
| A09 在真正的 HTTPS 連線上驗證 `Secure`；H07 在 HTTPS 上對每種狀態碼都有 HSTS | `crates/web/tests/server.rs`（新增 2 個） |
| W10：41 種錯誤情境（401／403／404／405／400／409／413／422、非 UTF-8 路徑…）都是 `{"error": {code, message}}`、`Content-Type: application/json`，且不含路徑、SQL、驅動名稱；W11：413 且 DB 不變、408 帶錯誤格式與安全 header、header 不送完 30 秒後被關閉（虛擬時間）；H07：每種狀態碼都有安全 header | `crates/web/tests/errors.rs`（5 個） |
| W01（每個篩選與組合對照 store、分頁走完每筆一次、竄改的 cursor）、W03（切回未讀、記錄帳號與時間）、W05（IHDR 解出寬高、縮圖較小、ETag）、W06（非 ASCII 號碼的下載檔名）、W07（紀錄指向 `data_dir` 外時，PNG 也是 404）、W08（`giving_up` 重排、之後成功的目標不重排、事件、404） | `crates/web/tests/fax_api.rs`（6 個） |
| config 的 trunk 新增／排序／刪除與稽核；W09：DB 斷線時 `/status` 與傳真路由（503、不洩漏位址）、服務狀態與待重啟；A12：兩個管理員、被拒的請求不改任何東西 | `crates/web/tests/admin_more.rs`（4 個） |
| V01：真正的 `fax_in::recorder::Recorder` 寫真正的 store，事件經 EventBus、伺服器、WebSocket 到客戶端；V04：過期／登出／不存在的 session 握手 401；V06：100 個連線、一個慢的 | `crates/web/tests/events_more.rs`（3 個） |
| H01／H05／H08 用 `openssl` 驗證（`#[ignore]`，由腳本在 Docker 內執行） | `crates/web/tests/openssl.rs`（3 個）、`scripts/web-openssl-docker.sh`、輸出 `docs/test-reports/M4b-openssl-docker.log` |
| 啟動流程（`web_service::start`）：`recorder_events` 的對應、H03 啟動失敗訊息、H04 啟動時的更換／警告、H06 明文 HTTP 的警告、H08 log 中的指紋 | `apps/sip-faxd/src/web_service.rs` 的 `mod tests`（6 個） |
| 既有測試的整理：events 測試依計畫編號改名（見 §3 第 5 點）；弱斷言改成精確斷言（§3 第 6 點）；`common::env_seeded(toml)` 讓測試以檔案匯入 trunk | `crates/web/tests/{events,admin,server,faxes}.rs`、`tests/common/mod.rs` |
| 產品碼（測試發現的問題，見 §3） | `crates/web/src/{error,lib}.rs`、`crates/web/src/api/{faxes,users}.rs` |
| CI／腳本 | `.github/workflows/ci.yml`、`scripts/ci-local.sh`（見 §7） |

`web` crate 目前共有單元測試 23 個、整合測試 70 個（其中 3 個 `#[ignore]` 的 openssl 測試由腳本執行）；`sip-faxd` 的 `web_service` 另有 6 個。測試前是單元 23、整合 36（admin 10、auth 8、events 6、faxes 9、server 3）。

## 2. 案例對照表

「M4a」指 `docs/test-reports/M4a.md` 已通過審核的測試。測試檔都在 `crates/web/tests/`，括號內是檔名；沒有檔名的是 `crates/web/src/` 內的單元測試。測試名稱都以案例編號開頭。

### 2.1 帳號與驗證（A）

| ID | 測試 | 驗證方式 | 結果 |
|---|---|---|---|
| A01 | `sessions::a01_the_first_admin_logs_in_with_the_file_must_change_it_and_the_file_goes_away`；`auth::a01_an_account_that_must_change_its_password_may_only_do_that`；`faxes::a01_a11_without_a_session_401_and_before_the_password_change_403`；`permissions::a11_a_session_that_must_change_its_password_is_refused_everywhere_else`；M4a `accounts::a01_…` | 初始密碼檔 → HTTP 登入；改密碼前，所有路由（含矩陣內每一條）回 403 `password_change_required` 且檔案仍在；規則不合的改密碼被拒且保留檔案；改完檔案被刪、舊密碼失效、所有路由恢復 | 通過 |
| A02 | M4a `accounts::a02_*`（argon2id 參數、`rust-argon2` 與 OpenSSL 互驗） | 屬 M4a，沒有 HTTP 部分 | 通過（M4a） |
| A03 | `sessions::a03_the_password_rules_are_422_with_fixed_codes_over_http_and_nothing_changes` | 改密碼與管理員建立帳號：太短、129 字元、與帳號相同（大小寫不分）都是 422 與固定代碼；沒有建立帳號、舊密碼仍可登入；剛好 10 字元可以 | 通過 |
| A04 | `sessions::a04_five_failures_lock_the_account_for_fifteen_minutes_over_http` | 5 次失敗，第 6 次密碼正確仍 423 `account_locked`＋`Retry-After`（800–900）且沒有 `Set-Cookie`；注入時鐘每分鐘試一次，14:59 仍鎖，15:01 可登入 | 通過 |
| A05 | `auth::a05_unknown_account_and_wrong_password_get_the_same_401`；`sessions::a05_over_http_unknown_and_wrong_password_take_the_same_time_within_20_percent` | 回應本文逐位元相同；HTTP 上各量 50 次取中位數，差異 < 20%，且兩條路徑都真的雜湊（> 1 ms） | 通過 |
| A06 | `auth::a06_more_than_twenty_attempts_a_minute_from_one_address_get_429` | 第 21 次（即使密碼正確）429＋`Retry-After`＋沒有 session；61 秒後恢復 | 通過 |
| A07 | `sessions::a07_idle_expiry_is_8_hours_from_the_last_request_with_the_edge_one_second_either_side`、`sessions::a07_the_absolute_limit_is_7_days_however_busy_the_session_is`；`events_more::v04_…`（握手） | 注入時鐘：閒置 8 小時前後各 1 秒、從最後一次使用算起；每 7 小時一次請求，7 天前後各 1 秒仍以絕對上限過期；過期後所有路由 401 `session_expired` | 通過 |
| A08 | `auth::a08_logout_and_a_password_change_end_the_sessions`；`sessions::a08_an_administrator_resetting_a_password_ends_all_sessions_of_the_account`；`admin::disabling_an_account_ends_its_sessions`；`events::a08_logout_password_and_disable_close_the_connections` | 登出、改密碼、管理員重設密碼（規則不合的重設不影響 session）、停用：該帳號所有 session 失效；WebSocket 以 4001 關閉，其他帳號的連線不受影響 | 通過 |
| A09 | `auth::a09_the_session_cookie_is_httponly_samesite_strict_and_not_secure_over_http`、`auth::a09_over_https_the_cookie_is_secure`、`server::a09_over_real_https_the_cookie_is_secure_httponly_samesite_strict_and_works` | HTTP：沒有 `Secure`；真正的 HTTPS 連線：四個屬性齊全、cookie 可用、沒有 cookie 的客戶端是陌生人、登出時清除用的 cookie 也帶 `Secure` | 通過 |
| A10 | `permissions::a10_every_route_that_changes_something_refuses_a_missing_or_wrong_csrf_header_or_a_wrong_content_type`；`auth::a10_a_change_without_json_or_without_the_right_csrf_header_is_403_and_changes_nothing` | 每一條會改變狀態的路由（POST／PUT／DELETE，共 9 條）× 六種壞請求（沒有 header、錯誤 token、空 token、form、text/plain、沒有 content type）＋另一個 session 的 token：一律 403 `csrf`，DB 快照不變 | 通過 |
| A11 | `permissions::a11_the_permission_matrix_of_the_spec_holds_for_every_route_and_role`、`permissions::a11_the_matrix_covers_every_route_of_the_router`、`permissions::a11_a_session_that_must_change_its_password_is_refused_everywhere_else`；`admin::agents_and_strangers_are_refused_and_nothing_changes`；M4a `accounts::a11_*` | 規格 §2.1 的每一列與 §3 的每一條路由（22 條）× 座席／管理員／未登入，共 66 格：允許的回 2xx（`/events` 回 400 表示過了驗證、只是不是升級請求），不允許的回 401／403＋對應錯誤代碼，且「使用者、設定、稽核、傳真、投遞」快照不變；結束後也檢查允許的請求真的生效（設定、trunk 刪除、新帳號、已讀與備註的帳號、只有管理員的重新投遞新增了一列） | 通過 |
| A12 | `admin::a12_the_last_admin_stays`；`admin_more::a12_two_admins_may_step_down_one_at_a_time_and_a_refused_request_changes_nothing`；M4a `accounts::a12_*` | 最後一個 admin 停用／降級／刪除都是 409 `last_admin`；同一個請求夾帶重設密碼或改顯示名稱時也一樣，且**帳號、密碼、session 都沒有被動到**（§3 第 4 點修正）；兩個 admin 時可以降級一個，被降級者的 session 結束且變成 403 | 通過 |
| A13 | M4a `apps/sip-faxd/tests/m4a_cli.rs::a13_user_commands_create_list_disable_enable_and_change_passwords` | 屬 M4a | 通過（M4a） |

### 2.2 REST API（W）

| ID | 測試 | 驗證方式 | 結果 |
|---|---|---|---|
| W01 | `faxes::w01_the_list_filters_pages_and_refuses_a_bad_cursor`；`fax_api::w01_every_filter_gives_what_the_store_gives_and_paging_walks_each_fax_once`；`api::faxes::tests::w01_list_parameters_are_checked` | 8 筆傳真（2 個 trunk、4 種狀態）：`status`、`trunk`、`dnis`／`ani` 前綴、`is_read`、`from`、`to` 與組合共 14 種查詢，結果與直接呼叫 store 的 `list` 相同（含 `%` 不是萬用字元），並有手算的預期值；每頁 3 筆走完 8 筆，每筆一次、新到舊、最後一頁沒有 cursor；`abc`、`-1`、`1:2`、過大的數字、SQL 片段等 cursor 都是 400 | 通過 |
| W02 | `faxes::w02_one_fax_with_its_deliveries_and_404_for_an_unknown_one`；`api::faxes::tests::w02_*` | 欄位齊全、投遞列表、webhook 目標遮蔽（token 不出現）、沒有檔案路徑；不存在、0、負數、非數字都是 404 | 通過 |
| W03 | `faxes::w03_the_read_flag_records_who_changed_it`；`fax_api::w03_the_read_flag_toggles_back_records_the_account_and_the_time_each_time`；`events::v01_hello_then_the_events_of_the_api` | 兩個帳號先後切換：`read_changed_by` 與時間每次都更新、切回未讀；不存在的傳真 404；`fax.read_changed` 事件帶帳號 | 通過 |
| W04 | `faxes::w04_a_note_conflict_is_409_with_the_current_note_and_changes_nothing` | 409 附目前備註、修改者與時間，DB 不變；帶最新時間後成功 | 通過 |
| W05 | `faxes::w05_page_png_is_the_preview_file_with_etag_304_404_and_410`；`fax_api::w05_the_png_is_a_real_png_of_the_page_and_the_thumbnail_is_smaller` | 內容與 `Preview` 的快取檔相同；PNG 檔頭與 IHDR 解出寬高（≥ 800，縮圖較小）；`ETag` 相同回 304 且沒有本文、不同的圖有不同的 tag、舊 tag 回 200；頁碼 0／2／不是 png 回 404；已清整回 410 | 通過 |
| W06 | `faxes::w06_downloads_are_the_files_with_a_safe_name_and_410_once_purged`；`fax_api::w06_a_number_with_characters_outside_ascii_gives_a_safe_download_name`；`api::faxes::tests::w06_*` | bytes 與磁碟檔案相同、`Content-Type`；號碼含中文、引號、反斜線、`/`、`..` 時，標頭是純 ASCII、檔名不含危險字元，`filename*` 與純名稱一致；中文檔名的 RFC 5987 編碼由單元測試驗證（`download_name` 只產生 ASCII，所以 HTTP 上走不到）；`Cache-Control: private, no-store`；已清整 410 | 通過 |
| W07 | `faxes::w07_tricks_in_the_path_never_read_outside_the_data_directory`；`fax_api::w07_a_stored_path_outside_data_dir_is_404_for_the_png_too_and_nothing_leaks`；`api::faxes::tests::w07_*` | `..`、編碼斜線、超大數字都是 400／404 且不含檔案內容；紀錄內的路徑指向 `data_dir` 外時，下載與 PNG（`full`／`thumb`）都是 404，回應不含檔名（§3 第 3 點修正：原本 PNG 是 500） | 通過 |
| W08 | `faxes::w08_redelivery_is_for_admins_and_only_requeues_failed_targets`；`fax_api::w08_giving_up_is_queued_again_a_target_that_later_succeeded_is_not_and_unknown_is_404`；`permissions::a11_*`（agent 403） | 座席 403 且投遞列不變；`failed`／`giving_up` 才重排、`attempt`＋1、新增一列 `pending`；之後已成功的目標不重排；第二次呼叫不再新增；有排入才發 `delivery.updated`；webhook 密碼遮蔽；不存在的傳真 404 | 通過 |
| W09 | `admin::w09_status_reports_service_database_and_config`；`admin_more::w09_the_status_follows_the_service_and_lists_what_waits_for_a_restart`；`admin_more::w09_status_says_the_database_is_unavailable_and_the_fax_routes_are_503_without_details` | 通話數、註冊狀態、`stopping`、DB 狀態與 schema、金鑰狀態；改 `sip.bind` 後兩種角色的 `pending_restart` 都列出；座席看不到 `key`／`undecryptable`；DB 斷線（在真的 store 前面注入）時 `/status` 仍 200 並標 `unavailable`、傳真路由 503、回應不含 DB 位址、寫入被拒後 DB 不變、恢復後正常 | 通過 |
| W10 | `errors::w10_every_kind_of_error_has_the_shape_and_leaks_nothing_about_the_server`；`auth::w10_bad_bodies_get_the_error_shape`；`server::h07_plain_http_has_the_security_headers_and_no_hsts` | 41 種錯誤情境（見 §1）全部是 JSON 且有 `code`／`message`，不含路徑、SQL、驅動名稱、臨時目錄；§3 第 1、2 點修正了 405 與非 UTF-8 路徑 | 通過 |
| W11 | `errors::w11_a_body_over_1_mib_is_413_and_changes_nothing`；`errors::w11_the_408_of_a_slow_body_carries_the_error_shape_and_the_security_headers`；`errors::w11_a_client_that_does_not_finish_its_headers_is_dropped_after_30_seconds`；`admin::w11_a_slow_body_gets_408`；`auth::w10_bad_bodies_get_the_error_shape` | 超過 1 MiB 的 note、新帳號、設定都是 413 `too_large`，傳真列、帳號、稽核都沒有變；略小於上限的請求通過；慢速 body 在期限內得到 408 `timeout`（帶錯誤格式與安全 header）；header 只送一半的連線在虛擬時間 30 秒後被關閉（不實際等待），新連線仍正常 | 通過 |

### 2.3 WebSocket（V）

| ID | 測試 | 驗證方式 | 結果 |
|---|---|---|---|
| V01 | `events_more::v01_a_three_page_fax_gives_created_three_progress_and_finalized_with_consecutive_seq`；`web_service::tests::v01_the_recorder_hook_publishes_created_progress_and_finalized_in_order`（sip-faxd）；`events::v01_hello_then_the_events_of_the_api` | 真正的 `Recorder`（`on_written` 與 `recorder_events` 相同的掛鉤）寫真正的 SQLite：客戶端依序收到 `fax.created`、3 次 `fax.progress`（頁數 1、2、3）、`fax.finalized`，`seq` 為 1–5 連續，每個事件的 `id` 都是 DB 內那一列，之後 `GET /faxes/{id}` 是 3 頁、`received`；晚來的客戶端以 `since=0` 補送相同的 5 個事件。sip-faxd 的測試對真正的 `recorder_events` 掛鉤送同樣的寫入通知。**不是 Docker 內以 SpanDSP 收傳真的 E2E**（見 §6） | 通過（非 E2E） |
| V02 | `events::v02_v03_since_replays_or_asks_for_a_resync`；`events_more::v01_…`（`since=0` 補送）；`events::tests::*` | 補送缺少的事件、不重複、之後接上即時事件沒有空洞 | 通過 |
| V03 | `events::v02_v03_since_replays_or_asks_for_a_resync`；`events::tests::since_replays_what_followed_and_resyncs_outside_the_ring` | `since` 比最後一筆新（服務重啟過）或比環狀緩衝舊都是 `resync`；`since=minus` 回 400 | 通過 |
| V04 | `events_more::v04_a_handshake_with_an_expired_ended_or_missing_session_is_401`；`events::v04_the_handshake_needs_a_session_and_this_origin` | 沒有 cookie、偽造的 token、閒置超過 8 小時（注入時鐘）、已登出、不存在的 session：握手 401；`Origin` 不同 403；不是升級請求 400 | 通過 |
| V05 | `events::v05_a_client_that_does_not_answer_pings_is_closed` | 不讀取（因此不回 pong）的連線被以 4002 關閉；正常讀取的連線在同一段時間內收到 ≥ 4 次 ping 且保持連線。**偏離**：用 150 ms 的真實 ping 間隔，不是注入時鐘（見 §6） | 通過 |
| V06 | `events_more::v06_100_connections_all_get_every_event_and_the_one_that_does_not_read_is_cut_off`；`events::v06_a_client_that_falls_behind_is_closed_with_4008` | 100 個連線（99 個讀取、1 個接收緩衝 4 KiB 且不讀取），2500 個 8 KiB 的事件、每 100 個一批：99 個連線各自收到 seq 1–2500 一個不缺一個不重複；慢的那個在讀到最後一筆之前就被伺服器切斷；之後新連線以 `since` 補送正常。落後超過 256 筆的連線被以 4008 關閉 | 通過 |

### 2.4 HTTP／HTTPS（H）

| ID | 測試 | 驗證方式 | 結果 |
|---|---|---|---|
| H01 | `openssl::h01_openssl_reads_an_ecdsa_p256_certificate_of_825_days_with_the_names_and_serverauth`（Docker）；`tls::tests::h01_*`；`server::h01_h07_https_with_the_self_signed_certificate_verifies_for_localhost_and_sends_hsts` | **`openssl x509 -text` 解析**：ECDSA P-256（`prime256v1`、`ecdsa-with-SHA256`）、EKU `TLS Web Server Authentication`、KeyUsage `Digital Signature`、SAN 含主機名稱、`localhost`、`127.0.0.1`、介面位址與 `tls_names`；`-checkend`：823 天後仍有效、826 天後過期；`notBefore` 是今天、`notAfter` 是 2029-01-24（825 天）；`openssl pkey` 確認私鑰是 P-256 且公鑰與憑證相同；私鑰檔權限 `600`（Docker 內 `stat -c %a`）；真實 HTTPS 連線以 `localhost` 完整驗證憑證，不信任的客戶端被拒 | 通過（Docker） |
| H02 | `tls::tests::h01_h02_*`、`h02_a_half_written_pair_is_replaced`、`h02_an_unreadable_self_signed_pair_is_made_again`；`openssl::h01_…`（第二次載入指紋相同）；`web_service::tests::h08_h01_https_without_a_certificate_makes_one_and_logs_its_fingerprint` | 第二次啟動沿用同一張（檔案位元組與指紋相同；之後的日期也不會重產）；啟動流程第二次不再記「was made」 | 通過 |
| H03 | `tls::tests::h03_configured_files_are_used_and_problems_are_named`；`web_service::tests::h03_a_missing_or_mismatched_certificate_stops_the_start_and_names_the_file` | 使用者提供的憑證被使用且不產生自簽；不成對（私鑰屬於另一張）、檔案不存在、不是 PEM 都啟動失敗，訊息含 `[web]` 與檔名；埠被占用時訊息含位址 | 通過 |
| H04 | `tls::tests::h04_*`（2 個）；`web_service::tests::h04_a_self_signed_certificate_that_ends_soon_is_replaced_with_a_warning_and_the_old_fingerprint`、`h04_a_configured_certificate_that_ends_soon_is_kept_with_a_warning` | 單元：以注入的日期驗證 30 天邊界（31 天前沿用、29 天前更換）。啟動流程：800 天前做的自簽憑證（25 天後到期）在啟動時被換掉，log 有 warning 與**舊指紋**，新指紋與磁碟上的檔案相符；使用者提供的 800 天前的憑證不換（檔案位元組不變），log 有「expires on … day(s) left」 | 通過 |
| H05 | `openssl::h05_tls_1_2_and_1_3_connect_and_1_1_and_1_0_are_refused`（Docker） | `openssl s_client`：`-tls1_3` → `TLSv1.3`、`-tls1_2` → `TLSv1.2`、不指定 → `TLSv1.3`；`-tls1_1`、`-tls1` 對我們失敗（`alert handshake failure`）。**對照**：同一個客戶端、同樣的參數對 `openssl s_server -tls1_1`／`-tls1`（另做的 RSA 憑證）可以連上（`Protocol version: TLSv1.1`／`TLSv1`），所以拒絕是我們伺服器造成的 | 通過（Docker） |
| H06 | `config::tests::h06_plain_http_off_loopback_is_warned_about`；`web_service::tests::h06_plain_http_on_a_reachable_address_warns_at_start_and_on_loopback_does_not` | `scheme = "http"` 且 bind 為 `0.0.0.0`：啟動 log 有 WARN 與「unencrypted」；bind 為 loopback：沒有任何 WARN；`enabled = false`：什麼都不啟動 | 通過 |
| H07 | `errors::h07_the_security_headers_are_on_every_answer_whatever_its_status`；`server::h07_plain_http_has_the_security_headers_and_no_hsts`、`server::h07_https_sends_hsts_with_every_status_and_the_other_headers_too`、`server::h01_h07_*` | HTTP：11 種回應（200 JSON／PNG／下載、401、404、405、403、400、413、WebSocket 拒絕）都有 CSP（與規格 §5.3 逐字相同）、`nosniff`、`Referrer-Policy: same-origin`，沒有 HSTS；HTTPS：401、404、405、fallback 都有 HSTS `max-age=31536000` 與其他三個 | 通過 |
| H08 | `openssl::h08_the_fingerprint_we_print_is_the_one_openssl_computes`（Docker）；`web_service::tests::h08_h01_https_without_a_certificate_makes_one_and_logs_its_fingerprint` | `openssl x509 -fingerprint -sha256` 與 `TlsMaterial::fingerprint` 逐字相同（兩張不同的憑證各驗一次）；啟動 log 的「certificate SHA-256 fingerprint …」與磁碟上憑證檔的 DER 指紋相同（PEM 由測試自己解碼，不經被測程式） | 通過（Docker） |

## 3. 發現並修正的問題

1. **W10：已存在的路徑用了不支援的方法，回的是空本文的 405。** 例如 `GET /api/v1/auth/login`。違反「所有錯誤都是 `{"error": {code, message}}`」。修正：`crates/web/src/error.rs` 新增 `json_errors`，在 `lib.rs::app` 以 `map_response` 套用，把框架產生的、不是 JSON 的錯誤回應（4xx／5xx）改成 API 的錯誤格式（保留 `Allow` 等 header，丟掉框架的原文）。
2. **W10：非 UTF-8 的路徑（`/api/v1/faxes/%FF`）回 axum 的純文字 400。** 同一個修正涵蓋（`bad_request`）。
3. **W07：紀錄指向 `data_dir` 之外的 TIFF 時，預覽 PNG 回 500 `internal`**（下載是 404）。沒有洩漏檔案內容，但壞掉的紀錄不該是伺服器錯誤，且 500 會觸發 error log。修正：`crates/web/src/api/faxes.rs::preview_error` 把 `PreviewError::UnsafePath` 對應成 404，並記一筆 warn。
4. **A11／A12：`PUT /users/{id}` 同時帶 `password` 與會被拒的變更時，請求回 409，但密碼已經被重設、所有 session 已被刪除。** 例如對最後一個 admin 送 `{"disabled": true, "password": "…"}`：回 409 `last_admin`，但那個 admin 的密碼被改成新的、`must_change_password` 變 true、所有 session 失效。違反「被拒的請求不改變任何東西」。修正：`crates/web/src/api/users.rs::update` 改為先檢查新密碼的規則（`accounts::password::check_rules`），再套用帳號變更（最後一個 admin 的檢查在這裡），最後才重設密碼。測試 `admin_more::a12_two_admins_may_…` 以四種組合驗證帳號與 session 都沒有被動到。
5. **V 編號與計畫不一致。** 既有的 `events.rs` 把 ping／pong 逾時命名為 `v04`、落後的連線命名為 `v05`、握手命名為沒有編號。依計畫（V04 握手、V05 ping、V06 緩衝滿）改名：`v05_a_client_that_does_not_answer_pings_is_closed`、`v06_a_client_that_falls_behind_is_closed_with_4008`、`v04_the_handshake_needs_a_session_and_this_origin`；並刪掉 `the_handshake…` 最後一行沒有意義的 `let _ = ws.send(...)`。
6. **弱斷言與空斷言。** 檢查既有的 web 測試，改為精確斷言：`admin.rs` 的 `pending_restart` 只檢查 `is_array()` → 與 `[]` 相等；`port_range` 只檢查 `is_string()` → 解出 low < high；驗證錯誤只檢查 `code.is_string()` → 等於 `reversed`；稽核的 `after.is_object()` → 內容等於 `32000-32999`；agent／未登入被拒的測試只檢查錯誤代碼是字串 → 分別等於 `forbidden`／`unauthenticated`；`/status` 的 `key.is_string()` → 與 `ConfigService::status().key` 相同；`server.rs` 明文 HTTP 打 HTTPS 埠用 `!is_ok_and(success)`（連 4xx 也算過）→ 必須是連線錯誤；W10 的 `message.is_string()` → 固定的英文訊息；`faxes.rs` W07 的預覽 `is_client_error() || is_server_error()` → 必須是 404（這個放寬讓 §3 第 3 點的 500 溜過去）。沒有發現 mock store：所有測試都是真的 SQLite；DB 斷線用的是 `runtime-config` K12 那種「在真的 store 前面注入斷線」，不是替身。
7. **H05 的對照組。** 第一版用 ECDSA 憑證與 `ALL` 密碼組當 TLS 1.1 的對照，OpenSSL 3.5（Rocky 映像）的伺服器端回 `tlsv1 alert internal error`（`invalid digest`：這個建置不提供 TLS 1.0／1.1 金鑰交換用的 MD5+SHA1 簽章），對照失敗所以測試正確地拒絕下結論。改用另做的 RSA 憑證與不需要簽章金鑰交換的 `AES128-SHA:@SECLEVEL=0` 後對照成立。
8. **本機（Windows）一次跑全部 `--ignored` 時卡住。** 在 Git Bash 的 OpenSSL 3.2 上同時跑三個 openssl 測試，H05 的對照伺服器（`openssl s_server`）讓測試掛住不結束，我沒有深究原因（疑為 Windows 上 `s_server` 的 stdin 與程序結束行為）；H01、H08 單獨在 Windows 本機可跑。H05 只在 Docker 內跑（本來就是計畫指定的環境），腳本以 `--test-threads 1` 序列執行。

## 4. 執行指令與結果

本機為 Windows 11（Git Bash）；所有 cargo 指令都加 `-j 2`（記憶體不足，之前平行建置被系統終止）。

```text
cargo fmt --all                                                   ok
cargo clippy -p web -p sip-faxd --all-targets -j 2 -- -D warnings  ok（0 個警告）
cargo test -p web -j 2                                            單元 23、整合 67 通過，3 個 ignored（openssl），0 failed
cargo test -p sip-faxd --bin sip-faxd web_service -j 2            6 passed; 0 failed
cargo llvm-cov -p web --summary-only -j 2                         見 §5
scripts/web-openssl-docker.sh                                     test result: ok. 3 passed; 0 failed（5.27s）
```

### 4.1 H01／H05／H08：Docker 內的 `openssl`

指令：

```sh
E2E_NO_BUILD=1 scripts/web-openssl-docker.sh
# = docker run --rm --platform linux/amd64 -v <repo>:/work … sip-fax-rs-test:rocky9 \
#     bash -euxc 'openssl version; cargo test -p web --test openssl -j 2 -- --ignored --nocapture --test-threads 1'
```

完整輸出（已去掉編譯訊息）在 `docs/test-reports/M4b-openssl-docker.log`。映像內是 `OpenSSL 3.5.5 27 Jan 2026`。重點：

```text
$ openssl x509 -in …/tls/self-signed.crt -noout -text
    Signature Algorithm: ecdsa-with-SHA256
    Validity   Not Before: Oct 22 00:00:00 2026 GMT   Not After : Jan 24 00:00:00 2029 GMT
    Public Key Algorithm: id-ecPublicKey   Public-Key: (256 bit)   ASN1 OID: prime256v1   NIST CURVE: P-256
    X509v3 Subject Alternative Name:
        DNS:faxsrv, DNS:localhost, IP Address:127.0.0.1, IP Address:192.0.2.10, DNS:fax.example.test, IP Address:198.51.100.4
    X509v3 Key Usage: critical   Digital Signature
    X509v3 Extended Key Usage:   TLS Web Server Authentication
$ openssl x509 -in …/self-signed.crt -noout -checkend 71107200   → Certificate will not expire   (823 天)
$ openssl x509 -in …/self-signed.crt -noout -checkend 71366400   → Certificate will expire       (826 天, exit 1)
$ stat -c %a …/self-signed.key                                    → 600

$ openssl x509 -in …/self-signed.crt -noout -fingerprint -sha256
  sha256 Fingerprint=94:91:39:E7:EC:0C:48:D8:9E:A0:9D:A8:34:65:54:D1:E5:E5:00:EA:21:21:B9:10:87:E8:05:75:E1:59:95:95
ours:   94:91:39:E7:EC:0C:48:D8:9E:A0:9D:A8:34:65:54:D1:E5:E5:00:EA:21:21:B9:10:87:E8:05:75:E1:59:95:95

# 對照：這個 openssl 能說 TLS 1.1 與 1.0（對另做的 s_server）
$ openssl s_client -connect 127.0.0.1:<control> -tls1_1 -cipher AES128-SHA:@SECLEVEL=0 -brief   → Protocol version: TLSv1.1  (exit 0)
$ openssl s_client -connect 127.0.0.1:<control> -tls1   -cipher AES128-SHA:@SECLEVEL=0 -brief   → Protocol version: TLSv1    (exit 0)
# 我們的伺服器
$ openssl s_client -connect 127.0.0.1:<ours> -tls1_3 -brief   → Protocol version: TLSv1.3   Ciphersuite: TLS_AES_256_GCM_SHA384
$ openssl s_client -connect 127.0.0.1:<ours> -tls1_2 -brief   → Protocol version: TLSv1.2   Ciphersuite: ECDHE-ECDSA-AES256-GCM-SHA384
$ openssl s_client -connect 127.0.0.1:<ours> -brief           → Protocol version: TLSv1.3
$ openssl s_client -connect 127.0.0.1:<ours> -tls1_1 -cipher AES128-SHA:@SECLEVEL=0 -brief   → alert handshake failure (exit 1)
$ openssl s_client -connect 127.0.0.1:<ours> -tls1   -cipher AES128-SHA:@SECLEVEL=0 -brief   → alert handshake failure (exit 1)
```

## 5. 涵蓋率（`cargo llvm-cov -p web --summary-only -j 2`，行涵蓋率）

| 檔案（`crates/web/src/`） | 行數 | 未涵蓋 | 行涵蓋率 |
|---|---|---|---|
| `api/auth.rs` | 126 | 6 | 95.24% |
| `api/config.rs` | 204 | 3 | 98.53% |
| `api/events.rs` | 129 | 9 | 93.02% |
| `api/faxes.rs` | 582 | 16 | 97.25% |
| `api/mod.rs` | 9 | 0 | 100.00% |
| `api/system.rs` | 35 | 2 | 94.29% |
| `api/users.rs` | 146 | 3 | 97.95% |
| `config.rs` | 90 | 1 | 98.89% |
| `error.rs` | 75 | 10 | 86.67% |
| `events.rs` | 164 | 4 | 97.56% |
| `extract.rs` | 15 | 0 | 100.00% |
| `lib.rs` | 21 | 0 | 100.00% |
| `server.rs` | 97 | 8 | 91.75% |
| `session.rs` | 113 | 11 | 90.27% |
| `state.rs` | 24 | 0 | 100.00% |
| `tls.rs` | 357 | 35 | 90.20% |
| **web 整體** | **2187** | **108** | **95.06%**（門檻 80%） |

加入本次測試前是 94.43%（119 行未涵蓋）；新增的 `error.rs::json_errors` 與 `users.rs` 的流程也已涵蓋。`error.rs` 較低是因為 `code_of` 的 415、422 等分支沒有路徑會產生（防禦用）。

計畫 §0 的門檻是 80%。`web` 的 `#[ignore]` 測試（openssl）不在這個數字內，涵蓋率來自其餘測試。

## 6. 已知限制與偏離計畫之處

- **V01 不是 Docker 內以 SpanDSP 收傳真的 E2E。** 沒有加進 `tools/fax-sim/tests/`：那一套需要 fax-sim 當 gateway、真實的 `sip-faxd serve` 與 Chromium 以外的完整 SIP 流程（約 1 分鐘，由 `scripts/e2e-docker.sh` 執行），也要在 `serve` 啟動 web。替代：真正的 `Recorder`（與 `serve` 相同的 `on_written` 對應）＋真正的 store＋真正的伺服器與 WebSocket 客戶端，驗證順序、`seq`、頁數、DB 內容（`events_more::v01_…`）；`recorder_events` 本身的對應由 `web_service::tests::v01_…` 驗證。尚未驗證的只剩「recorder 的事件由真實通話產生」這一段，那一段由 fax-in 的 `f03_*`（M4b ⑤）與 M2d／M3 的 E2E 分別涵蓋。
- **V05 沒有使用注入時鐘。** 計畫 §0 要求時間相關的邏輯要能注入時鐘。ping 間隔可以由 `AppState::ws_ping` 注入（150 ms），伺服器內部用 `tokio::time::Instant`，沒有 `Clock` 的抽象；測試用短的真實間隔（約 1 秒），沒有真的等 30／60 秒。如果要用 `start_paused`，需要把連線處理拆出來，不在本次範圍。
- **A05 的計時測試依賴牆上時間。** 50 次中位數、差異 < 20%；測試機器很忙時有可能不穩。`accounts` 內已有同樣的測試（M4a）；這份是 HTTP 版，多一層 HTTP 的雜訊。
- **A11 的「每條路由都在矩陣裡」是手動維護的清單。** `a11_the_matrix_covers_every_route_of_the_router` 比對的是測試內寫死的路由宣告清單，axum 沒有公開的路由列舉；新增路由時要同時改清單（測試註解已說明）。額外的保險：矩陣內所有路由的實際請求都做過，且未宣告的路徑回 404。
- **W01 的全文搜尋 `q`** 尚未實作（TODO 已記，store 的 `ListFilter` 沒有這個欄位），規格 §3 的篩選清單中只缺這一項；`q` 目前會被當作未知參數回 400。
- **W06 的中文檔名 RFC 5987** 只能在單元測試驗證：`download_name` 把非 ASCII 一律換成 `_`，HTTP 上看不到百分比編碼。
- **H04 在啟動流程中用真實的現在時間。** `web_service::start` 以 `Utc::now()` 判斷，不能注入；測試改用「800 天前做的憑證」讓真實時間落在 30 天內，邊界（29／31 天）的精確驗證在 `tls::tests` 以注入日期完成。
- **H05 只在 Docker 內執行**（本機 Windows 的對照伺服器會卡住，見 §3 第 8 點）。H01、H08 本機也可執行（`cargo test -p web --test openssl h01 -- --ignored --nocapture`）；`stat` 的 0600 斷言只在 unix 執行（Docker 內已驗證）。
- **V06 的慢速連線被切斷時沒有 close frame**：緩衝都滿了，伺服器寫不出去，所以客戶端看到的是連線結束；測試以「讀完緩衝後在最後一筆之前結束」斷言。慢速客戶端的偵測是靠 256 筆的落後上限與 10 秒的寫入逾時，測試機上實測在數秒內切斷。
- **用 CLI 停用帳號不會立刻關閉該帳號的 WebSocket**（M4b ⑤ 已知限制，CLI 不經過服務）；沒有測試，也不是缺口。
- A02、A13 屬於 M4a，只做對照。

## 7. CI

- `.github/workflows/ci.yml` 的 coverage 步驟新增 `cargo llvm-cov -p web --fail-under-lines 80`；`scripts/ci-local.sh` 同步新增這一行，並在有 Docker 時於 `scripts/e2e-docker.sh` 之後執行 `scripts/web-openssl-docker.sh`。
- `scripts/web-openssl-docker.sh` 是新檔，需要執行權限（`git update-index --chmod=+x scripts/web-openssl-docker.sh`）。
- GitHub CI 沒有加 openssl 的測試：它只在 Docker 映像內驗證過（OpenSSL 3.5.5）；ubuntu runner 的 OpenSSL 設定不同，H05 的 TLS 1.1 對照組可能要調整，沒有驗證就不放進必要檢查。若要放，建議在 `e2e-spandsp` job 內加一步 `scripts/web-openssl-docker.sh`（該 job 已有 Docker）。
- 本報告所有測試尚未經 GitHub CI 驗證（本機 Windows＋Docker）；其中 `events_more::v06_…` 在 Linux 上緩衝的大小不同，若 CI 上發現沒有切斷，把 `EVENTS` 或 `PAD` 調大。

## 8. 新增依賴

只有 `crates/web` 的 `[dev-dependencies]`：`async-trait`（DB 斷線的 `StoreSource` 注入）、`fax-in`（V01 用真正的 `Recorder`）。兩者都已在 workspace 內，沒有新的外部 crate，沒有新增執行期依賴；`cargo deny` 不受影響（未重新執行，只動 dev-dependencies 與 `Cargo.lock` 內的內部依賴邊）。

## 9. Opus 審核（2026-10-22）

**結論：通過。**

- **重跑結果**：`cargo test -p web` 98 個（95 個正常執行全數通過，3 個 openssl 測試由腳本執行）；`cargo llvm-cov -p web` 行涵蓋率 95.06%（門檻 80%）；`scripts/web-openssl-docker.sh` 3 個全數通過。三項都與報告一致。
- **產品程式的修正**：三處都審過並接受。`PUT /users/{id}` 那一處是 ④ 的實作錯誤（先重設密碼、再做可能被拒的帳號變更），改為先檢查密碼規則、再變更帳號、最後重設密碼。
- **抽查**：A11 的權限矩陣在每個被拒的請求前後，比對帳號、設定、稽核紀錄、傳真與投遞的完整快照，不是空斷言。W06 的檔名：下載檔名由 id 與來電號碼組成，本來就是 ASCII，`Content-Disposition` 另附 RFC 5987 形式，單元測試以中文檔名驗證，不是缺口。
- **接受的偏離**：
  - V01 以真的 `Recorder` 驅動，不是 SpanDSP 通話；在 M4c 的 Playwright E2E 補真實通話（已列入 TODO）。
  - V05 用真實的短間隔，不是注入時鐘。
  - A05 依賴牆上時間，但兩種請求交錯量測，負載對兩者的影響相近；這是計畫要求的案例，保留。若在 CI 上不穩，再改成只在 `accounts` 層量測。
- **後續**：openssl 檢查放進 GitHub CI 的 `e2e-spandsp` job（已列入 TODO）。V06 在 Linux 上的慢速連線切斷，以這次 push 的 GitHub CI 結果確認。