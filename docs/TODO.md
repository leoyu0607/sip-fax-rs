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
- [x] [Sonnet] 以 t4 解碼器逐像素比對收到的頁面（M0 審核追蹤 #2；M2c 的 X 測試已做；SIP 端到端版本在 M2d）
- [ ] [Sonnet] fax-in：串起 SIP 到 T.38、T.30 再到文件，並完成端到端多頁接收
- [ ] [Opus] 審核 docs/test-reports/M2.md

- [x] [Sonnet] 調查 sip-core 涵蓋率一次掉到 83.5% 的不穩定測試（M2a 審核；M2c 結論見報告第 6 節）
- [x] [Sonnet] t4 在小量餵入時的效能，以實際 T.38 封包大小量測（M2b 審核；M2c：最差 1 byte 一包也只用線路時間的 0.7 %，不需要改成可續解）

## M3 — Store / Delivery / ECM
- [ ] [Opus] 取得 ITU-T T.30 原文，重新核對 DIS/DCS 的 bit 位置（V.33、掃描時間碼）（M2c 審核）
- [ ] [Sonnet] ECM 下的 MMR 互通測試（補 X01 的 MMR 格）（M2c 審核）
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
- [ ] [Any] 用 CS210 的實際擷取驗證 localTCF 時是否送 CFR，並調整 `cht-cs210` 預設組（M2c 審核）
- [ ] [Sonnet] K01 的根本解：dialog 改用單一有序事件佇列（ADR-016）
- [ ] [Opus] 撰寫 docs/test-plans/M6.md
- [ ] [Sonnet] 建立 Sonus/AudioCodes SIP trace 回歸測試集
- [ ] [Sonnet] 打包時加入 systemd unit 範例和 conf 範例
- [ ] [Any] 打 v0.1.0 tag，產生 Release
- [ ] [Opus] 審核 docs/test-reports/M6.md
