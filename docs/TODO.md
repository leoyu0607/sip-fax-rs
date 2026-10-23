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
- [x] [Any] 發佈 v0.4.1-alpha（Web 介面改善，2026-10-23）：版本 0.4.1（Cargo、tools、web-ui）、CHANGELOG 中英、README 狀態、ROADMAP 發佈紀錄、field-test（0.4.1 改善與從 0.4.0 升級）、兩份手冊的適用版本；tag 在 main
- [x] [Any] 發佈 v0.4.0-alpha（M4e、M4f，2026-10-23）：版本 0.4.0（Cargo、tools、web-ui）、CHANGELOG 中英、README 狀態、ROADMAP 發佈紀錄、field-test（從 0.3.0 升級）、兩份手冊的適用版本；tag 在 main
- [x] [Any] 發佈 v0.3.0-alpha（M4，2026-10-22，tag 在 main 的 merge commit `8911040`）：版本 0.3.0、CHANGELOG 中英、README 狀態、ROADMAP 發佈紀錄、field-test（升級說明）；`scripts/release-notes.sh` 改用 awk 反轉行（macOS 沒有 `tac`）；發版時 CI 的 `cargo deny` 擋下當天公布的 RUSTSEC-2026-0384（tokio 1.54.0 的 `Interval` 競態，已被撤下），升到 1.54.1 並寫進 CHANGELOG 的安全性

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
- [x] [Sonnet] M4b：web crate（REST、WebSocket、HTTP/HTTPS 與自簽憑證、安全 header）（2026-10-22，分支 `m4b-web`，測試報告 `docs/test-reports/M4b.md`，Opus 審核通過）
  - 2026-10-22 使用者決定：自簽憑證改為有效期 825 天、到期前 30 天自動更新（macOS／iOS 拒絕超過 825 天的憑證；ADR-056）。
  - 分階段：① web crate 骨架與 HTTP/HTTPS（H）② 登入、session、CSRF、權限（A 的 HTTP 部分）③ 傳真 API（W01–W08）④ 設定、帳號、狀態 API（W09–W11）⑤ WebSocket（V）⑥ Sonnet 依計畫補測試並寫報告。分支 `m4b-web`。
  - [x] ① web crate 骨架與 HTTP/HTTPS
  - [x] ② 登入、登出、`/auth/me`、改密碼；session cookie（HttpOnly、SameSite=Strict、HTTPS 加 Secure）；CSRF（JSON＋`X-Sipfax-Csrf`）；第一次登入強制改密碼；IP 限流 429、帳號鎖定 423（`Retry-After`）
  - [x] ③ 傳真 API（W01–W08）：`GET /faxes`（篩選、cursor 分頁）、`GET /faxes/{id}`（含投遞，webhook 目標遮蔽）、`PUT /faxes/{id}/read`、`PUT /faxes/{id}/note`（409 附目前備註）、`GET /faxes/{id}/pages/{n}.png`（ETag／304）、`GET /faxes/{id}/file.pdf|file.tif`（只讀 data_dir 內的一般檔案，410）、`POST /faxes/{id}/redeliver`（admin）
  - [x] ④ 設定、帳號、狀態 API（W09–W11）：`GET /config`（全部區段＋`status`）、`GET/PUT /config/{section}`（PUT body `{version, body}`；422 `invalid` 的 `fields` 以欄位路徑為 key、每欄位取第一個錯誤；409 `conflict` 附 `current_version`）、`DELETE /config/trunk:{name}?version=N`（非 trunk 回 405）、`PUT /config/trunks/order`（`{order: [...]}`，規格沒列、為了 trunk 比對順序新增）、`GET /config/audit?limit&before`（回 `entries`、`next`）；`GET/POST /users`、`PUT/DELETE /users/{id}`（PUT 可帶 `password` 重設密碼，`must_change_password` 預設 true；`display_name`／`locale` 給 `null` 是清除；`last_admin` → 409）；`GET /status`（`service`：`web::StatusSource` trait，sip-faxd 以 `HubStatus` 包 `service::hub::Hub`；`database`：3 秒內以 `schema_version()` 檢查；`config`：`source`、`pending_restart`，admin 另有 `key`、`undecryptable`）；W11：每個請求 30 秒期限（`lib.rs` 的 `deadline` middleware，回 408 `timeout`，含讀 body；下載的串流不受限），hyper 的 header 讀取逾時 30 秒（`server.rs` 加 `TokioTimer`）。測試 `crates/web/tests/admin.rs`（10 個）；`common::env_with(timeout)` 可縮短期限，`env.status.set(...)` 改 `/status` 的服務狀態
  - [x] ⑤ WebSocket `/api/v1/events`（V01–V06）：`crates/web/src/events.rs` 的 `EventBus`（編號、1000 筆環狀緩衝與廣播在同一把鎖下，`attach(since)` 一次取得補送與即時接收，不漏不重）；`crates/web/src/api/events.rs` 的連線處理（先送 `hello{seq}`，再送 `since` 之後的事件或 `resync`；30 秒 ping、60 秒沒有 pong 關閉 4002；落後 256 筆關閉 4008；帳號 session 結束關閉 4001；檢查 `Origin` 與 `Host` 相同）。事件來源：fax-in recorder 新增 `RecorderConfig::on_written`（`RecordWritten::Created/Pages/Finalized`，寫進 DB 後才送，帶 row id）→ `fax.created/progress/finalized`；delivery 新增 `DeliveryEngine::set_on_change`、sip-faxd `DeliveryService::set_on_change` → `delivery.updated {fax_id}`；web 的 read／note／redeliver 處理器 → `fax.read_changed`／`fax.note_changed`／`delivery.updated`；`events::watch_status` 每 2 秒比較 `StatusSource`（不含 uptime）與 `pending_restart` → `status.changed`。登出、改密碼、admin 停用／降級／重設密碼／刪除帳號時 `revoke_user`。測試：`crates/web/tests/events.rs`（6 個，tokio-tungstenite 真的 WebSocket）、`events.rs` 單元測試 3 個、`fax-in/tests/recorder.rs` f03、`delivery/tests/retry.rs` 的 change hook。**已知限制**：用 CLI（`sip-faxd user disable`）停用帳號不會立刻關閉該帳號的 WebSocket（CLI 不經過服務），要等下一次重連時才被拒絕；連線期間不重新驗證 session，因為 `authenticate` 會更新 `last_seen`，開著分頁就會讓閒置逾時失效
  - [x] ⑥ Sonnet 依 `docs/test-plans/M4.md` §2–§5 補齊測試並寫 `docs/test-reports/M4b.md`（含 H01／H05／H08 在 Docker 內以 `openssl` 驗證、A11 權限矩陣表格驅動、涵蓋率 web ≥ 80%）。2026-10-22 完成，**Opus 審核通過**（報告 §9）；測試找到並修正 3 個產品問題（框架錯誤不是 API 錯誤格式、預覽遇到 data_dir 外的路徑回 500、`PUT /users/{id}` 帶密碼且帳號變更被拒時密碼已被重設）
  - [x] M4c 的 Playwright E2E 要補一個「真的收一份傳真，前端即時看到 `fax.created`／`fax.progress`／`fax.finalized`」的案例（M4b 審核：V01 目前以真的 `Recorder` 驅動，不是 SpanDSP 通話）（併入 M4c ⑥，即 U11）
  - [ ] 把 `scripts/web-openssl-docker.sh` 放進 GitHub CI 的 `e2e-spandsp` job（M4b 審核；目前只在 `ci-local.sh` 有 Docker 時執行）
  - [x] `GET /faxes` 的全文搜尋 `q`（規格 §3）：M4c ③ 完成（`ListFilter::q`，store 測試 s07c）
  - **交接狀態（2026-10-22）**：分支 `m4b-web`（已 push），①–⑤ 完成，下一步是 ⑥（Sonnet subagent 依測試計畫補測試並寫報告，之後 Opus 審核）。web crate 的入口是 `crates/web/src/lib.rs::app(AppState)`；路由在 `crates/web/src/api/`，每個處理器以 `Session` extractor 驗證並以 `s.need(Capability::…)` 檢查權限，body 用 `ApiJson`，錯誤用 `ApiError`（`{"error": {code, message, fields?}}`）。`sip-faxd serve` 在 `apps/sip-faxd/src/web_service.rs` 組 `AppState`。測試：`crates/web/tests/`（`common::env()` 起真的 server＋SQLite）。已知：在本機（macOS 的 Claude Code 沙盒）跑整個 workspace 測試時，sip-faxd 的 control／logfile／delivery_service 單元測試偶爾因暫存目錄寫入被拒（`Permission denied`）而失敗，重跑或在 CI 上都通過，與程式無關
