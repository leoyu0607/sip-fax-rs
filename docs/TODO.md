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
- [ ] [Sonnet] （M1c）Dialog：UAS、in-dialog UAC（re-INVITE、BYE）、491、session timer、OPTIONS
- [x] [Sonnet] SDP 模型：audio 加 `m=image udptl t38`，含全部 T38Fax* 屬性（M1a；D01–D07 通過；待 Opus 審核）
- [ ] [Sonnet] fax-sim 的 SIP UAC 模式：INVITE、選擇由誰發起 re-INVITE、BYE
- [x] [Any] CI 新增涵蓋率門檻（`coverage` job，`cargo llvm-cov -p sip-core --fail-under-lines 85`）與 nightly fuzz workflow（`.github/workflows/fuzz.yml`，不在 push 觸發）；`scripts/branch-protection.json` 已加入 `coverage`，需由管理員重新套用（`scripts/set-branch-protection.sh`）
- [ ] [Opus] 審核 docs/test-reports/M1.md

## M2 — T.38 / T.30 / T.4（non-ECM）
- [ ] [Opus] 撰寫 docs/test-plans/M2.md
- [ ] [Sonnet] t38：IFP PER codec（v0–v3）、UDPTL redundancy 和 parity FEC，加上 proptest 和 fuzz
- [ ] [Sonnet] t30：接收端狀態機（DIS/DCS/TCF/CFR/FTT/MCF/RTN/MPS/EOP/DCN），以表格驅動測試
- [ ] [Sonnet] t4：MH/MR/MMR 解碼驗證、TIFF Class F 寫出、PDF 輸出
- [ ] [Sonnet] 第 2 層互通測試：與 SpanDSP `t38_terminal` 以 IFP 互打
- [ ] [Sonnet] fax-sim 加入 UDPTL redundancy/FEC 模型，補上 ECM + 封包遺失案例（多個 seed）（M0 審核追蹤 #1）
- [ ] [Sonnet] 端到端測試以 t4 解碼器逐像素比對收到的頁面（M0 審核追蹤 #2）
- [ ] [Sonnet] fax-in：串起 SIP 到 T.38、T.30 再到文件，並完成端到端多頁接收
- [ ] [Opus] 審核 docs/test-reports/M2.md

## M3 — Store / Delivery / ECM
- [ ] [Opus] 撰寫 docs/test-plans/M3.md
- [ ] [Sonnet] store：Repository traits、sqlx（SQLite/MySQL/PG）、tiberius（MSSQL）、migrations
- [ ] [Sonnet] delivery：folder、email（lettre、rustls）、webhook（reqwest、rustls），含重試佇列
- [ ] [Sonnet] t30 ECM（PPS/PPR、64 frame block）
- [ ] [Any] CI 新增 `db-matrix` job（MySQL、MariaDB、PostgreSQL、MSSQL 的 services）
- [ ] [Opus] 審核 docs/test-reports/M3.md

## M4 — 介面
- [ ] [Opus] 撰寫 docs/test-plans/M4.md
- [ ] [Sonnet] web：axum REST、WebSocket、驗證機制（argon2 加 session）
- [ ] [Sonnet] web-ui：Svelte + Vite SPA，以 rust-embed 內嵌
- [ ] [Sonnet] TUI（ratatui）和 CLI 子命令
- [ ] [Any] CI 新增 `web-ui` job
- [ ] [Opus] 審核 docs/test-reports/M4.md

## M5 — G.711 passthrough
- [ ] [Opus] 撰寫 docs/test-plans/M5.md
- [ ] [Sonnet] fax-modem：CNG/CED、V.21 HDLC、V.27ter、V.29、V.17（接收方向優先）
- [ ] [Sonnet] RTP 接收 payload stream 和 jitter buffer
- [ ] [Opus] 審核 docs/test-reports/M5.md

## M6 — 互通強化與發佈
- [ ] [Opus] 撰寫 docs/test-plans/M6.md
- [ ] [Sonnet] 建立 Sonus/AudioCodes SIP trace 回歸測試集
- [ ] [Sonnet] 打包時加入 systemd unit 範例和 conf 範例
- [ ] [Any] 打 v0.1.0 tag，產生 Release
- [ ] [Opus] 審核 docs/test-reports/M6.md
