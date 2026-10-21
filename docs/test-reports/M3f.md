# M3f 測試報告：delivery 接進 sip-faxd

- 實作與執行：Sonnet（M3f）
- 計畫：`docs/test-plans/M3.md` 第 5 節（整合）、第 6 節（投遞）；規格：`docs/design/m3-store.md` §9、§11；接線說明：`docs/test-reports/M3c.md` §11
- 狀態：**待 Opus 審核**。已 commit 在分支 `worktree-agent-a99da9d126bdb86b0`（基於 `m3`），沒有 push。
- 沒有碰 `docs/TODO.md`、`docs/ROADMAP.md`、`docs/DECISIONS.md`。

## 1. 交付內容
| 項目 | 位置 |
|---|---|
| fax-in 的「final 寫入成功」掛鉤 `RecorderConfig.on_finalized`（含 write queue 重試成功後；放棄的寫入與 `backfill` 不觸發） | `crates/fax-in/src/recorder.rs` |
| 投遞服務：背景任務擁有 `DeliveryEngine`；等資料庫可用 → `catch_up` → `spawn`；轉送 recorder 的通知；關閉時補 dispatch 並限時等待 | `apps/sip-faxd/src/delivery_service.rs` |
| `serve` 接線（有 `[[routes]]` 才啟動；關閉順序：fax-in → recorder 清佇列 → 投遞 → 關資料庫） | `apps/sip-faxd/src/main.rs` |
| 設定：頂層 `[[routes]]`（delivery crate 型別）、`[delivery]`（`catch_up_hours` 預設 24、`max_attempts` 預設 10）；`Routes::validate()` 失敗時 `config check` 與 `serve` 以結束碼 2 拒絕 | `apps/sip-faxd/src/settings.rs` |
| `config check` 路由摘要（密碼、HMAC secret、URL 帳密與 query 值遮蔽為 `***`） | `apps/sip-faxd/src/commands.rs`（`routes_summary`）、`settings.rs`（`mask_http_url`） |
| 範例設定、兩份 README、`docs/database.md`、`CHANGELOG.md` | `conf/sip-faxd.toml.example`、`README.md`、`README.zh-TW.md`、`docs/database.md`、`CHANGELOG.md` |
| E2E | `tools/fax-sim/tests/e2e_m3f.rs`、`e2e_common::faxd_routes`、`Server::stop` 回傳 log |
| 腳本：`scripts/e2e-docker.sh`、`scripts/musl-docker.sh` 新增 `E2E_NO_BUILD=1`（不連 registry，直接用本機映像）；`E2E_IMAGE` 可指定映像 | `scripts/` |

### 1.1 設計判斷
- **backfill 不自動投遞。** `db backfill` 是離線指令，沒有 engine；補回舊紀錄不應灌爆信箱。但補回的通話若在 `catch_up_hours` 內，下一次 `serve` 啟動的 `catch_up` 會投遞它（更舊的不會）。`catch_up_hours = 0` 可完全關閉。F04 證明 backfill 本身不建立任何 `delivery_attempts`；F13 證明下次啟動才補送。
- **資料庫不可用不影響收傳真。** 投遞服務自己等資料庫（每 10 秒重試），期間 recorder 的通知留在無界 channel；起來後 `catch_up` 與 channel 轉送都會跑，dispatch 對 `(fax, target)` 冪等，不會重複。F15 證明連不上資料庫時關閉不會卡住。
- **掛鉤只送 id、不阻塞。** 投遞失敗只寫 `delivery_attempts`，不改 `faxes`（F11）。
- **{date}/{time} 使用伺服器本地時區**（啟動時取偏移量；夏令時間變更需重啟才更新）。
- 關閉時進行中的嘗試最多等 15 秒；超時的嘗試維持 `pending`，下次啟動繼續（at-least-once）。

