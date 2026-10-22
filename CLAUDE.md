# CLAUDE.md — agent 入口

sip-fax-rs 是以 Rust 開發的 SIP 傳真伺服器。目前只做 fax-in：
- 來源端是 VoIP gateway
- 預設 T.38，G.711 passthrough 為備援
- 結果存 DB，並透過 Email/Webhook/資料夾投遞
- 以靜態 musl 執行檔發佈到 RHEL 9/10

## 開始工作前必讀（依序）
1. `docs/TODO.md`：目前進度和下一步待辦
2. `docs/GOALS.md`：原始需求與已確認的決策（不得擅自推翻）
3. `docs/ARCHITECTURE.md`：crate 分工與設計
4. `docs/DECISIONS.md`：決策紀錄（ADR）
5. `docs/ROADMAP.md`：里程碑與完成標準

## 常用指令
```sh
cargo build
cargo test --workspace --all-targets
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all
scripts/ci-local.sh              # push 前必跑，與 CI 必要檢查一致（有 Docker 時會一併跑 e2e-docker.sh）
scripts/e2e-docker.sh            # 在 Rocky 9 容器內跑 tools/ workspace：SpanDSP 模擬、fax-sim SIP UAC、真實 sip-faxd 對 SIPp 的互通測試（tools/sipp/*.xml、tools/interop），以及 M2d 的 E01–E07（fax-sim 當 gateway 用 SpanDSP 即時送傳真給 sip-faxd，tools/fax-sim/tests/e2e_m2d.rs，約 1 分鐘）
sip-faxd serve --config conf/sip-faxd.toml     # 執行（預設路徑：執行檔旁的 conf/sip-faxd.toml）
sip-faxd config check --config conf/sip-faxd.toml.example   # 驗證設定檔
cargo llvm-cov -p sip-core --fail-under-lines 85 && cargo llvm-cov -p fax-in --fail-under-lines 70   # 涵蓋率門檻（協定 crate t38、t4、t30 同為 85）
cargo zigbuild --release --target x86_64-unknown-linux-musl -p sip-faxd
scripts/package.sh x86_64-unknown-linux-musl   # 產生 dist/*.tar.gz
```

## 開發規範
- **每次 push 都必須通過 CI**（`.github/workflows/ci.yml`）。push 前先執行 `scripts/ci-local.sh`。
- **測試治理流程**：每個里程碑都要走完以下四步。
  1. Opus 撰寫測試計畫，放在 `docs/test-plans/M<n>.md`。
  2. Sonnet subagent（`model: "sonnet"`）依計畫實作並執行測試，報告放在 `docs/test-reports/M<n>.md`。
  3. Opus 審核：檢查是否有空斷言、過度 mock、漏測，以及涵蓋率門檻（協定 crate 85% 以上，其餘 70% 以上）。不合格就退回修正。
  4. 審核通過後才勾選里程碑。
- **Subagent 只用在測試**（2026-10-22 使用者決定，為了節省額度）：功能開發、修 bug、整合、文件都由主 session 直接做，不要派 subagent（包括 Explore、Plan、general-purpose 等）。只有上面測試治理流程的第 2 步才派**一個** Sonnet subagent，prompt 要精簡、範圍明確；不要同時開多個 subagent，也不要用 Workflow 平行展開。
- **每完成一個階段就 commit 並 push** 工作分支，並在 `docs/TODO.md` 對應段落記下進度與接手要點，讓另一個 agent 隨時能接手。
- 協定解析器（SIP parser、IFP/PER、UDPTL、T.4）需要有 proptest 屬性測試和 cargo-fuzz 目標。
- `sip-core` **不可依賴**任何 fax crate，因為它未來會拆成獨立專案。
- 全面使用 rustls，禁止引入 OpenSSL 或 native-tls（`deny.toml` 會擋）。
- SpanDSP 只能出現在 `tools/fax-sim` 和測試中，不可進入正式執行檔。
- 進度或決策有變動時，同一個 commit 要一併更新 `docs/TODO.md`、`docs/ROADMAP.md`、`docs/DECISIONS.md`。
- 從 sipress 抽取程式碼時，必須記錄在 `docs/sipress-extraction.md`。
- 文件語言：README 有英文版（`README.md`）和繁中版（`README.zh-TW.md`），兩份要同步更新；docs/ 下以繁體中文撰寫。

## 發版檢查清單（每次打 tag 前都要完成，並在同一個 commit 一起更新）
1. 更新 `Cargo.toml`（以及 `tools/Cargo.toml`）的 `version`。
2. 把 `CHANGELOG.md` 的 `[Unreleased]` 改成新版本號和日期，英文和繁中兩段都要寫。
3. 更新兩份 README 頂部的**狀態**段落（`README.md`、`README.zh-TW.md` 要同步）。
4. 在 `docs/ROADMAP.md` 的「發佈紀錄」表格加上一列；里程碑狀態有變動時一併更新。
5. 影響部署或試接時，同步更新 `docs/field-test.md` 和 `conf/sip-faxd.toml.example`。
6. 在 `docs/TODO.md` 記錄這次發佈。
7. 先跑 `scripts/ci-local.sh`，push 後等 CI 全綠，才打 tag。