- [x] [Any] 2026-10-22 使用者決定（ADR-057）：① Web 預設開啟（沒有 `[web]` 也啟動；測試用設定檔一律 `enabled = false`）；② PNG 預覽在收完傳真時由伺服器產生，寫在 TIFF／PDF 旁（`<name>-p<n>.png`、`-thumb.png`），預覽 API 讀這些檔案，沒有才轉一次；取消 `cache/png` 與 `preview_cache_mb` 的容量上限；清整一併刪除。GOALS、CHANGELOG（中英）、README（中英）、field-test（防火牆 8443）已更新
- [x] [Sonnet] M4c：web-ui（Svelte SPA、i18n、Playwright E2E）（分支 `m4c-web-ui`；ADR-058；測試報告 `docs/test-reports/M4c.md`，**Opus 審核通過**）
  - 分階段：① 骨架與嵌入 ② 共用模組與登入 ③ 傳真列表與內容頁 ④ 管理頁 ⑤ CI 與打包 ⑥ Sonnet 依測試計畫補測試（vitest＋Playwright E2E 在 Docker）並寫報告、Opus 審核 ⑦ 文件（`docs/web.md`、README、field-test）
  - [x] ① 骨架與嵌入：`web-ui/`（Svelte 5＋Vite 8＋TS 6，`npm run lint|check|test|coverage|build`）；`crates/web/build.rs` 把 `web-ui/dist` 嵌進執行檔（沒有 dist 時嵌入說明頁；`SIPFAX_REQUIRE_WEB_UI=1` 時失敗），`crates/web/src/ui.rs` 提供 `/` 與檔案（`assets/` 長期快取）
  - [x] ② 共用模組與登入：`src/lib/api.ts`（CSRF、錯誤格式）、`i18n.svelte.ts`＋`src/locales/*.json`（由 `scripts/gen-locales.mjs` 產生，427 個 key）、`route.ts`（hash 路由）、`events.ts`（`since`／`resync`／4001／4008 重連）、`session.svelte.ts`、`live.svelte.ts`；登入、強制改密碼、我的帳號（語言、改密碼）
  - [x] ③ 傳真列表（篩選、`q` 搜尋、keyset 捲動、即時事件、捲動時新傳真暫存成「有 N 份新傳真」、列上切換已讀）與內容頁（縮圖＋整頁、縮放、方向鍵換頁、下載、已讀、備註衝突處理、投遞、重新投遞）；後端 `ListFilter::q`（store 測試 s07c，四種 DB 同一段 SQL）
  - [x] ④ 管理頁：設定（`src/lib/schema.ts` 欄位定義表、`FieldInput`／`SectionEditor`／`RoutesEditor`、trunk 新增／刪除／排序、需要重啟的欄位標示：後端 `RESTART_FIELDS`→`GET /config` 的 `restart_fields`）、帳號、稽核紀錄、系統狀態（`status.changed` 即時更新）；後端 `PUT /auth/me`、`locale` 檢查（`bad_locale`）
  - [x] ⑤ CI 與打包：`web-ui` job（lint、check、coverage ≥ 70%、build）；`build-musl` 先建前端並設 `SIPFAX_REQUIRE_WEB_UI=1`；`package.sh` 檢查執行檔含有這次的前端；`ci-local.sh` 有 npm 時一併執行
  - [x] ⑥ Sonnet 依 `docs/test-plans/M4.md` §6 與 M4c 新增案例補測試並寫 `docs/test-reports/M4c.md`；Playwright E2E（U10–U19、M4b 留下的「真的收一份傳真看到 fax.created／progress／finalized」）放進 `scripts/e2e-docker.sh` 與 CI；U02 需要後端產生代碼清單的測試
    - 2026-10-22 測試完成，**待 Opus 審核**（報告 `docs/test-reports/M4c.md`）：W12、W13、A14、U05（`crates/web/tests/m4c.rs`、`ui.rs`）、U02（`crates/web/tests/codes.rs` 寫 `web-ui/src/lib/api-codes.json`、vitest `codes.test.ts`）、U06（`scripts/test-package.sh`）、U07（vitest 41 個、statements 91.65%）、Playwright 16 個（`web-ui/e2e/*.spec.ts`：U08、U10–U19，U11 是 SpanDSP 真的送 3 頁；執行 `scripts/e2e-web.sh`，已放進 `ci-local.sh`、CI 的 `e2e-web` job 與 `branch-protection.json`，需管理員重新套用）。web 行涵蓋率 95.31%。測試找到並修正 6 個產品問題（`validation.reversed` 沒翻譯、路由／投遞的 422 沒顯示、備註即時更新判斷錯、保留的秘密被重新加密、列表離開頁面時錯過新傳真、`PUT /auth/me {}` 清掉語言）。M4b 留下的「真的收一份傳真看到即時事件」由 U11 關閉。
    - 2026-10-22 **Opus 審核通過**（報告 §9）。審核時修正報告留下的兩個介面問題（新 trunk 第一次儲存後表單重建、清除秘密後仍顯示「已設定」：儲存後重讀區段），U08 改為斷言這兩點；ADR-058 補上保留秘密不重新加密等決策。
  - [x] ⑦ 文件：`docs/web.md`（繁中使用說明，打包時放進 `docs/`）、兩份 README 的 Web 介面段落、`docs/field-test.md`。README 頂部的**狀態**段落在發版時更新（發版檢查清單）
  - **交接狀態（2026-10-22）**：M4c 全部完成並審核通過。待辦：CI 的 `e2e-web` job 在 GitHub 第一次執行的結果要確認；`web-ui`、`e2e-web` 已於 2026-10-22 套用為必要檢查（`scripts/set-branch-protection.sh`）。執行檔大小（x86_64 musl，含介面）：19.46 MB，v0.2.0-alpha 為 14.54 MB（+4.9 MB，M4a–M4d 的 axum／rustls 伺服器、argon2、ratatui 與介面）。本機開發：`cd web-ui && npm run dev`（proxy 到 `SIPFAX_DEV_API`，預設 https://127.0.0.1:8443）；或 `npm run build` 後重新 `cargo build`，`sip-faxd serve` 直接提供介面。注意：macOS 上覆蓋執行中的執行檔（`cp` 到同一路徑）會讓新程序被 SIGKILL，要先 `rm` 再複製。W10 的 413 案例原本送 2 MiB，伺服器回 413 並關閉連線時用戶端可能還在寫而得到 reset（main 上也會偶發），改為只超過上限 16 bytes