## 2. 指令
```sh
cargo fmt --all --check && cargo fmt --manifest-path tools/Cargo.toml --all --check
cargo clippy --workspace --all-targets -- -D warnings
SMTP 容器：scripts/smtp-test-servers.sh up   （環境變數見該腳本輸出）
T4_ORACLE=skip cargo test --workspace --all-targets
cargo deny check
cargo llvm-cov -p fax-in --fail-under-lines 70
cargo llvm-cov -p delivery --fail-under-lines 80
E2E_NO_BUILD=1 E2E_IMAGE=sip-fax-rs-test:rocky9-m3e scripts/e2e-docker.sh      # 完整（fmt、clippy、tools 全部測試、oracle）
E2E_NO_BUILD=1 scripts/musl-docker.sh
```
本機 Windows 11；本機當時連不上 Docker Hub，所以兩個腳本以 `E2E_NO_BUILD=1` 使用本機既有映像（`rocky9-m3e` 含 netpbm）。

## 3. 案例結果
所有測試都用真正的 SQLite store（不 mock）。時間邏輯沒有 sleep；背景 worker 的結果以輪詢檔案／資料表等待（上限 15 秒）。

| ID | 案例（測試名稱開頭） | 位置 | 結果 |
|---|---|---|---|
| F01（fax-in） | `f01_the_hook_gets_the_row_id_...`：final 之前不通知，final 寫入後恰好通知一次、id 等於資料列 id | `crates/fax-in/tests/recorder.rs` | 通過 |
| F02（fax-in） | `f02_the_hook_fires_after_a_final_write_that_needed_a_retry` | 同上 | 通過 |
| F03（fax-in） | `f03_the_hook_is_not_called_for_a_final_write_that_was_given_up_on` | 同上 | 通過 |
| F04 | `f04_backfill_creates_no_delivery_attempts` | 同上 | 通過 |
| F01–F04（設定） | `f01_...defaults`、`f02_routes_and_the_delivery_section_...`（三種 kind）、`f03_invalid_routes_stop_the_start_and_name_every_problem`（預設路由不在最後、無 deliveries、未知欄位／kind、`max_attempts = 0`）、`f04_webhook_urls_are_masked_for_display` | `apps/sip-faxd/src/settings.rs` | 通過 |
| F05 | `f05_the_route_summary_lists_everything_and_hides_every_secret`（email 密碼、URL 帳密、query token、HMAC secret 都不出現） | `apps/sip-faxd/src/commands.rs` | 通過 |
| F06 | `f06_the_routes_in_the_example_file_are_valid_once_uncommented`（範例檔內被註解的路由取消註解後通過 validate；範例檔本身不投遞） | `settings.rs` | 通過 |
| F10 | `f10_a_finalized_call_is_delivered_...`：Started＋Finished 事件 → recorder → 掛鉤 → engine → 暫存資料夾出現 PDF 與 TIFF（bytes 與來源相同、檔名 `fax-<id>`），`delivery_attempts` 為 `done`／attempt 1／無錯誤，紀錄不變 | `apps/sip-faxd/src/delivery_service.rs` | 通過 |
| F11 | `f11_a_failing_delivery_neither_touches_the_record_nor_the_recorder`：目的目錄不存在且 `create_dir = false` → 第 1 列 `failed`＋錯誤訊息、第 2 列 `pending`（attempt 2、`next_try_at` 在未來）；紀錄仍 `received`；recorder 的 failed／dropped／spilled 皆 0 | 同上 | 通過 |
| F12 | `f12_partial_faxes_follow_deliver_partial_...`：partial 在 `deliver_partial=false` 的路由不投遞（沒有任何 attempt 列、目錄不存在）、在 `true` 的路由投遞；`failed` 不投遞 | 同上 | 通過 |
| F13 | `f13_catch_up_delivers_what_was_stored_while_the_engine_was_not_running`：沒有掛鉤時收下的傳真，重啟後補送；再重啟一次不重複（1 列、2 個檔案） | 同上 | 通過 |
| F14 | `f14_catch_up_hours_zero_...`：關閉補送時不補、但通知仍會投遞 | 同上 | 通過 |
| F15 | `f15_stopping_while_the_database_is_unreachable_does_not_hang`：資料庫連不上時關閉在 5 秒內返回、什麼都沒投遞 | 同上 | 通過 |
| F20 | `f20_config_check_prints_the_routes_and_hides_every_secret`（真實執行檔；stdout／stderr 皆無秘密；沒有路由時顯示 `routes: none`） | `apps/sip-faxd/tests/delivery_cli.rs` | 通過 |
| F21 | `f21_config_check_and_serve_refuse_broken_routes_...`：結束碼 2、列出全部問題、stdout 空、不含密碼 | 同上 | 通過 |
| F22 | `f22_serve_delivers_a_fax_an_earlier_run_stored_...`：真實 `serve` 啟動後補送 2 小時前的傳真（PDF／TIFF bytes 相同、attempt `done`）；48 小時前的不在補送範圍 | 同上 | 通過 |
| F30（E2E） | `f30_a_two_page_fax_is_delivered_to_the_folder_byte_for_byte`：真實 sip-faxd＋SpanDSP 收 2 頁；目的地 PDF／TIFF 與 `data_dir` 內檔案 bytes 相同、PDF 大小等於紀錄、attempt `done`、沒有 `.part`；SIGTERM 後 log 有 `delivery engine running`、`shutting down`，沒有 `still running` 與 ERROR | `tools/fax-sim/tests/e2e_m3f.rs` | 通過 |
| F31（E2E） | `f31_a_partial_fax_is_not_delivered_...`：中途掛斷（partial），`deliver_partial=false`：log 有「status not delivered by route」、沒有 attempt 列、目的地空 | 同上 | 通過 |
| F32（E2E） | `f32_a_partial_fax_is_delivered_when_the_route_asks_for_it`：同一情境 `true`：投遞，bytes 與 `data_dir` 相同（F31 的正對照） | 同上 | 通過 |
| F33（E2E） | `f33_a_webhook_gets_the_record_a_valid_signature_and_the_pdf`：測試內的 HTTP server 收到一次 POST；簽章以測試獨立計算的 HMAC-SHA256 比對（並與另一把 key 的結果確認不同）；JSON 欄位（id、call_id、event、status、pages、ani、dnis、TSI、pdf_size）；multipart 的 PDF 與 `data_dir` 檔案 bytes 相同；`target` 不含 query | 同上 | 通過 |

