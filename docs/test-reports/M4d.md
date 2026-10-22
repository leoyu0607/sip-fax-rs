# M4d 測試報告：服務管理、控制 socket、log 輪替、TUI

- 實作與執行：Sonnet（服務管理的初版與 TUI）、Opus（接手整合、修正、E2E 與報告）
- 計畫：`docs/test-plans/M4.md` §0、§7 M01–M14；規格：`docs/design/m4-web.md` §7；ADR-055
- 分支：`m4d-service-management`，建立在 M4a 之上；M4a 合併進 `main`（`24f5470`）後已 rebase 到 main。
- 狀態：**Opus 審核通過（2026-10-22）**，見 §9

## 1. 範圍與交付內容

| 項目 | 位置 |
|---|---|
| `[service]`（`pid_file`、`socket`、`stop_timeout_secs`、`start_timeout_secs`）與 `[log]` 的輪替設定；相對路徑以執行檔所在目錄為準 | `apps/sip-faxd/src/service/config.rs`、`apps/sip-faxd/src/settings.rs` |
| PID 檔加 `flock`：「是否在執行」只看鎖；`start` 失敗不會留下 PID 檔 | `apps/sip-faxd/src/service/pidfile.rs` |
| 控制 socket（0600、一行一個 JSON）：`status`、`calls`、`stop`、`reload`、`subscribe`；`reload` 接到 M4a 的 `ConfigService::reload` | `apps/sip-faxd/src/service/{control,hub,protocol}.rs`、`apps/sip-faxd/src/runtime.rs`（`ConfigReloader`） |
| `start [-d]`／`stop [--force] [--timeout]`／`restart`／`status [--json]`／`logs [-f] [-n]`；systemd 偵測與 `--ignore-systemd` | `apps/sip-faxd/src/service/{cli,systemd}.rs`、`apps/sip-faxd/src/main.rs` |
| fax-in 的 drain：新的 INVITE 回 503，等進行中的通話結束，逾時或 force 時結束剩下的通話（`partial`、`shutdown`） | `crates/fax-in/src/controller.rs`（`FaxIn::drain`、`active_calls`、`RegistrationView`） |
| `serve` 的停止流程：SIGTERM、Ctrl+C、控制 socket 的 `stop` 都走同一套；第二次訊號等於 force | `apps/sip-faxd/src/main.rs` |
| log 檔：依大小或日期輪替、gzip、保留份數、注入時鐘；`logs -f` 輪替後繼續跟隨 | `apps/sip-faxd/src/service/logfile.rs` |
| TUI `sip-faxd tui`（ratatui＋crossterm）：傳真列表（未讀篩選）、詳細資料與投遞紀錄、切換已讀、編輯備註（樂觀鎖）、通話與狀態、繁中／英文 | `apps/sip-faxd/src/tui/` |
| 與 M4a 整合：`status` 的 trunk 清單取自執行中的設定；`RegistrationView` 會跟著熱套用更新；`FaxIn` 改為 `Arc`，供 M4a 的 applier 使用 | `apps/sip-faxd/src/main.rs`、`crates/fax-in/src/controller.rs` |
| 測試環境修正：E2E 把 PID 檔與 SQLite 檔放在容器內的暫存目錄（Docker Desktop 的 bind mount 上檔案鎖不會互斥） | `tools/interop/src/lib.rs` |
| 文件 | `README.md`、`README.zh-TW.md`、`conf/sip-faxd.toml.example`、`docs/field-test.md`、ADR-055 |

## 2. 案例對照表

E2E 是指 `tools/fax-sim/tests/e2e_m4d.rs`，在 Docker（Rocky 9）裡以 SpanDSP 當 gateway，對真實的 `sip-faxd` 執行。

