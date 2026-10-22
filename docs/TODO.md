# TODO

角色說明：
- **[Opus]**：規劃、撰寫測試計畫、審核
- **[Sonnet]**：依計畫實作、執行測試
- **[Any]**：不限

完成時把 `[ ]` 改為 `[x]`，並附上 commit 或 PR 連結。每個里程碑都要依 CLAUDE.md 的測試治理流程收尾。

## M0 — 基礎建設
- [x] [Any] 初始化 git、workspace 骨架（9 個 crate 加 `apps/sip-faxd`）
- [x] [Any] README.md（英文）和 README.zh-TW.md，互相連結
- [x] [Any] 交接文件：CLAUDE.md、docs/GOALS、ARCHITECTURE、ROADMAP、TODO、DECISIONS、sipress-extraction
- [x] [Any] CI：lint（fmt/clippy/cargo-deny）、test、build-musl（x86_64/aarch64，驗證靜態連結）、smoke-rhel（Rocky 9/Alma 10）、tag 時自動 release
- [x] [Any] `scripts/ci-local.sh`、`scripts/package.sh`
- [x] [Any] 建立 GitHub repo，push 後確認 CI 全綠
- [x] [Any] 設定 `main` 的 branch protection，把 CI job 設為必要檢查（`scripts/set-branch-protection.sh`；需由有管理員權限的人執行）
- [x] [Any] `docker/test-rocky9.Dockerfile`：Rust toolchain、SpanDSP v3.1.1（從原始碼建置，見 ADR-007）、libtiff-devel
- [x] [Sonnet] `tools/fax-sim` 雛形：`spandsp-sys`（手寫 FFI，見 ADR-007）綁定、T.38/音訊 loopback、TIFF 產生器；測試 S01–S10 全數通過（見 docs/test-reports/M0.md）
- [x] [Any] CI 新增 `e2e-spandsp` job，在 Rocky 9 容器中執行（GitHub CI 已綠燈）
- [x] [Opus] 撰寫 docs/test-plans/M0.md，並審核 docs/test-reports/M0.md（通過，見報告第 6 節）

## M1 — sip-core
- [ ] [Opus] 撰寫 docs/test-plans/M1.md：涵蓋 RFC 3261/3264/4475 合規條目、重送和計時器、re-INVITE 和 glare、Record-Route
- [x] [Sonnet] 抽取 sipress 模組：net.rs、take_frame、DigestChallenge（M1a 完成，已記錄到 sipress-extraction.md；測試報告見 docs/test-reports/M1.md）
- [ ] [Sonnet] 抽取 sipress 的 G.711 與 RtpPacket（並修正 decode）（M1/M5）
- [x] [Sonnet] 訊息模型與 parser（compact、多值、folding、Route/Record-Route、嚴格 Content-Length），加上 proptest 和 fuzz 目標（M1a；P01–P11 通過，P12 fuzz 目標已建立並本機各跑 60 秒；待 Opus 審核）
- [x] [Sonnet] Transport：UDP 監聽（rport）、TCP listener 與連線池、keep-alive、來源 IP 白名單（M1b；N01–N07、C01 通過；待 Opus 審核）
- [x] [Sonnet] Transaction layer：INVITE/non-INVITE server、client（Timer A/B/E/F/G/H/I/J/K、RFC 6026 L/M），用 tokio 的暫停時間做確定性測試（M1b；T01–T13 通過；待 Opus 審核）
- [x] [Sonnet] Registration client：REGISTER、digest、到期前刷新、指數退避、狀態 watch（M1b；C03、C04 通過；SIPp registrar 測試留待 M1c；待 Opus 審核）
- [x] [Sonnet] （M1c）Dialog：UAS、in-dialog UAC（re-INVITE、BYE）、491、session timer、OPTIONS、401/407 重送、Endpoint API、訊息 tap（G01–G17、E01–E06、C04b、C05、C10 通過；待 Opus 審核，見 docs/test-reports/M1.md「M1c」）
- [x] [Sonnet] SDP 模型：audio 加 `m=image udptl t38`，含全部 T38Fax* 屬性（M1a；D01–D07 通過；待 Opus 審核）
- [x] [Sonnet] （M1d）fax-sim 的 SIP UAC 模式（`fax_sim::sip_uac`＋`sip-uac` bin）：INVITE、選擇由誰發起 re-INVITE、BYE，對 `sip-faxd serve` 每個方向一個測試。SIPp 測試改直接對真實 `sip-faxd` 進行，因此未做 `uas-test`（見 M1 報告 M1d 偏離）
- [x] [Sonnet] （M1d）fax-in 來電策略骨架：trunk profile（`generic`／`cht-cs210`，來源 IP 對應）、`t38_policy`（auto／wait-peer／initiate／audio-only）、audio+image 選擇、SDP 寬容度旗標（C02、C06–C09，另有 `sip-faxd serve`／`config check`、設定檔與 conf/sip-faxd.toml.example；待 Opus 審核）
- [x] [Sonnet] （M1d）每通來電的診斷擷取：以 `MessageTap` 寫入 `logs/calls/<call-id>/sip.log`，可設定只在失敗時保留（C11；待 Opus 審核）
- [x] [Sonnet] （M1d）SIPp 互通 S01–S11（S11：我方 re-INVITE 的 200 在收到 ACK 前重送兩次；另含 C03/C04 的 SIPp registrar 版本；待 Opus 審核，見 docs/test-reports/M1.md「M1d」）
- [x] [Any] CI 新增涵蓋率門檻（`coverage` job，`cargo llvm-cov -p sip-core --fail-under-lines 85`；M1d 加上 `-p fax-in --fail-under-lines 70`）與 nightly fuzz workflow（`.github/workflows/fuzz.yml`，不在 push 觸發）；`scripts/branch-protection.json` 已加入 `coverage`，需由管理員重新套用（`scripts/set-branch-protection.sh`）
- [x] [Opus] 審核 docs/test-reports/M1.md（M1a–M1d 全數通過）