測試數量：`T4_ORACLE=skip cargo test --workspace --all-targets`（含 3 個 SMTP 容器）**802 個通過、0 失敗**；`scripts/e2e-docker.sh` 完整執行（fmt／clippy／tools 全部測試／`oracle`＋`png_oracle`）**99 個通過、0 失敗**，結束碼 0。

## 4. 涵蓋率（`cargo llvm-cov --summary-only`）
| crate | 行涵蓋率 | 門檻 |
|---|---|---|
| fax-in | **94.87%**（3841 行，漏 197） | 70 |
| delivery | **95.91%**（1713 行，漏 70；本次含 SMTP 容器） | 80 |

## 5. 靜態連結（musl，`scripts/musl-docker.sh`，zig 0.14.1、Rocky 9、x86_64）
- `sip-faxd` 連結 lettre（ring）與 reqwest（aws-lc-rs）之後：**`statically linked, stripped`**（`file` 檢查通過）；store 測試執行檔同樣靜態且在容器內 49 個測試全過。
- 大小：M3b 的 commit `6cbbb57`（同一腳本、同一工具鏈）13,139,616 bytes（約 12.5 MiB）→ 本次 **14,542,048 bytes（約 13.9 MiB）**，增加 1,402,432 bytes（約 1.34 MiB，+10.7%），來源為 lettre、reqwest、hyper、rustls 與 HMAC。
- aarch64 未建置（報告範圍只要求 x86_64）。

## 6. `cargo deny check`
`advisories ok, bans ok, licenses ok, sources ok`（沒有新增相依需要改 `deny.toml`；`tools/` workspace 新增的 dev-dependency `hmac`、`sha2` 只用在測試）。

