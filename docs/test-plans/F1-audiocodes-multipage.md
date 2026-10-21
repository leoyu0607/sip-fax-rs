# F1：AudioCodes 多頁傳真失敗（現場問題 #1）

- 撰寫：Opus
- 執行：Sonnet
- 審核：Opus
- 現場資料：`tools/fax-sim/tests/traces/audiocodes-m1000-6.80A-8page-fail.{udptl.log,sip.log,result.json}`。這次是 AudioCodes Mediant 1000 v6.80A 送 8 頁，結果只收到 1 頁，原因碼 `t2_expired_image`。

## 根因分析（Opus，依 udptl.log）
T.30 FCF 依 ADR-019 的慣例表示：0xC1=DCS、0x21=CFR、0xF2=MPS（X=1）、0x31=MCF、0x5F=DCN。

| 時間 | 事件 |
|---|---|
| 24.429 | 第 1 頁資料結束（T4NonEcmSigEnd） |
| 24.53 | 對方送 V21Preamble |
| 25.649 | 收到 **MPS**，封包型別是 `HdlcFcsOk`，**還沒有 sig-end** |
| **25.724** | **我方就開始送 V21Preamble**（準備回 MCF） |
| 25.790 | 對方才送出 `HdlcSigEnd` |
| 25.809 | 對方送 NoSignal |
| 26.725–26.804 | 我方送出 MCF |
| 29.910 | 對方**重送 MPS**，代表它沒有收到我方的 MCF（大約是 T4 = 3s 之後） |
| 31.050 → **31.125** | 我方再次在對方 sig-end（31.169）**之前**就開始回應，**又重演一次** |
| 32.8 左右 | 從**第一次** MCF 起算的 T2（6s）到期 → 我方送 DCN（33.1），通話結束 |

**根因 1（主因）**：我方在收到 `HdlcFcsOk` 時就開始回應，沒有等對方的 V.21 訊號結束（`HdlcSigEnd` 或 NoSignal）。
- SpanDSP 一律送合併的 `HdlcFcsOkSigEnd`，所以 M2 的所有測試都碰不到這種情況。
- AudioCodes 會把 FCS-OK 和 sig-end **分成兩個封包**，中間隔 120–140ms。
- gateway 是半雙工，還在送 V.21 時收到的 preamble 會被丟掉。因此我方的 MCF 沒有被調變出去，傳真機收不到。
- （DCS 之後同樣是分開送的，但因為後面接著 TCF，那時我方不需要回應，所以沒出事。）

**根因 2（次因）**：對方重送 post-message command（MPS/EOP/EOM）時，我方雖然重送了 MCF，卻**沒有重新啟動 T2**。結果 T2 從第一次 MCF 起算，在對方第二次重送期間就到期了。

**待查**：第 1 頁有 `bad_lines=1`，但完全沒有掉封包。
- 可能是頁首或頁尾的處理問題（例如 RTC 前的 fill，或第一個 EOL），也可能是 AudioCodes 的資料本身就有一行壞掉。
- summary 模式的擷取沒有影像資料，無法重現。需要請使用者用 `udptl = "full"` 再送一次。

## 測試案例
| ID | 案例 | 預期 |
|---|---|---|
| F1-01 | 引擎單元測試：對方先送 `HdlcFcsOk`，間隔 140ms 才送 `HdlcSigEnd`，再隔 20ms 送 NoSignal | 我方的 V21Preamble 在 sig-end 或 NoSignal **之後**，至少再等 turnaround 延遲（預設 75ms）才送出。斷言精確的毫秒時間點 |
| F1-02 | 同 F1-01，但對方一直沒送 sig-end，也沒送 NoSignal（異常情況）| 經過可設定的保底等待時間（預設 500ms，依 V.21 frame 長度推算後記錄理由）後仍然回應，不會卡死 |
| F1-03 | T.30：送出 MCF 後收到重複的 MPS（以及 EOP、EOM）| 重送 MCF，**並重新啟動 T2**；之後收到下一頁就能正常接收。斷言 T2 的到期時間點 |
| F1-04 | **現場 trace 重播**：解析 `audiocodes-...-fail.udptl.log` 的 RX 時間軸（indicator、HDLC、sig-end 的時間點和分包方式；影像資料以已知測試頁替代），把它當作對方餵給引擎 | 修正前會重現 `t2_expired_image`（**必須先證明這個測試能重現失敗**）；修正後完成多頁接收 |
| F1-05 | fax-sim 加入 `gateway_quirks.split_fcs_sig_end = 140ms`，模擬 AudioCodes 把 FCS-OK 和 sig-end 分開送；E2E 由 SpanDSP 經 SIP/T.38 送 **8 頁** | 成功，8 頁逐像素相同（libtiff 和 poppler 各比對一次，附反例檢查）|
| F1-06 | 回歸測試 | 所有既有的 R、X、E 測試仍然通過，時序沒有變慢到會破壞 SpanDSP 互通 |

## 驗收
- `scripts/ci-local.sh` 全部通過。
- F1-04 要附上修正前失敗、修正後成功的兩份輸出。
- 發佈 `v0.1.1-alpha`。