## M2 — T.38 / T.30 / T.4（non-ECM）
- [x] [Sonnet] 媒體埠改為 port_range 依通話配置（M1d 審核 #1，M2a）
- [x] [Sonnet] sip-core：re-INVITE 的 200 與 BYE 同時到達的競態，200 已收到就回報 Accepted（M1d 審核 #2，M2a，ADR-016）
- [x] [Opus] 撰寫 docs/test-plans/M2.md
- [x] [Sonnet] t38：IFP PER codec（v0–v3）、UDPTL redundancy 和 parity FEC，加上 proptest 和 fuzz（M2a）
- [x] [Sonnet] t30：接收端狀態機（DIS/DCS/TCF/CFR/FTT/MCF/RTN/MPS/EOP/DCN），以表格驅動測試（M2c；R01–R10 通過，涵蓋率見 docs/test-reports/M2c.md；待 Opus 審核）
- [x] [Sonnet] fax-in：sans-IO T.38 接收引擎 `fax_in::t38engine`（虛擬時鐘、pacing、重複指示、UDPTL 薄包裝；M2c）
- [x] [Sonnet] t4：MH/MR/MMR 解碼驗證、TIFF Class F 寫出、PDF 輸出（M2b）
- [x] [Sonnet] 第 2 層互通測試：與 SpanDSP `t38_terminal` 以 IFP 互打（M2c X01–X05，Docker；逐像素比對每一頁）
- [x] [Sonnet] fax-sim 加入 UDPTL redundancy/FEC 模型，補上 ECM + 封包遺失案例（多個 seed）（M0 審核追蹤 #1，M2a X06）
- [x] [Sonnet] 以 t4 解碼器逐像素比對收到的頁面（M0 審核追蹤 #2；M2c 的 X 測試已做；SIP 端到端版本 M2d 完成，且改用 libtiff／poppler 兩個獨立來源比對）
- [x] [Sonnet] fax-in：串起 SIP 到 T.38、T.30 再到文件，並完成端到端多頁接收（M2d：`t38session`、TIFF/PDF 輸出、`udptl.log`、fax-sim 即時 gateway；E01–E07 通過，見 docs/test-reports/M2d.md；待 Opus 審核）
- [x] [Opus] 審核 docs/test-reports/M2a–M2d.md（全數通過）

- [x] [Sonnet] 調查 sip-core 涵蓋率一次掉到 83.5% 的不穩定測試（M2a 審核；M2c 結論見報告第 6 節）
- [x] [Sonnet] t4 在小量餵入時的效能，以實際 T.38 封包大小量測（M2b 審核；M2c：最差 1 byte 一包也只用線路時間的 0.7 %，不需要改成可續解）