## 7. 過程中的失敗與根因
1. F11 第一版用「目的地是一般檔案」製造失敗，但 delivery crate 把它判為**永久**失敗（不重試），與測試想驗證的「重試排程」不符；改用 `create_dir = false` 且目錄不存在（暫時性失敗）。
2. F15 第一版用「SQLite 檔案在不存在的目錄」製造連不上，但 store 會自動建立上層目錄而連線成功；改用 `postgres://…@127.0.0.1:1/`（拒絕連線）。
3. F02（fax-in）第一版同時消耗了 INSERT 的重試次數（進入 degraded，之後只給一次嘗試），final 因此被放棄；改成只讓 final 的第一次嘗試失敗。
4. E2E 第一次編譯失敗：`Server::stop` 回傳 `()`；改為回傳 log（既有呼叫端不受影響）。
5. 本機一度連不上 Docker Hub（`docker build` 解析 `rockylinux/rockylinux:9` 逾時），腳本新增 `E2E_NO_BUILD`／`E2E_IMAGE`，改用本機映像執行；之後所有 Docker 步驟都成功。
6. 這個環境中 heredoc 加 `cargo` 的複合指令會被拒絕，改用 Write 工具建檔再串接（工具問題，非程式問題）。

## 8. 對規格的偏離與補充
| 項目 | 說明 |
|---|---|
| 新增 `[delivery]` 區段 | 規格沒有；`catch_up_hours`（預設 24）與 `max_attempts`（預設 10）。理由：補送範圍與嘗試次數需要可設定，且 `catch_up_hours = 0` 可以關閉 backfill 後的補送。 |
| 沒有路由就不啟動投遞 | 啟動時記 info。之後加上路由並重啟，`catch_up_hours` 內未投遞的傳真會被補送。 |
| 投遞服務不跟 `Db` 的重連共用鎖以外的狀態 | 資料庫起來後取得一次 `Arc<dyn FaxStore>`，之後由連線池自己重連。 |

## 9. 未驗證／未解事項
1. **aarch64 musl 沒有建置**。x86_64 的 reqwest（`rustls-platform-verifier`）在 RHEL 9／10 實機上的信任庫行為（尤其內部 CA）沒有實測；E2E 只用 http 的 webhook，HTTPS webhook 與 STARTTLS／SMTPS 只在 delivery crate 層級（M3c）驗證。
2. **正常關閉時間**：Windows 只驗證了 `kill`（F22）；SIGTERM 的優雅關閉由 Linux 的 F30 驗證（log 檢查），沒有測「關閉時恰好有嘗試在執行」的情況（15 秒寬限的超時路徑沒有測試）。
3. **`{date}`／`{time}` 的時區**使用啟動時的本地偏移，沒有針對非 UTC 時區做 E2E（容器是 UTC）。
4. 連線池重連：投遞服務只在啟動取一次 store；資料庫中途中斷時依賴 sqlx 連線池自動重連，沒有針對「服務跑到一半資料庫重啟」寫測試（M3b 的 recorder 有 S11 覆蓋寫入端）。
5. CI（`.github/workflows/ci.yml`）沒有改，也沒有在 GitHub 上跑過；`scripts/ci-local.sh` 沒有整體執行（單獨執行了其中各步驟）。
6. M3c 未解事項 #4（永久失敗不會自動恢復）、#7（at-least-once）、#9（多實例）仍然成立，README 已說明前兩項。

## 10. 異動檔案
- 新增：`apps/sip-faxd/src/delivery_service.rs`、`apps/sip-faxd/tests/delivery_cli.rs`、`tools/fax-sim/tests/e2e_m3f.rs`、`docs/test-reports/M3f.md`
- 修改：`apps/sip-faxd/{Cargo.toml,src/main.rs,src/settings.rs,src/commands.rs}`、`crates/fax-in/{src/recorder.rs,tests/recorder.rs}`、`conf/sip-faxd.toml.example`、`README.md`、`README.zh-TW.md`、`docs/database.md`、`CHANGELOG.md`、`tools/fax-sim/{Cargo.toml,tests/e2e_common/mod.rs}`、`tools/Cargo.lock`、`Cargo.lock`、`scripts/e2e-docker.sh`、`scripts/musl-docker.sh`