| ID | 測試 | 驗證方式 | 結果 |
|---|---|---|---|
| M01 | E2E `m01_start_runs_in_the_background_shows_the_log_and_ctrl_c_leaves_the_service_alone`；`pidfile::m01_the_lock_writes_the_pid_excludes_a_second_holder_and_cleans_up`、`m01_a_garbage_pid_file_reads_as_none` | 背景啟動、PID 檔與鎖、`setsid`（session leader 是服務本身）、就緒後顯示 log；對前景的 `start` 送 SIGINT 後服務仍在執行 | 通過 |
| M02 | E2E `m02_start_d_returns_at_once_and_a_second_start_is_refused_with_the_pid` | `start -d` 立即返回；第二次 `start` 回非 0 並說明 PID | 通過 |
| M03 | E2E `m03_a_start_that_fails_shows_the_end_of_the_log_and_leaves_no_pid_file`、`m03_a_service_that_never_becomes_ready_is_stopped_after_the_timeout`；`control::m03_a_stale_socket_file_is_replaced_and_a_live_one_is_refused`、`m03_a_path_that_is_too_long_for_a_unix_socket_is_reported_with_the_path`；`pidfile::m03_a_stale_file_without_a_lock_is_not_a_running_service` | bind 失敗與就緒逾時：顯示 log 最後 50 行、回非 0、不留下 PID 檔 | 通過 |
| M04 | E2E `m04_stop_waits_for_the_fax_in_progress_and_refuses_new_calls`；`drain.rs::m04_a_running_fax_is_waited_for_new_invites_get_503_and_the_fax_is_complete`、`m04_drain_without_calls_returns_at_once_and_still_refuses_new_invites`；`cli::m04_the_stop_lines_say_what_is_waited_for` | 收傳真途中 stop：新 INVITE 回 503（反向斷言）；那通收完才結束；DB 為 `received`、頁數完整 | 通過 |
| M05 | E2E `m05_a_stop_that_runs_into_stop_timeout_secs_ends_the_call_and_keeps_the_pages`、`m05_stop_force_ends_the_call_at_once_and_keeps_the_pages`、`m05_the_timeout_option_overrides_the_configured_one`；`drain.rs` 的四個 `m05_*`；`hub::m05_the_first_stop_decides_the_timeout_and_a_later_force_upgrades_it` | 逾時與 `--force`：通話被結束，已收的頁面寫出，`partial`＋`shutdown`；drain 用 tokio 的暫停時鐘 | 通過 |
| M06 | E2E `m06_restart_waits_for_the_call_then_starts_a_new_process_with_the_same_settings` | 等通話結束後重新啟動，PID 改變、監聽位址相同；重啟前在 DB 改的執行設定（`[call] station_id`）和建立的登入 session，重啟後都還在（設定的 version 沒有變，表示沒有從設定檔重新匯入）；新程序可以收傳真 | 通過（session／設定的斷言是 Opus 審核時補上的，見 §3） |
| M07 | E2E `m07_status_shows_the_running_service_and_exits_3_when_there_is_none`；`hub` 的四個 `m07_*`；`control::m07_the_client_reads_replies_line_by_line`；`cli::m07_the_status_text_has_every_field`、`m07_uptimes_drop_leading_zero_units` | 執行中：PID、版本、uptime、監聽位址、通話數、trunk 註冊狀態；沒有執行時回 3 | 通過 |
| M08 | E2E `m08_under_systemd_start_stop_and_restart_point_to_systemctl_unless_overridden`；`systemd` 的五個 `m08_*`；`cli::m08_a_managed_service_is_refused_unless_told_otherwise` | 設定 `INVOCATION_ID` 時，start／stop／restart 提示 systemctl 並回非 0，不會啟動任何東西（反向斷言）；`--ignore-systemd` 可略過 | 通過 |
| M09 | E2E `m09_sigterm_to_serve_is_the_same_stop_and_a_second_signal_forces_it`、`m09_sigterm_waits_for_the_fax_which_then_completes`；`hub::m09_a_stop_request_is_announced_streamed_and_ended_with_stopped`；`control::m09_a_stop_over_the_real_socket_ends_with_stopped_and_shutdown_waits_for_it` | SIGTERM 與 stop 走同一套流程；第二次訊號等於 force | 通過 |
| M10 | E2E `m10_the_control_socket_is_0600_and_another_uid_cannot_connect`；`control::m10_the_socket_is_mode_0600_and_answers_status_even_under_a_permissive_umask` | 權限 0600（umask 寬鬆時也是）；Docker 中以另一個 uid 連線被拒（反向斷言） | 通過 |
| M11 | `logfile` 的八個 `m11_*`；E2E 的 `m12_…_and_m11_the_old_files_are_valid_gzip` | 依大小、依日期、保留份數、gzip；注入時鐘；多個 writer 同時寫時不遺失、不切斷任何一行；舊檔以 `gzip -t` 驗證（獨立工具） | 通過 |
| M12 | `logfile` 的六個 `m12_*`；E2E `m12_logs_f_keeps_following_across_rotations_and_m11_the_old_files_are_valid_gzip` | `logs -f` 在輪替後繼續跟隨新檔，每一行恰好出現一次 | 通過 |
| M13 | `tui/tests.rs` 的 18 個 `m13_*`、`tui/i18n.rs` 的 2 個 `m13_*` | ratatui `TestBackend` 比對畫面；真的 SQLite store（不 mock）；即時事件有一個案例走真的 `Hub`＋控制 socket；已讀與備註操作後檢查 DB（`read_by = cli:<帳號>`）；同時編輯備註時被拒，DB 保留另一筆修改（反向斷言）；沒有 socket 時顯示「服務未執行」但 DB 功能正常；`LANG=zh_TW` 顯示中文 | 通過 |
| M14 | M4a：`tests/m4a_cli.rs::m14_fax_commands_show_the_database_and_record_cli_as_the_operator`、`runtime::m14_the_operator_is_cli_and_the_os_user`、`store admin_tests::m14_redeliver_body` | 見 `docs/test-reports/M4a.md` | 通過（M4a） |