## 現場試接
- [x] [Opus] 現場試接指南 docs/field-test.md，打包進 tarball
- [x] [Any] 發佈 v0.1.0-alpha（GitHub Release，prerelease）：https://github.com/leoyu0607/sip-fax-rs/releases/tag/v0.1.0-alpha
- [ ] [使用者] 對接 CS210 試接，回傳 logs/calls/ 和結果；依結果調整 `cht-cs210` 預設組

## 現場問題
- [x] [Sonnet] F1：AudioCodes Mediant 1000 v6.80A 以 T.38 送 8 頁只收到 1 頁（`t2_expired_image`）。根因 1：`HdlcFcsOk` 之後對方的 V.21 訊號還沒結束，我方就開始回應（引擎現在等 `hdlc-sig-end`／`no-signal`／新指示，保底 `peer_signal_wait_ms` = 500 ms）；根因 2：重送的 MPS／EOP／EOM 沒有重新啟動 T2（t30 現在重送回應並在送完後重啟 T2）；另查出 `bad_lines=1` 是 t4 把第一個 EOL 之前約 1 s 的 0xFF（marking idle）算成一個壞行（已修）。F1-01～F1-06 見 docs/test-plans/F1-audiocodes-multipage.md 與 docs/test-reports/F1.md；版本 0.1.1（待 Opus 審核，之後發佈 `v0.1.1-alpha`）
- [ ] [Opus] 審核 docs/test-reports/F1.md，合併報告第 7 節的擬議 ADR 到 docs/DECISIONS.md
- [ ] [使用者] 以 0.1.1 再對接 AudioCodes 一次，確認 8 頁都收到；同意後才能把 `udptl = "full"` 的擷取（含影像資料）放進 repo
- [x] [Any] 發佈 v0.1.1-alpha
- [x] [Any] 發佈 v0.2.0-alpha（M3）

## M3 — Store / Delivery / ECM
> 2026-10-21 恢復開發：M3b（整合）、M3c（delivery）、M3e（PNG 預覽）、M3f（delivery 接進 sip-faxd）完成，已合併到 main。測試報告 M3a–M3f 全數審核通過。
- [x] [Opus] 依需求定案資料表 schema（`inbound_faxes` 欄位對應 GOALS 的欄位表），並撰寫 M3 設計和測試計畫
- [x] [Sonnet] ANI/DNIS 擷取規則（PAI、From、Request-URI、To、Diversion 的優先順序，可依 trunk 設定）
- [x] [Sonnet] 掛線方向和掛線原因的正規化（SIP 加 T.30）
- [x] [Sonnet] 已讀/未讀（全體共用，記錄切換者和時間）、備註（記錄修改者和時間）的資料模型和 API
- [x] [Sonnet] PNG 預覽：開啟時從 TIFF 轉出每頁 PNG 並快取（純 Rust，新增依賴要寫 ADR）（M3e，ADR-049）
- [x] [Sonnet] 接收中狀態與即時頁數：INVITE 接通時就建立紀錄（狀態 `receiving`），每收完一頁就更新已收頁數，通話結束時再寫入最終狀態；正式的 TIFF/PDF 仍然在整通結束後才寫出（2026-10-21 使用者需求）
- [x] [Sonnet] 每個 trunk 可以設定 DIS 要宣告哪些解析度（standard/fine；superfine、300/400 dpi 視需求再評估）（2026-10-21 使用者詢問）
- [x] [Sonnet] M3b 追加：migration 並行鎖（advisory lock）、MySQL 非交易 DDL 的可重跑性、本機 musl 建置改走 Docker 或固定 zig 0.14（M3a 審核）
- [x] [Sonnet] M3b 追加：result.json、DB 和 config check 顯示 ECM 與協商結果（M3d 審核）
- [x] [Any] db-matrix 第一次在 GitHub 跑完後，把檢查名稱加進 `scripts/branch-protection.json`（使用者需重新套用）
- [x] [Sonnet] 清整排程：檔案（TIFF/PDF/PNG）和紀錄分別設定保留天數，刪檔後紀錄標記「檔案已清除」；測試要涵蓋時間邊界和刪檔失敗的情況
- [x] [Opus] 取得 ITU-T T.30 原文，重新核對 DIS/DCS 的 bit 位置（V.33、掃描時間碼）（M2c 審核）
- [x] [Sonnet] ECM 下的 MMR 互通測試（補 X01 的 MMR 格）（M2c 審核）
- [x] [Opus] 撰寫 docs/test-plans/M3.md
- [x] [Sonnet] store：Repository traits、sqlx（SQLite/MySQL/PG）、tiberius（MSSQL）、migrations（M3a）
- [x] [Sonnet] delivery：folder、email（lettre、rustls）、webhook（reqwest、rustls），含重試佇列（M3c，ADR-047、048）
- [x] [Sonnet] M3f：delivery 接進 sip-faxd（設定、config check、catch_up、finalize 後通知、E2E、musl 確認）（ADR-050）
- [x] [Any] 第一次 push 後確認 CI（02618eb 全綠）：db-matrix 的 migration 並行測試、coverage 的 `--ignore-filename-regex`、test／coverage job 的 SMTP services
- [ ] [Any] 在有 nightly 的環境跑 t4 的 `tiff_read` fuzz 目標
- [ ] [Sonnet] `crates/fax-in/tests/t38session.rs` 在高 CPU 負載（llvm-cov 加上平行編譯）下有 5 個案例逾時（`timed out waiting for a frame from the server`）；單獨重跑通過。改用注入時鐘或放寬逾時（2026-10-22 發版前發現）
- [x] [Sonnet] t30 ECM（PPS/PPR、64 frame block）（M3d；預設關閉，ADR-035）
- [x] [Any] CI 新增 `db-matrix` job（MySQL、MariaDB、PostgreSQL、MSSQL 的 services）
- [x] [Opus] 審核 docs/test-reports/M3a–M3f.md（全數通過）

