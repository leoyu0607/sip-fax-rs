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
scripts/e2e-docker.sh            # 在 Rocky 9 容器內跑 tools/ workspace（SpanDSP 模擬）的 fmt/clippy/test
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
- 協定解析器（SIP parser、IFP/PER、UDPTL、T.4）需要有 proptest 屬性測試和 cargo-fuzz 目標。
- `sip-core` **不可依賴**任何 fax crate，因為它未來會拆成獨立專案。
- 全面使用 rustls，禁止引入 OpenSSL 或 native-tls（`deny.toml` 會擋）。
- SpanDSP 只能出現在 `tools/fax-sim` 和測試中，不可進入正式執行檔。
- 進度或決策有變動時，同一個 commit 要一併更新 `docs/TODO.md`、`docs/ROADMAP.md`、`docs/DECISIONS.md`。
- 從 sipress 抽取程式碼時，必須記錄在 `docs/sipress-extraction.md`。
- 文件語言：README 有英文版（`README.md`）和繁中版（`README.zh-TW.md`），兩份要同步更新；docs/ 下以繁體中文撰寫。
