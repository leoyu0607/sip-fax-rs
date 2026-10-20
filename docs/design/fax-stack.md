# 傳真協定層設計規格（M2：t38、t4、t30、fax-in 整合）

- 撰寫：Opus
- 實作者依本規格實作。要偏離時，先在 `docs/DECISIONS.md` 追加 ADR。

## 0. 原則
- 三個 crate 都是**純 Rust、不做 I/O 的協定核心**：輸入 bytes 或事件，輸出 bytes 或動作。I/O 和計時由 fax-in 以 tokio 驅動。
- 不得依賴任何 C 函式庫。SpanDSP、libtiff、poppler 只能出現在測試中，作為**獨立的驗證來源**。
- 解析任何外部輸入都不可以 panic，要搭配 proptest 和 cargo-fuzz。
- 涵蓋率門檻：`t38`、`t4`、`t30` 的 line coverage 都要 ≥ 85%。

## 1. t38 crate
### 1.1 IFP（ITU-T T.38 第 7 節與 ASN.1 附錄）
- 實作 IFP 封包的 PER 編解碼，涵蓋 T.38 **version 0 到 3**：
  - `type-of-msg`：`t30-indicator` 或 `t30-data`
  - `data-field`：`SEQUENCE OF { field-type, field-data OPTIONAL }`
- **v0 和 v1+ 的差異**要依標準正確處理：
  - extension marker
  - v2 以後新增的 V.8、V.34 indicator 和 data type
  - v2 以後新增的 CM/JM/CI/V34rate field type
- **型別**：
  - `Indicator`（no-signal、cng、ced、v21-preamble、v27-2400/4800-training、v29-7200/9600-training、v17-7200/9600/12000/14400 short/long training，以及 v1+ 的 v8/v34 系列）
  - `DataType`（v21、v27-2400/4800、v29-7200/9600、v17-7200/9600/12000/14400，以及 v1+ 的 v8、v34 系列）
  - `FieldType`（hdlc-data、hdlc-sig-end、hdlc-fcs-OK、hdlc-fcs-BAD、hdlc-fcs-OK-sig-end、hdlc-fcs-BAD-sig-end、t4-non-ecm-data、t4-non-ecm-sig-end，以及 v2+ 的 cm/jm/ci/v34rate）
- **黃金向量**：M0 的 fax-sim loopback 已經能擷取 SpanDSP 產生的 IFP 封包，用它建立測試向量（`crates/t38/tests/vectors/spandsp/*.ifp`）。我們解碼後再編碼，結果必須**逐 byte 相同**。這是 PER 實作正確性的主要依據。
### 1.2 UDPTL（T.38 附錄 / 9.1）
- `UDPTLPacket { seq-number(0..65535), primary-ifp-packet, error-recovery }`
- `error-recovery` 有兩種形式：
  - `secondary-ifp-packets`（redundancy）
  - `fec-info { fec-npackets, fec-data }`（parity FEC，以 XOR 計算）
- **送出端**：可以設定 redundancy 深度（預設 3）或 FEC 參數（span、entries）。
- **接收端**（`UdptlReceiver`）：
  - 依 seq 排序後交付；重複的封包丟棄。
  - 以 redundancy 或 FEC 補回遺失的封包。
  - 無法補回的缺口以 `Gap{count}` 事件交給上層，不可以卡住。
  - 要正確處理 seq 在 65535 繞回 0 的情況。
  - 封包亂序的容忍視窗可以設定（預設 32）。
- 上層要求我方送 redundancy 或 FEC 時都照做；接收端則**不論對方送哪一種格式都要接受**，依封包自己的 CHOICE 判斷。

## 2. t4 crate
- **解碼器**：串流式，逐行輸出 1-bit 的 row。
  - 支援 MH（T.4 1D）、MR（T.4 2D，K=2 或 4）、MMR（T.6）。
  - 寬度支援 1728、2048、2432 pixel。
  - 要處理 EOL、fill bits，並偵測 RTC（6 個 EOL）和 EOFB。
  - **非 ECM 的錯誤處理**：解碼失敗的行，用上一行替代，並計入 `bad_lines`；之後在下一個 EOL 重新同步。MMR 沒有 EOL，遇到錯誤就放棄該頁剩下的部分，並把錯誤回報上層。
  - 統計 `rows`、`bad_lines`、`longest_bad_run`，提供 T.30 判斷要回 MCF 還是 RTN。
- **編碼器**：MMR（T.6），用來把收到的頁面重新編碼成乾淨的 G4。
- **TIFF 寫出**：多頁 TIFF Class F，壓縮為 G4（Compression=4）。tag 包含：
  - XResolution 204、YResolution 98 或 196、ResolutionUnit inch
  - PageNumber、FillOrder=1、Photometric=MinIsWhite
  - Software、DateTime
  - ImageDescription：記錄對方的 TSI