## M4 — 介面
> 2026-10-21 使用者決定：帳號分管理員／座席；Web 可選 HTTP 或 HTTPS（HTTPS 沒有憑證時自動產生 10 年自簽憑證）；介面繁中＋英文；除了啟動必需的設定，其餘設定改存 DB、可在 Web 修改（ADR-051）。設計見 `docs/design/m4-web.md`，分期 M4a–M4d。
- [x] [Opus] 撰寫 docs/design/m4-web.md 與 docs/test-plans/M4.md
- [x] [Sonnet] M4a：執行設定入 DB（匯入／匯出、驗證、秘密加密、熱套用）、帳號與 session、CLI（config、user、fax）
  - 2026-10-22：Opus 審核通過（`docs/test-reports/M4a.md` §10）；審核時修正 i05 的 schema 版本、K10 的 nonce 斷言、R05 的真實時區測試、投遞 catch-up 延遲與 f22 的競態；ADR-052–054。
- [ ] [Sonnet] M4b：web crate（REST、WebSocket、HTTP/HTTPS 與自簽憑證、安全 header）
  - 2026-10-22 使用者決定：自簽憑證改為有效期 825 天、到期前 30 天自動更新（macOS／iOS 拒絕超過 825 天的憑證；ADR-056）。
  - 分階段：① web crate 骨架與 HTTP/HTTPS（H）② 登入、session、CSRF、權限（A 的 HTTP 部分）③ 傳真 API（W01–W08）④ 設定、帳號、狀態 API（W09–W11）⑤ WebSocket（V）⑥ Sonnet 依計畫補測試並寫報告。分支 `m4b-web`。
  - [x] ① web crate 骨架與 HTTP/HTTPS
  - [x] ② 登入、登出、`/auth/me`、改密碼；session cookie（HttpOnly、SameSite=Strict、HTTPS 加 Secure）；CSRF（JSON＋`X-Sipfax-Csrf`）；第一次登入強制改密碼；IP 限流 429、帳號鎖定 423（`Retry-After`）
  - [x] ③ 傳真 API（W01–W08）：`GET /faxes`（篩選、cursor 分頁）、`GET /faxes/{id}`（含投遞，webhook 目標遮蔽）、`PUT /faxes/{id}/read`、`PUT /faxes/{id}/note`（409 附目前備註）、`GET /faxes/{id}/pages/{n}.png`（ETag／304）、`GET /faxes/{id}/file.pdf|file.tif`（只讀 data_dir 內的一般檔案，410）、`POST /faxes/{id}/redeliver`（admin）
  - [x] ④ 設定、帳號、狀態 API（W09–W11）：`GET /config`（全部區段＋`status`）、`GET/PUT /config/{section}`（PUT body `{version, body}`；422 `invalid` 的 `fields` 以欄位路徑為 key、每欄位取第一個錯誤；409 `conflict` 附 `current_version`）、`DELETE /config/trunk:{name}?version=N`（非 trunk 回 405）、`PUT /config/trunks/order`（`{order: [...]}`，規格沒列、為了 trunk 比對順序新增）、`GET /config/audit?limit&before`（回 `entries`、`next`）；`GET/POST /users`、`PUT/DELETE /users/{id}`（PUT 可帶 `password` 重設密碼，`must_change_password` 預設 true；`display_name`／`locale` 給 `null` 是清除；`last_admin` → 409）；`GET /status`（`service`：`web::StatusSource` trait，sip-faxd 以 `HubStatus` 包 `service::hub::Hub`；`database`：3 秒內以 `schema_version()` 檢查；`config`：`source`、`pending_restart`，admin 另有 `key`、`undecryptable`）；W11：每個請求 30 秒期限（`lib.rs` 的 `deadline` middleware，回 408 `timeout`，含讀 body；下載的串流不受限），hyper 的 header 讀取逾時 30 秒（`server.rs` 加 `TokioTimer`）。測試 `crates/web/tests/admin.rs`（10 個）；`common::env_with(timeout)` 可縮短期限，`env.status.set(...)` 改 `/status` 的服務狀態
  - [ ] ⑤ WebSocket `/api/v1/events`（V01–V06）：事件 `fax.created/progress/finalized/read_changed/note_changed`、`delivery.updated`、`status.changed`，`seq` 與 1000 筆環狀緩衝、`since` 補送、`resync`、30 秒 ping／60 秒逾時、慢客戶端斷線；登出／停用／改密碼時關閉該使用者的連線（A08）。③ 的 read／note 處理器之後要送出事件
  - [ ] ⑥ Sonnet 依 `docs/test-plans/M4.md` §2–§5 補齊測試並寫 `docs/test-reports/M4b.md`（含 H01／H05／H08 在 Docker 內以 `openssl` 驗證、A11 權限矩陣表格驅動、涵蓋率 web ≥ 80%）
  - [ ] `GET /faxes` 的全文搜尋 `q`（規格 §3）：store 的 `ListFilter` 沒有這個欄位，需要在四種 DB 加 LIKE 查詢（ani、dnis、remote_ident、note），③ 先略過
  - **交接狀態（2026-10-22）**：分支 `m4b-web`（已 push），①②③④ 完成，從 ⑤ 接手（WebSocket：事件來源可沿用 `service::hub::Hub` 的 `subscribe` 廣播，再加傳真 read／note 與投遞事件；做法同 `/status`，在 `AppState` 加 trait）。web crate 的入口是 `crates/web/src/lib.rs::app(AppState)`；路由在 `crates/web/src/api/`，每個處理器以 `Session` extractor 驗證並以 `s.need(Capability::…)` 檢查權限，body 用 `ApiJson`，錯誤用 `ApiError`（`{"error": {code, message, fields?}}`）。`sip-faxd serve` 在 `apps/sip-faxd/src/web_service.rs` 組 `AppState`。測試：`crates/web/tests/`（`common::env()` 起真的 server＋SQLite）。已知：在本機（macOS 的 Claude Code 沙盒）跑整個 workspace 測試時，sip-faxd 的 control／logfile／delivery_service 單元測試偶爾因暫存目錄寫入被拒（`Permission denied`）而失敗，重跑或在 CI 上都通過，與程式無關