另有 `m00_*`（設定解析、協定編解碼、錯誤設定回 2）作為基礎單元測試。

## 3. 審核時發現並修正的問題

1. **Docker E2E 11 個失敗 → 測試環境的問題。** 第一輪 `e2e_m4d` 是 4 過 11 敗，症狀是 `start` 成功後 `status` 卻說沒有在執行，第二次 `start` 也能啟動。在 rocky9 映像裡實測：bind mount（`/work`）上，另一個程序可以拿到同一個檔案的 `flock`；容器內的 `/tmp` 則正確拒絕。所以把 E2E 的 PID 檔改放在容器內的暫存目錄（和 socket、log 一樣）。修正後 15 個全部通過。產品文件也寫明 `run/` 必須放在本機檔案系統（README、ADR-055）。
2. **M4a 的 `m4a_cli` 測試共用預設的 `run/`。** 加入 PID 鎖之後，同時啟動 `serve` 的測試會互相擋住（「already running」）。改成每個測試使用自己的 PID 檔與 socket。
3. **投遞 catch-up 要等 10 秒才送出。**（這項修正在 M4a 審核時一併放進 M4a，見 `docs/test-reports/M4a.md` §10。） `DeliveryEngine::spawn` 第一次 `run_due` 要等一個 `poll_interval`，所以 f22 會在負載高時逾時。worker 啟動時先執行一次 `run_due`，`delivery_cli` 從 10.9 秒降到 1.1 秒。
4. **f11 的競態（M3f 就有）。** 投遞失敗時，引擎刻意先寫重試的那一列，再把原本的那一列標成 `failed`；測試只等「有兩列」，在 `llvm-cov` 較慢的環境下會看到 `pending`。改成等到第一列已經結束。
5. **`reload` 沒有接上。** 初版的控制 socket 回「not available」；現在接到 `ConfigService::reload`，回覆會說明哪些 trunk 變更、哪些註冊重啟、哪些設定要等重啟。
6. **M06 沒有檢查設定與 session。** 原本只比對監聽位址，已補上 §2 所述的斷言。
7. **E2E 的 SQLite 檔也要離開 bind mount。** 補上 M06 的斷言後（測試程序也寫入 DB），重啟後那通傳真的列偶爾看不到：新程序確實寫入了，測試的連線卻讀不到。SQLite 判斷「最後一條連線」與 WAL 的共用也依賴檔案鎖，而 bind mount 上的鎖無效（同第 1 點）。把 interop 預設的 `fax.db` 移到容器內的暫存目錄後，連續執行都通過。GitHub CI 在原生 Linux 上執行，沒有這個問題。
8. **M4a 留下的 E2E 失敗。**（這項修正在 M4a 審核時一併放進 M4a。） `e2e_m3b::i05` 仍預期 schema 版本 1；M4a 新增 migration 0002 後應為 `0 -> 2 (applied 0001, 0002)`。這個測試只在 Docker 裡跑，M4a 的 CI（`6e676d1`）同樣是失敗的。