- [x] [Sonnet] M4d：控制 socket、start/stop/restart/status/logs、log 輪替、TUI
  - 2026-10-22：實作完成（分支 `m4d-service-management`，已 rebase 到含 M4a 的 main），測試報告 `docs/test-reports/M4d.md`，**Opus 審核通過**（§9）；ADR-055。
- [x] [Sonnet] web：axum REST、WebSocket、驗證機制（argon2 加 session）（M4a／M4b 完成）
- [x] [Sonnet] 傳真列表與預覽頁：已讀/未讀篩選、可手動切換、備註編輯、PNG 逐頁預覽、下載 TIFF/PDF；接收中的傳真即時顯示進度（第 N 頁）（M4c ③；E2E U11–U13）
- [x] [Sonnet] web-ui：Svelte + Vite SPA，內嵌進執行檔（M4c ①；以 build.rs 取代 rust-embed，ADR-058）
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
- [x] [Any] CI 新增 `web-ui` job（M4c ⑤；2026-10-22 已套用為必要檢查，連同 `e2e-web`）
- [x] [Opus] 審核 M4 測試報告：分期為 `docs/test-reports/M4a.md`、`M4b.md`、`M4c.md`、`M4d.md`，各自審核通過（2026-10-22）

## M4e — 換資料庫（`sip-faxd db copy`）
> 2026-10-22 使用者同意：SQLite 之後想改用 MSSQL 時，要能把全部資料（含已讀、備註、投遞、帳號）搬過去。ADR-059；測試計畫 `docs/test-plans/M4e.md`；分支 `m4e-db-copy`。
- [x] [Opus] 撰寫 `docs/test-plans/M4e.md`（含設計摘要）
- [x] [Any] 實作：`crates/store/src/copy.rs`（`copy_database`）、CLI `sip-faxd db copy --to <URL>`（`apps/sip-faxd/src/commands.rs::db_copy`）；文件：`docs/database.md`「換資料庫」、`docs/manual/it.md` §11、CHANGELOG 中英
  - 2026-10-22 手動驗證：SQLite → PostgreSQL／MSSQL／MySQL，再從 MSSQL、MySQL 複製回 SQLite，七個表逐列與原始相同；複製後 PostgreSQL、MSSQL 新增帳號的 id 接續（2）