- [ ] [Sonnet] M4c：web-ui（Svelte SPA、i18n、Playwright E2E）
- [x] [Sonnet] M4d：控制 socket、start/stop/restart/status/logs、log 輪替、TUI
  - 2026-10-22：實作完成（分支 `m4d-service-management`，已 rebase 到含 M4a 的 main），測試報告 `docs/test-reports/M4d.md`，**Opus 審核通過**（§9）；ADR-055。
- [ ] [Sonnet] web：axum REST、WebSocket、驗證機制（argon2 加 session）
- [ ] [Sonnet] 傳真列表與預覽頁：已讀/未讀篩選、可手動切換、備註編輯、PNG 逐頁預覽、下載 TIFF/PDF；接收中的傳真即時顯示進度（第 N 頁）
- [ ] [Sonnet] web-ui：Svelte + Vite SPA，以 rust-embed 內嵌
- [x] [Sonnet] TUI（ratatui）和 CLI 子命令
- [x] [Sonnet] 服務管理指令 `sip-faxd start | stop | restart | status | logs`（2026-10-21 使用者需求）：
  - `start`：在背景啟動服務（以 setsid 重新執行自己，寫入 PID 檔 `run/sip-faxd.pid` 並加 flock 防止重複啟動），等服務就緒後，在前景持續顯示 log（`logs/sip-faxd.log`）。**Ctrl+C 只停止顯示 log，不會停止服務**，並提示這一點；`start -d` 只啟動、不顯示 log。
  - `stop`：**等進行中的傳真收完再停**（使用者決定）。立即停止接新來電（回 503），等進行中的通話結束，最多等待 `[service] stop_timeout_secs` 秒（預設 300）。`--force` 立即停止，已收到的頁面仍會寫出並標記 partial。
  - `restart`：先 stop（同樣等通話結束），再 start。
  - `status`：是否在執行、PID、版本、uptime、監聽位址、進行中的通話數、各 trunk 的註冊狀態。透過本機控制介面（unix socket 或 M4 的本機 HTTP API）取得。
  - `logs [-f]`：查看 log。服務 log 檔要能輪替，依大小或天數，並設定保留份數。
  - 用 systemd 管理時仍使用 `serve`（前景）。偵測到服務已經由 systemd 管理時，`start`/`stop` 要提示改用 `systemctl`。