## 4. 執行指令與結果

本機為 macOS；t4 的 oracle 測試需要 libtiff／poppler，在本機以 `T4_ORACLE=skip` 略過，在 Docker 裡實際執行。

```text
cargo fmt --all --check                                   ok
cargo clippy --workspace --all-targets -- -D warnings     ok
cargo deny check                                          advisories ok, bans ok, licenses ok, sources ok
T4_ORACLE=skip cargo test --workspace --all-targets       67 個測試執行檔全部通過，0 failed
cargo test -p sip-faxd --bin sip-faxd m13                 20 passed; 0 failed
E2E_ONLY="--test e2e_m4d" scripts/e2e-docker.sh           test result: ok. 15 passed; 0 failed（160.56s）
```

`scripts/ci-local.sh` 的完整結果見 §7。

## 5. 涵蓋率（`cargo llvm-cov`，行涵蓋率）

| 範圍 | 行涵蓋率 |
|---|---|
| `fax-in`（門檻 70%） | 93.39% |
| `sip-faxd` 整體 | 80.24% |
| `service/config.rs` | 100.00% |
| `service/control.rs` | 96.53% |
| `service/hub.rs` | 93.36% |
| `service/logfile.rs` | 97.50% |
| `service/protocol.rs` | 97.44% |
| `service/systemd.rs` | 86.15% |
| `service/pidfile.rs` | 83.50% |
| `service/cli.rs` | 30.32% |
| `service/mod.rs` | 44.44% |
| `tui/app.rs` | 90.00% |
| `tui/ui.rs` | 97.60% |
| `tui/i18n.rs` | 95.10% |
| `tui/mod.rs` | 26.25% |

`service/cli.rs`（start／stop／restart／logs 的子程序控制）、`service/mod.rs`（訊號）與 `tui/mod.rs`（終端機迴圈）主要由 Docker E2E 或互動式終端機執行，不在本機的 llvm-cov 量測範圍內；其中的邏輯已盡量抽成可單元測試的函式（`format_status`、`stopping_line`、`managed_by_systemd`、`App`、`draw`）。

## 6. 已知限制與偏離計畫之處

- **M14** 由 M4a 實作與測試，本報告只做對照。
- **TUI 的資料來源**：傳真資料直接讀寫 DB（與 `fax` CLI 相同，操作者 `cli:<作業系統帳號>`），控制 socket 只提供狀態、通話與即時事件。規格 §7.3 寫「透過控制 socket 連線」；這樣做的好處是服務沒有執行時 TUI 仍可用，而且不需要在 socket 上重做一套傳真 API。除了收到事件時重新查詢，TUI 也每 2 秒重新整理一次。
- **TUI 沒有測試終端機本身**（raw mode、按鍵執行緒）；畫面與狀態以 `TestBackend` 測試。
- **中文寬字元**：`TestBackend` 會在寬字元後留空白格，所以中文標籤的測試是去掉所有空白後比對文字。
- **服務指令與 TUI 只支援 unix。** 本機的 macOS 可以執行，但 macOS 的 unix socket 路徑上限是 104 bytes，過長時會清楚報錯（M03 有測試）。
- **`flock` 的檔案系統限制**：見 §3 第 1 點；已寫入 README 與 ADR-055。