- [x] [Sonnet] 依計畫實作 C01–C13 並寫 `docs/test-reports/M4e.md`（DB 矩陣四種 DB 全過、C12 在 Docker 內以 SpanDSP 收傳真後搬到 PostgreSQL；沒有找到產品錯誤；store 涵蓋率 83.08%）
  - 測試：`crates/store/src/tests/copy_tests.rs`（C01–C06、C10、C13）、`apps/sip-faxd/tests/m4e_cli.rs`（C07–C09、C11）、`tools/fax-sim/tests/e2e_m4e.rs`（C12；`scripts/e2e-docker.sh` 會起 PostgreSQL 容器，CI 的 `e2e-spandsp` 有 postgres service）
- [x] [Opus] 審核（2026-10-22 通過，報告 §9；審核時補上檔案路徑存相對於 `data_dir`、換機器要連 `data/faxes/` 一起搬的說明）

## M4f — 設定簡化：資料庫分欄位、`db check`、設定檔只留啟動設定
> 2026-10-22 使用者決定（ADR-060）：① 資料庫帳密不必百分比編碼（分欄位、`password_file`）；② 單純測試連線的指令；③ 設定檔只放啟動設定，執行設定在 Web；保留舊設定檔的相容匯入。測試計畫 `docs/test-plans/M4f.md`；分支 `m4f-config`（從 `m4e-db-copy` 開出）。
- [x] [Opus] 撰寫 `docs/test-plans/M4f.md`
- [x] [Any] 實作：`apps/sip-faxd/src/settings.rs`（`DatabaseFile` → `DatabaseSection`）、`crates/store/src/check.rs`（`check_url`）、`db check`、`db copy --to-password-stdin`／`--to-config`、`config check` 與啟動 log 的訊息（`commands::NO_TRUNK`）、Web 的「沒有中繼」橫幅（`App.svelte`）、`conf/sip-faxd.toml.example` 拆出 `conf/runtime.toml.example`（打包時一起放）；文件：field-test、IT 手冊、database.md、web.md、兩份 README、CHANGELOG 中英
  - 2026-10-22 手動驗證：PostgreSQL 建立密碼 `p@ss:w/rd#1 中%` 的帳號，分欄位與 `--password-stdin` 都能連線；`db check` 對錯密碼、不存在的資料庫、關閉的 port、不存在的 SQLite 檔各給正確提示；`db copy --to-password-stdin` 成功