- [x] [Sonnet] 手動重新投遞（`failed`／`giving_up` 的投遞；Web 按鈕與 CLI）（M3c 審核）（M4a 完成：CLI 與 store；Web 按鈕屬於 M4c）
- [x] [Sonnet] 投遞服務關閉時的寬限逾時、執行中 DB 重啟、非 UTC 時區範本的測試（M3f 審核；與 stop/restart 一起做）（M4a 完成）
- [x] [Sonnet] 預覽快取：與清整的競態（孤兒 PNG）、容量上限（M3e 審核）（M4a 完成）
- [ ] [Any] CI 新增 `web-ui` job
- [ ] [Opus] 審核 docs/test-reports/M4.md

### 文件（優先級在功能開發之後）
> 2026-10-22 使用者決定：使用手冊分「座席」與「IT」兩份；先寫 Markdown（`docs/manual/`，繁中），要 release 時轉成 PDF；正式 release 的 PDF 手冊要含截圖畫面（Web 的截圖可在 M4c 以 Playwright 自動產生）。
- [ ] [Any] `CHANGELOG.md` 的 `[Unreleased]` 補上 M4a（設定存 DB、帳號與 session、`config`／`user`／`fax` CLI）與 TUI，中英兩段都要補；下次發版前完成
- [ ] [Opus] 座席使用手冊 `docs/manual/agent.md`：看傳真、篩選未讀、已讀、備註、投遞狀態（TUI；M4c 之後是 Web，Web 完成後定稿）
- [ ] [Opus] IT 安裝維運手冊 `docs/manual/it.md`：安裝（RHEL 9/10）、設定檔、資料庫、gateway 試接、`start/stop/status/logs`、systemd、log 輪替、帳號、備份與還原（`config export/import`）、疑難排解
- [ ] [Any] release 時把兩份手冊轉成 PDF；正式 release 的 PDF 要含截圖

## M5 — G.711 passthrough
- [ ] [Opus] 撰寫 docs/test-plans/M5.md
- [ ] [Sonnet] fax-modem：CNG/CED、V.21 HDLC、V.27ter、V.29、V.17（接收方向優先）
- [ ] [Sonnet] RTP 接收 payload stream 和 jitter buffer
- [ ] [Opus] 審核 docs/test-reports/M5.md

## M6 — 互通強化與發佈
- [ ] [Sonnet] T.38 latch 之後對方位址改變的處理（M2d 審核）
- [ ] [Any] 用 CS210 的實際擷取驗證 localTCF 時是否送 CFR，並調整 `cht-cs210` 預設組（M2c 審核）
- [ ] [Sonnet] K01 的根本解：dialog 改用單一有序事件佇列（ADR-016）
- [ ] [Opus] 撰寫 docs/test-plans/M6.md
- [ ] [Sonnet] 建立 Sonus/AudioCodes SIP trace 回歸測試集
- [ ] [Sonnet] 打包時加入 systemd unit 範例和 conf 範例
- [ ] [Any] 打 v0.1.0 tag，產生 Release
- [ ] [Opus] 審核 docs/test-reports/M6.md
