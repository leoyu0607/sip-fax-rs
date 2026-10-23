# Roadmap

狀態圖例：⬜ 未開始　🟨 進行中　✅ 完成（測試報告經 Opus 審核通過）

| # | 狀態 | 內容 | 完成標準 |
|---|---|---|---|
| M0 | ✅ | Repo、文件、workspace 骨架、CI、musl 建置、Rocky 9 測試容器、fax-sim 雛形（SpanDSP 綁定可運作）| CI 全綠；`sip-faxd` 是靜態連結；fax-sim 能在容器內呼叫 SpanDSP |
| M1 | ✅ | sip-core：message/transport/transaction/UAS dialog/SDP-T.38/re-INVITE | fax-sim 能完成 INVITE→200→ACK→re-INVITE(T.38)→BYE，包含重送情境 |
| M2 | ✅ | t38 + t30（non-ECM）+ t4 → TIFF/PDF | 與 SpanDSP 互通，端到端收到多頁傳真 |
| M3 | ✅ | store（4 種 DB）+ delivery（folder/email/webhook）+ ECM | 各 DB 在容器中的整合測試通過；投遞重試正確 |
| M4 | ✅ | Web 管理介面 + TUI + CLI 完整（M4a–M4f） | 可查詢和下載傳真、管理路由與設定 |
| M5 | ⬜ | fax-modem：G.711 passthrough | fax-sim 以 G.711 模式送傳真，可以成功接收 |
| M6 | ⬜ | Gateway 互通強化（Sonus/AudioCodes 參數、SIP trace 回歸測試）、打包發佈 | 解壓後在 RHEL 9/10 可直接執行 |

## 主要風險
- 純 Rust 的 T.30 和 ECM 需要細心對照 SpanDSP 行為。以互通測試（第 2 層）持續驗證。
- 純 Rust 的 V.17/V.29 數據機（M5）工程量最大，排在最後。完成之前，G.711 進線會明確標記為不支援。
- MSSQL 走獨立實作，需要維護兩套 SQL。以 Repository trait 隔離，並共用同一份整合測試案例。

## 發佈紀錄
詳見 [CHANGELOG.md](../CHANGELOG.md)。

| 版本 | 日期 | 內容 |
|---|---|---|
| v0.1.0-alpha | 2026-10-21 | 第一個現場試接版（T.38 收傳真） |
| v0.1.1-alpha | 2026-10-21 | 修正 AudioCodes 多頁傳真（F1）、修正每頁的壞行、壓縮檔擁有者 |
| v0.2.0-alpha | 2026-10-21 | M3：資料庫紀錄（4 種 DB）、清整、投遞（資料夾／Email／Webhook）、ECM（預設關閉）、PNG 預覽函式庫 |
| v0.3.0-alpha | 2026-10-22 | M4：Web 介面（內建、繁中／英文、即時更新）、帳號、設定存資料庫並可線上修改、服務管理（start／stop／status／logs）、TUI |
| v0.4.0-alpha | 2026-10-23 | M4e／M4f：`db copy` 換資料庫、`[database]` 分欄位與 `db check`、設定檔只留啟動設定；Web 的外觀切換（明亮／深色）、已執行時間每秒更新 |
| v0.4.1-alpha | 2026-10-23 | Web 介面改善：帳號選單、未讀數、整列可點、簡短時間、傳真內容的資訊欄、需要重啟的設定標記、新增帳號表單、儲存列與帳號列表的版面修正 |