## 7. `scripts/ci-local.sh` 與 GitHub CI

- `scripts/ci-local.sh`（macOS＋Docker Desktop）：fmt、clippy、`cargo deny`、workspace 測試、涵蓋率門檻、musl 靜態連結（`statically linked`）都通過；第一次完整執行時 E2E 的 M06 失敗，原因與修正見 §3 第 7 點。修正後重跑完整的 `scripts/e2e-docker.sh`：e2e_m2d 10、e2e_m3b 6、e2e_m3d 5、e2e_m3f 4、e2e_m4d 15、e_ecm 11、m0 10、x_interop 12 等全部通過；interop 的 `sipp::s02` 一次在建置 `sip-faxd` 時出現 `can't find crate`（bind mount 上的建置 I/O，測試尚未開始），單獨重跑 16 passed。
- GitHub CI（`4feccc0`，run 39691358914）：lint、test、coverage、e2e-spandsp、db-matrix（postgres、mysql、mariadb、mssql）、build-musl（x86_64、aarch64）、smoke-rhel（Rocky 9、AlmaLinux 10）全部 success。

## 8. 新增依賴

`rustix` 1（`fs`、`process`）、`flate2` 1（`rust_backend`）、`ratatui` 0.30（`default-features = false`，只開 `crossterm`）、`crossterm` 0.29；全部是純 Rust，`cargo deny` 通過。ADR-055。

## 9. Opus 審核（2026-10-22）

**結論：通過。** 計畫 §7 的 M01–M13 都有對應測試，M14 由 M4a 涵蓋；安全相關的案例（M08 被拒時不會啟動任何東西、M10 另一個 uid 連線被拒、M04 新 INVITE 回 503、M13 同時編輯備註被拒）都有反向斷言；沒有 mock store；log 的 gzip 和收到的頁面都以獨立工具（`gzip -t`、`tiffinfo`、`pdfinfo`）驗證。

**注意**：這次 M4d 大部分的整合與修正是 Opus 接手完成的（§3），審核也是 Opus 做的，所以不是完全獨立的第二雙眼睛。為了彌補，審核時以腳本逐一檢查斷言，並逐項對照計畫的「預期」欄。

**檢查方式**
- 以腳本掃描 M4d 新增或修改的 126 個測試：沒有空斷言。只有一個斷言的 8 個測試逐一看過，每個都只驗證一件事，而且斷言正好對應那件事。
- 逐項對照計畫 §7 的「預期」欄：M03 有斷言「log 最後 50 行」、不留 PID 檔與 socket；M04 有斷言 503（兩次）、`received`、頁數、磁碟上的頁數；M05 有斷言 `partial`、`shutdown`、`system`、檔案大小；M06 補上了設定與 session 的斷言（§3 第 6 點）。

**審核時發現並已修正**
1. **`RegistrationView` 沒有測試。** `status` 的 trunk 註冊狀態就是從它取得的，而且 rebase 時改成跟著熱套用更新，E2E 只測了不註冊的 trunk。新增 `hot_apply.rs::m07_the_registration_view_follows_apply_config_and_outlives_the_controller`：註冊成功、關閉、換帳號重新開啟、關閉 controller 之後都正確。
2. **TUI 會因 DB 連線而凍結。** 主迴圈直接 `await` 連線；伺服器型 DB 停掉時，每次嘗試要等 10 秒的 acquire timeout，期間按鍵沒有反應。測試只用 SQLite，所以沒抓到。改為背景 task 連線，成功後再交給主迴圈；每 2 秒的重新整理加上逾時。新增 `m13_the_database_is_connected_in_the_background_and_handed_over_once_it_answers`。
3. **f11 的修正沒有進到 main。** M4a 合併後 main 的 CI 在 f11 失敗（同 §3 第 4 點的競態）；已把修正 cherry-pick 到 main（`88ce22a`）。

**保留為已知限制**：§6 所列。