- [x] [Sonnet] 依計畫實作 D01–F05 並寫 `docs/test-reports/M4f.md`（2026-10-23 由另一位 agent 接手完成驗證）
  - 已完成且通過：D01、D03、D04（`apps/sip-faxd/src/settings.rs::m4f_tests`）；D05、K01–K04（SQLite／關閉的 port／解析不到的主機）、P01、P02、F01–F04（`apps/sip-faxd/tests/m4f_cli.rs`）；D02、K01–K04、P01 的伺服器部分（`apps/sip-faxd/tests/m4f_server.rs`，db-matrix 四種都過，CI `db-matrix` 新增 step）；store 層 `crates/store/src/tests/check_tests.rs`；F02 真傳真（`tools/fax-sim/tests/e2e_m4f.rs`）；F05（`web-ui/e2e/setup.spec.ts` 6 個案例，e2e-web 實例 `c`）
  - 沒有 ignore 任何測試。未解：`scripts/e2e-web.sh` 最後一次執行 `access.spec.ts` U18（`toBeFocused`）失敗，前一次通過，疑似不穩定，需重跑確認；MySQL 的 R04（既有）在 db-matrix 偶爾逾時
  - 執行結果：fmt、clippy、deny、`cargo test --workspace`（1102 通過）、db-matrix 四種、`e2e_m4f`（Docker）、web-ui lint／check／coverage、store 涵蓋率 81.53% 都通過（細節見報告 §7）
  - 修掉的產品問題：`--to-password-stdin` 搭配 `--to-config` 被靜默接受；MSSQL 資料庫不存在被提示成密碼錯誤；新增 PostgreSQL emoji 密碼（SASLprep，sqlx 限制，未修）的提示。報告 §3 另列 5 個觀察到但沒修的小問題
  - 2026-10-23：GitHub CI 的 `e2e-web` 連續兩次通過，U18 沒有重現（視為偶發）；CI run 39824400230 全綠（含 db-matrix 新 step）
