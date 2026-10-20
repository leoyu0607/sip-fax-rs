# 專案目標

## 原始需求（使用者原文）
1. 以 rust 開發，需要有簡易 cmd 跟 tui，另外要有 web 管理介面
2. 基本架構為三大模塊：1.sip 核心引擎, 2.fax-in 模組, 3.fax-out 模組
3. sip 核心引擎可以從 sipress 專案的核心中抽取可用部分，這部份未來可能會變成獨立專案
4. 先做 fax-in，fax-out 未來再考慮
5. 先預設來源端是 voip gateway（sonus、audiocode...）
6. 先預設是 T.38（G.711 passthrough 為備援方案）
7. 開發過程中需為專案做測試，測試方案為使用 SpanDSP 做模擬 fax 進線
8. 傳真結果及設定需存在 DB，本機預設 SQLite，但須支援常見 DB 如：MSSQL、MySQL/MariaDB、PostgreSQL
9. 程式期望部署方式為解壓縮目錄即可使用，應採用靜態編譯（RHEL 9/10）

## 追加要求
- 在 GitHub 建立 repo `leoyu0607/sip-fax-rs`（public）。README.md 用英文寫，並提供連結切換到繁中版 README.zh-TW.md。
- 測試要盡可能完善且嚴謹：由 Opus 建立測試計畫，Sonnet 執行測試，Opus 監督並審核結果。
- GitHub Actions 建立 CI，每次 push 都必須通過。
- 專案目標和待辦事項都要存成文件，方便後續接手的 agent。

## 已確認的決策（2026-10-20）
| 項目 | 決策 |
|---|---|
| 傳真協定層（T.30/T.38/T.4、G.711 數據機） | **純 Rust 自行實作**；SpanDSP 只用於測試端 |
| sipress | https://github.com/leoyu0607/Sipress（MIT）。只抽取可用的葉節點模組，SIP 堆疊主體重寫（見 DECISIONS ADR-001） |
| Fax-in 投遞方式（第一版） | 存檔+DB、Email、HTTP Webhook、資料夾投遞（全部都要） |
| Web 前端 | axum 後端 + 內嵌 SPA（Svelte + Vite，rust-embed 打包進執行檔） |
| 授權 | MIT |
| Repo 可見性 | public |

## 非目標（目前）
- fax-out（送出傳真）
- SIP TLS/SRTP（介面先預留）
- 一般 SIP 終端（非 gateway）的相容性最佳化