- **PDF 寫出**：每一頁以 `/Filter /CCITTFaxDecode /DecodeParms << /K -1 /Columns W /Rows H /BlackIs1 false >>` 直接嵌入 G4 資料。頁面大小依解析度換算（寬 = W/204 英吋，高 = H/(98 或 196) 英吋）。手寫最小合法 PDF，xref 必須正確。

## 3. t30 crate（M2 只做接收端、non-ECM，ECM 留到 M3）
- **角色**：被叫端（answering terminal），也就是接收方。
- **狀態機**：純函式。`on_event(Event) -> Vec<Action>`，事件和動作的種類如下：

  | 方向 | 種類 |
  |---|---|
  | 事件（輸入） | 收到 HDLC frame（含 FCS OK/BAD）、收到 indicator、收到 non-ECM 影像資料、資料結束、計時器到期、上層要求中止 |
  | 動作（輸出） | 送 indicator、送 HDLC frame、啟動/取消計時器、輸出頁面資料、完成（結果碼）|

- **Phase B**
  - 開始時送 CED indicator，接著送 v21-preamble，再送 CSI 加 DIS frame。
  - DIS 內容依設定：
    - 調變：V.27ter、V.29、V.17
    - 解析度：standard、fine
    - 編碼：MH、MR、MMR
    - 紙寬：A4
    - ECM：M2 先關閉
  - 收到 DCS（含 TSI）後，依它的內容設定接收參數。
- **TCF**
  - `transferredTCF`：接收 t4-non-ecm-data，依實收長度和全 0 的比例判斷（門檻可設定，預設 ≥ 95% 為 0，長度 ≥ 1.2 秒），通過就回 CFR，否則回 FTT。
  - `localTCF`：不會收到 TCF，收到 DCS 後直接回 CFR。
- **Phase C**：收到 training indicator 後，把 t4-non-ecm-data 交給 t4 解碼器，直到 sig-end 或 RTC。
- **Phase D**：收到 MPS、EOP 或 EOM 時，依 bad_lines 的比例回 MCF、RTN 或 RTP。門檻可設定：預設 bad_lines 超過 10% 或連續 bad 超過 2.5 mm（standard 約 10 行）時回 RTN。
  - MPS：進入下一頁。
  - EOP：回 MCF 後等 DCN。
  - EOM：回到 Phase B。
- **計時器**：T1 = 35s、T2 = 6s、T4 = 3s，都可以設定。DIS 沒有回應時，每隔 T4 重送，最多 3 次，之後以失敗結束。
- **其他處理**：
  - 任何時候收到 DCN，都要正確結束；若當時還在收頁面，就依已完成的頁面回報部分成功。
  - 收到 CRP 時，重送上一個回應。
  - 收到無法預期的 frame 要記錄下來；嚴重的話回 DCN。
- **結果**：

  | 欄位 | 內容 |
  |---|---|
  | `code` | 0 表示成功，失敗碼另外定義成 enum |
  | `pages` | 頁數 |
  | `bit_rate` | 速率 |
  | `resolution` | 解析度 |
  | `compression` | 編碼方式 |
  | `remote_ident` | 對方的 TSI |
  | `bad_lines_per_page` | 每頁的 bad lines 數 |
  | `duration` | 通話長度 |

- **T.38 傳送節奏**：
  - HDLC 和 indicator 依 T.38 的建議時間間隔送出，由 fax-in 的時鐘驅動。例如送 v21-preamble 後等 ≥ 75ms 才送 frame。
  - indicator 要重送幾次可以設定（預設 3），用來對付 UDP 遺失。

## 4. fax-in 整合
- `[media] port_range = "30000-30999"` 取代單一的 `t38_port`。每通電話配置一個偶數埠，通話結束時釋放。埠用完時，對新的來電回 **503**，並記錄錯誤。
- `T38FaxSession` 實作 `FaxSession` trait：
  - 綁定 UDPTL socket（latching：第一個收到的封包決定對方的實際位址）。
  - 依 SDP 協商出的參數設定 UDPTL 和 T.38 版本。
  - 由 tokio 驅動 t30 的計時器和送出節奏。
- 收完後寫出 `data/faxes/<YYYY>/<MM>/<DD>/<call-id>.tif` 和同名的 `.pdf`。CallResult 加入 t30 的結果欄位。
- 擷取：`logs/calls/<call-id>/udptl.log`，每個封包一行，內容是時間戳、方向、seq 和 hex。
- 一併修正 M1d 審核列出的競態問題：re-INVITE 的 200 已經收到，就要回報 Accepted。