- [x] [Any] `docs/database.md` 與 `docs/manual/it.md` 要明白寫：sip-faxd 需要自己專用的資料庫（不要與其他系統共用；`users`、`sessions`、`settings`、`schema_migrations` 等通用表名可能衝突，MySQL／MariaDB 的 `CREATE TABLE IF NOT EXISTS` 會悄悄沿用別的系統的表）。使用者 2026-10-22 決定：只要求專用資料庫，不做衝突檢查，不加表名前綴
- [x] [Opus] 審核 `docs/test-reports/M4f.md`（2026-10-23 通過，報告 §9；審核時修正新安裝的 log 訊息、Windows 的編譯警告，新增 `.gitattributes`）
- [x] [Any] 合併順序：先把 `m4e-db-copy`（審核已通過；確認其 CI run 39794768007 為綠）合進 main，再合 `m4f-config`，然後依 CLAUDE.md 發版檢查清單發 v0.4.0-alpha（2026-10-23：兩個分支以 merge commit 合進 main）
- 備註：RUSTSEC-2026-0381/0382/0383（rustls-webpki、webpki-roots、webpki-root-certs，2026-10-19/20）已於 2026-10-23 檢查：main、m4e、m4f、v0.2.0、v0.3.0 的 lock 檔都已是修正版（0.103.16、1.0.10），不需處理

## Web 介面改善（2026-10-23 使用者提出）
- [x] [Any] 系統狀態頁的運作時間（uptime）不會跳：`web-ui/src/routes/Status.svelte` 只在載入時與每 30 秒 `load()` 一次，`status.changed` 也不帶 uptime（`events::watch_status` 刻意排除）。改為前端以載入時的 `uptime_secs` 加上經過的時間每秒更新顯示，不必增加後端推送（2026-10-23：`src/lib/clock.ts::liveUptime`，以 `performance.now()` 計時；vitest `clock.test.ts`、Playwright U20 在 `admin.spec.ts`）
- [x] [Any] 明亮模式：`web-ui/src/app.css` 原本只跟隨作業系統（`prefers-color-scheme`），作業系統是深色時無法切到明亮。2026-10-23：頁首語言選單旁新增「外觀」選單（跟隨系統／明亮／深色），`src/lib/theme.ts` 存在 `localStorage` 的 `sipfax.theme`，`<html data-theme>` 切換 `app.css` 的顏色；`main.ts` 在掛載前套用（登入頁也有效）。vitest `theme.test.ts`、Playwright U21 `theme.spec.ts`
- [ ] [Opus] UI/UX 優化：先盤點各頁（登入、傳真列表、內容頁、設定、帳號、稽核、系統狀態）的問題並與使用者確認範圍，再分項實作；改完重跑手冊截圖（`scripts/e2e-web.sh --config playwright.shots.config.ts`）
  - 2026-10-23 盤點 17 項，使用者選第一批（分支 `ui-ux`）：
    - [x] 設定表單的儲存列（使用者回報「怪怪的」：灰色帶子沒貼齊卡片）：改為卡片的一部分、陰影；停用的主要按鈕改一般樣式；「有尚未儲存的變更」用警示色
    - [x] ① 頁首帳號選單（我的帳號／語言／外觀／登出；Esc、點外面關閉）U22
    - [x] ② 「傳真」旁的未讀數：`FaxStore::count_unread`、`GET /faxes/counts`（S05、W03b、A11），`src/lib/unread.svelte.ts` 依事件重讀 U22
    - [x] ⑤ 整列可點、⑥ 簡短時間 `formatWhen`（今天／昨天／月日）U24
    - [x] ⑨ 內容頁資訊欄（詳細資料、備註、投遞在預覽旁；「更多資訊」）U25
    - [x] ⑪ 需要重啟的欄位改為 ⟲ 標記與區段說明 U26
    - [x] ⑭ 新增帳號按按鈕才打開、⑮ 帳號列表按鈕對齊（`<td class="toolbar">` 是 flex，破壞表格）U23
    - 第二批（2026-10-23，分支 `ui-ux-2`，在 v0.4.1-alpha 之後）：
      - [x] ③ 拿掉連線圓點，中斷超過 3 秒才顯示「即時更新已中斷」U28
      - [x] ⑦ 列表欄寬：備註欄占剩下的寬度、其他欄不換行 U30
      - [x] ⑧ 空狀態：沒有傳真／篩選不到（附「清除篩選」）U29
      - [x] ⑩ 上一份／下一份傳真（`faxes.ts::neighbours`）U32
      - [x] ⑫ 離開前提醒未儲存（`src/lib/unsaved.ts`；路由的 `hashchange` 與 `beforeunload`；設定表單與備註）U31
      - [x] ⑬ 表單欄位寬度一致（28rem）
      - [x] ⑯ 系統狀態的今日概況與磁碟空間（`FaxStore::count_since`、`count_deliveries`；`GET /faxes/counts?since=`；`/status` 的 `storage`，rustix `statvfs`）U33
      - [x] ⑰ 登入頁：拿掉「請登入」、顯示密碼、語言移到下方 U27
      - [x] 手冊截圖重跑（登入頁、系統狀態的今日概況與磁碟、內容頁的上一份／下一份）；截圖抓到磁碟顯示「957531.1 MB」：`formatBytes` 補上 GB、TB
      - [x] `sip-core` E04 偶發失敗（CI run 39917590693：`dialog_count()` 讀到 1）：dialog 在 `bye()` 回 200 之後才從表中移除，測試改為最多等 1 秒（與同一測試的 task 數檢查相同做法）
      - [x] v0.4.1-alpha 的 release run 39915367748 中 U22 偶發失敗：帳號選單的「點外面關閉」要等 `toggle` 事件才掛上監聽器，打開後立刻點外面會漏掉。改為登入期間一直掛著、直接看 `details.open`。（那次 release 重跑失敗的 job 發佈，0.4.1 內仍有此問題，實際操作幾乎碰不到）
    - ④ 手機與窄螢幕版面：2026-10-23 使用者決定不做（介面以桌機瀏覽器為主）
    - [x] 手冊截圖（`docs/manual/images/`）重跑，座席與 IT 手冊內文同步（帳號選單、未讀數、整列可點、資訊欄、⟲ 標記、新增帳號按鈕）

### 文件（優先級在功能開發之後）
> 2026-10-22 使用者決定：使用手冊分「座席」與「IT」兩份；先寫 Markdown（`docs/manual/`，繁中），要 release 時轉成 PDF；正式 release 的 PDF 手冊要含截圖畫面（Web 的截圖可在 M4c 以 Playwright 自動產生）。
- [x] [Any] `CHANGELOG.md` 補上 M4a（設定存 DB、帳號、`config`／`user`／`fax` CLI）與 TUI，中英兩段（v0.3.0-alpha）
- [x] [Opus] 座席使用手冊 `docs/manual/agent.md`（2026-10-22，v0.3.0-alpha；Web 為主，附 TUI 按鍵）
- [x] [Opus] IT 安裝維運手冊 `docs/manual/it.md`（2026-10-22，v0.3.0-alpha：安裝、設定、gateway、資料庫、Web 與憑證、防火牆、systemd、log、帳號、投遞、清整、備份還原、升級、疑難排解）
- [ ] [Any] release 時把兩份手冊轉成 PDF；正式 release 的 PDF 要含截圖
  - 截圖已可自動產生：`scripts/e2e-web.sh --config playwright.shots.config.ts`（`web-ui/e2e/manual.shots.ts`，Docker 裡真的 sip-faxd 與 SpanDSP 傳真），輸出到 `docs/manual/images/`；介面改了就重跑

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
