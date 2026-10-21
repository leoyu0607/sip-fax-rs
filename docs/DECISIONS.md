# 決策紀錄（ADR）

新增決策時，往檔案最後追加一則，格式為：背景 / 決策 / 影響。

## ADR-001：sipress 只抽取葉節點模組，SIP 堆疊重寫（2026-10-20）
**背景**

sipress-core 是為壓力測試設計的字串模板式 UAC 工具，有以下限制：
- 沒有訊息模型、transaction layer，也沒有監聽端 transport
- UAS 不會重送 2xx
- 發 BYE 時忽略 Record-Route
- 無法主動發出 re-INVITE
- 完全沒有 T.38

**決策**

| 處理方式 | 項目 |
|---|---|
| 抽取 | `net.rs`、`take_frame`、`DigestChallenge`、G.711 編解碼、`RtpPacket`（修正 decode） |
| 只作參考 | `message.rs` 的 UAS 回應鏡像、`ack_for_non2xx` |
| 重寫 | 其餘部分 |

詳見 `sipress-extraction.md`。

**影響**

M1 的工作量增加，但換來可以獨立成專案的乾淨 SIP 核心。

## ADR-002：傳真協定層以純 Rust 實作（2026-10-20）
**決策**

T.38 IFP/UDPTL、T.30、T.4/T.6 和 G.711 數據機全部自行實作。SpanDSP 只用在測試端。

**影響**
- 正式執行檔沒有 C 相依，靜態編譯單純。
- 數據機（M5）工程量大，排到最後。
- 以 SpanDSP 互通測試確保行為正確。

## ADR-003：DB 層採 sqlx + tiberius（2026-10-20）
**背景**

sqlx 0.7 之後不支援 MSSQL。

**決策**

| DB | 實作 |
|---|---|
| SQLite、MySQL/MariaDB、PostgreSQL | sqlx |
| MSSQL | tiberius |

兩者都以 Repository trait 隔離，各方言各有一套 migrations，並共用整合測試案例。

## ADR-004：靜態編譯採 musl + cargo-zigbuild + rustls（2026-10-20）
**決策**
- 目標平台：`x86_64`/`aarch64-unknown-linux-musl`
- 禁用 OpenSSL 和 native-tls，由 `deny.toml` 強制檢查
- SQLite 使用 bundled 版本
- 在 musl 上改用 mimalloc 彌補配置器效能

**驗證**

CI 的 `build-musl` 檢查執行檔是否 statically linked；`smoke-rhel` 在 Rocky 9 和 Alma 10 容器中解壓後執行。

## ADR-005：Web 採 axum + 內嵌 Svelte SPA（2026-10-20）
**決策**

SPA build 的產物以 rust-embed 打包進執行檔，部署時仍然只有一個 binary。

## ADR-006：測試治理與 CI 閘門（2026-10-20）
**決策**
- 每個里程碑都由 Opus 撰寫測試計畫，Sonnet 實作並執行，Opus 審核。
- 每次 push 都必須通過 CI。
- 必要檢查會隨里程碑逐步增加：e2e-spandsp、涵蓋率門檻、db-matrix、web-ui。
- fuzz 以 nightly 排程執行，不阻擋 push。

## ADR-007：測試用 SpanDSP 從原始碼建置，FFI 手寫不用 bindgen（2026-10-20）
**背景**

Rocky 9 的 EPEL 只提供 spandsp 0.0.6，沒有 T.38 terminal 與現行 T.30 API（需要 3.x）。

**決策**
- `docker/test-rocky9.Dockerfile` 從 upstream tag `v3.1.1` 建置 SpanDSP 並安裝到 `/usr/local`（共用函式庫在 `/usr/local/lib64`）。SpanDSP 只用於測試工具，不進入正式執行檔。
- `tools/spandsp-sys` 以手寫 FFI 取代 bindgen：所有 SpanDSP 狀態型別都當 opaque pointer，`*_init` 傳 NULL 讓 SpanDSP 自行配置，需要的函式只有約 30 個，因此不需要 libclang，也不必處理標頭版本差異。build.rs 以 `pkg-config` 探測 `spandsp >= 3.1`，找不到時指向 `scripts/e2e-docker.sh`。
- `tools/` 是獨立 cargo workspace（根 `Cargo.toml` 以 `exclude` 排除），使沒有 SpanDSP 的主機仍可建置主專案。

**代價**

簽名需人工對照標頭（已在容器內 grep 驗證 v3.1.1）；升級 SpanDSP 版本時要重新核對。S01–S10 的實際呼叫會在簽名不符時失敗。

## ADR-008：sip-core 的依賴（2026-10-20）
**背景**

`docs/design/sip-core.md` 第 0 節限定 sip-core 只能依賴 `tokio`、`bytes`、`thiserror`、`tracing`、`rand`。從 sipress 抽取的 Digest 認證（`auth.rs`）需要 MD5，sipress 使用 `md-5` crate。

**決策**
- 新增 `md-5`（RustCrypto，純 Rust，MIT/Apache-2.0，不引入 OpenSSL）。
- 所有依賴版本放在根 `Cargo.toml` 的 `[workspace.dependencies]`：`tokio`、`bytes`、`thiserror`、`tracing`、`rand`（0.9）、`md-5`，開發用 `proptest`。
- M1a（純函式部分）實際只使用 `bytes`、`thiserror`、`rand`、`md-5`；`tokio`、`tracing` 先宣告於 workspace，M1b（transport）才在 sip-core 啟用。
- fuzz 目標位於 `crates/sip-core/fuzz/`，自成一個 workspace（`libfuzzer-sys` 只在那裡），主 workspace 以 `exclude` 排除，不影響 stable 建置和 CI。

## ADR-009：非 sip/sips/tel 的 URI 以不透明方式保留（2026-10-20）
**背景**

設計規格第 1 節寫「`SipUri`（sip/sips/tel 以外拒絕）」，但 RFC 4475 3.3.2（`unkscm`）、3.3.3（`novelsc`）、3.3.4（`unksm2`）要求 parser 必須接受 request-line 和 To/From/Contact 中的其他 URI scheme（`nobodyKnowsThisScheme:`、`soap.beep://`、`isbn:`、`http:`、`name:`），由上層回 416 等。

**決策**
- `SipUri::parse` 仍然只接受 sip/sips/tel，其他 scheme 回錯誤（符合規格原文）。
- 新增 `Uri` enum：`Uri::Sip(SipUri)` 與 `Uri::Other { scheme, rest }`（語法合法、不解析內容）。`Request::uri` 與 `NameAddr::uri` 使用 `Uri`。是否拒絕該 scheme 由 UAS／上層決定。
- 規格第 2 節「必要 header 缺少回 ParseError」與 RFC 4475 3.4.1（`inv2543`，無 Max-Forwards）衝突：M1a 先依規格拒絕；**已被 ADR-010 取代**（改為容忍）。

**代價**

呼叫端要多一次 `as_sip()`；但避免 parser 層丟掉合法訊息。

## ADR-010：Max-Forwards 在 parser 層為選用；RFC 6026 下 2xx 的責任分工（2026-10-20）
**背景**

M1a 審核指出：規格第 9 節要求寬容對待沒有 `Max-Forwards` 的 request（中華電信 CS210 沒有規格文件，無法排除），parser 層不應拒絕。

**決策**
- parser 不再因缺少 `Max-Forwards` 回 `ParseError`；`Headers::max_forwards()` 回傳 `Result<Option<u8>, _>`，值格式錯誤或重複仍然拒絕。缺少時如何處理（例如回 483 或補預設值）由 UAS／上層依旗標決定。`inv2543` 因此改列為「可容忍」。
- INVITE client transaction 在 Accepted 狀態（RFC 6026 Timer M = 64·T1）吸收重送的 2xx，不重複上報 TU（測試計畫 T09）。**此項已被 ADR-011 取代**（改以 `TuEvent::Retransmitted2xx` 上報）。因此 **Dialog 層（M1c）必須自行處理重送 2xx 的 re-ACK**（例如以 `TuInput::StrayResponse` 在 transaction 結束後收到的 2xx，並在 M1c 評估是否需要新增「重複 2xx」事件）。
- INVITE server transaction 在 Accepted 狀態不重送 2xx，2xx 的重送（13.3.1.4）由 Dialog 層負責。
- sip-core 的 dev-dependency 啟用 `tokio` 的 `test-util` feature（只用於測試的 paused time），不新增 crate。


## ADR-011：M1c 的 Dialog／Endpoint 設計決定（2026-10-20）
**背景**

M1b 審核（見 docs/test-reports/M1.md「M1b — Opus 審核結論」）要求在 M1c 修正 2xx 重送的 ACK、re-INVITE 應用層逾時、註冊的 423／Retry-After，以及對無法解析的 request 回 400。實作 Dialog 與 Endpoint 時另有幾個規格沒有寫死的選擇。**沒有新增任何依賴**（沿用 ADR-008）。

**決策**
- **取代 ADR-010 的一部分**：INVITE client transaction 在 Accepted 狀態收到重送的 2xx 時，改以 `TuEvent::Retransmitted2xx(Response)` 上報（第一個 2xx 仍是 `TuEvent::Response`）。Dialog 層重送快取的 ACK（同一組 bytes）。Timer M 之後才到的 2xx 走 `TuInput::StrayResponse`，由 endpoint 依 Call-ID／tag 找到 dialog 後同樣重送 ACK。
- **架構**：每個 dialog 一個 tokio task（`dialog/task.rs`），狀態與所有計時器（2xx 重送、AckTimeout、re-INVITE 逾時、491 重試、session 更新／逾時）都在 task 內，以 `tokio::time` 驅動；我方送出的 client transaction 事件由小型 forwarder task 轉進同一個 channel。`Endpoint` 的 dispatcher 依 `(Call-ID, local tag, remote tag)` 把 in-dialog request／ACK／stray 2xx 分派給 dialog。所有 task 登記在 `Tasks`，`Endpoint::shutdown()` 全部 join。
- **對外 API 與規格 §7 的差異**：`DialogHandle::next_event()` 回傳 `Option<DialogEvent>`（`Terminated` 之後為 `None`）；新增 `DialogEvent::Established`（UAS：初始 2xx 的 ACK 到達）供 fax-in 的「ACK 後等 N 秒」策略使用；新增可複製的 `DialogControl`（`reinvite`／`bye`），讓一個 task 等事件、另一個 task 下指令；新增 `ReinviteOutcome::NoAnswer`（2xx 沒有可用 SDP，仍會 ACK）；`bye()` 回傳最終狀態碼（例如驗證失敗時為 407）。
- **re-INVITE**：序列化執行（UAS 在初始 2xx 的 ACK 之前不發 re-INVITE，RFC 3261 14.1）。應用層逾時預設 32 s（`EndpointConfig::reinvite_timeout`）：收到過 provisional 才送 CANCEL（RFC 9.1），結果為 `Timeout`；未收到任何回應時由 Timer B 在同一時間點結束，不送 CANCEL。491 的重試等待依 RFC 3261 14.1：Call-ID 擁有者（我方是 UAC）2.1–4 s，另一方 0–2 s（規格 §5 只寫了後者）；次數上限 `max_491_retries`（預設 3）。
- **Session timer**：refreshee 的逾時點依 RFC 4028 §10 為 `interval − min(32, interval/3)`（90 s → 60 s）；refresher 在 `interval/2` 送 re-INVITE，SDP 內容相同、`o=` 版本加一。對方不支援 timer（沒有 `Supported: timer`）時我方自己當 refresher。422 的 `Min-SE` 取我方設定的下限。
- **Require**：預設只認得 `timer`；其他 tag 回 420 並列出 `Unsupported`，旗標 `accept_unknown_require` 可改為接受（C10）。`Supported` 的未知值一律容忍。
- **UPDATE 帶 SDP 回 488**（M1 限制，與規格一致）；INFO、不帶 SDP 的 UPDATE 回 200。
- **400 回應**：由 transport 在收到無法解析的資料時，依 `TransportConfig::reply_to_malformed`（`Endpoint` 預設開啟）呼叫純函式 `transport::recover::bad_request_reply`；取不到 Via／From／To／Call-ID／CSeq，或內容是 response／ACK 時不回應。只對來源政策放行的來源回應。
- **訊息 tap**：`TransportConfig::tap: Option<MessageTap>`，預設關閉。每一則進出的 SIP 訊息（UDP／TCP、含 403／400 與無法解析的收到資料）以 `TapEvent { at: SystemTime, direction, peer, transport, bytes }` 交給觀察者。`MessageTap::new(callback)` 在 I/O task 內同步呼叫（回呼不得阻塞）；`MessageTap::channel(n)` 用 `try_send`，滿了就丟棄並計數（`dropped()`），因此不會拖慢協定堆疊。M1d 的 per-call 擷取（C11）以此為基礎，用 Call-ID 分流。
- **註冊**：423 → 把 Expires 調到 Min-Expires 並立即重送（每次嘗試限一次，且記住新值供刷新使用）；任何帶 `Retry-After`（秒數格式）的失敗回應，等待該秒數取代指數退避。

**代價**
- `TuEvent` 多了一個變體；M1b 外部若有 `match` 需要補上（目前只有 sip-core 自己使用）。
- HTTP-date 格式的 `Retry-After` 不支援（遇到時退回指數退避）。
- 每個 client transaction 多一個 forwarder task（壽命與 transaction 相同）。

## ADR-012：Rust 工具鏈固定版本（2026-10-20）
**背景**

`rust-toolchain.toml` 原本使用浮動的 `stable`：
- 本機是 1.91.1，CI 和 Docker 是 1.99.1。
- 新版 clippy 的 `collapsible_match` 讓 M1b 的 commit 在 CI 上 lint 失敗。

**決策**

固定 `channel = "1.99.1"`，元件和 target 一併寫進設定。Docker 測試映像以 `RUST_VERSION` 參數安裝同一個版本。

**影響**
- 升級時要同時修改 `rust-toolchain.toml` 和 Dockerfile，並在同一個 commit 修正新的 lint。
- fuzz 仍然使用 nightly。

## ADR-013：M1d 的 fax-in、設定檔與依賴（2026-10-21）
**背景**

M1d 在 sip-core 之上建立 fax-in 呼叫控制骨架與可執行的 `sip-faxd serve`，需要設定檔格式、結構化輸出，以及 sip-core 以外的新依賴。

**決策**
- **新依賴（只在 sip-core 以外）**：`serde`（derive）、`toml` 0.8（設定檔）、`serde_json`（通話結果 `result.json` 與日誌）、`tracing-subscriber`（`sip-faxd` 的日誌輸出，只啟用 fmt/env-filter）。`sip-core` 的依賴沒有變動（仍遵守 ADR-008）。測試工具 workspace 另加 `md-5`（測試端獨立重算 Digest）。全部放在根 `[workspace.dependencies]`；皆為 MIT/Apache-2.0、純 Rust，沒有 OpenSSL。
- **設定檔**：TOML，每個區段都 `deny_unknown_fields`，拼錯的選項直接報錯。`[[trunks]]` 先套內建 preset（`generic`、`cht-cs210`，以 TOML 表深度合併，使用者的鍵覆蓋 preset）。`cht-cs210` 目前等於 `generic` 加明確的 UDP，待 M6 的實際擷取再調整。
- **`t38_policy`**：`auto`／`initiate` 在 M1d 的線上行為相同（ACK 後 `initiate_after_secs` 秒送出我方 re-INVITE，同時接受對方先發的 re-INVITE）；差別留給 M2（CNG 偵測只會提前觸發 `auto`）。`wait-peer` 從不主動發；`audio-only` 對 T.38 回 488，offer 同時有 audio 與 image 時選 audio。
- **T.38 失敗的處理**：對方對我方 re-INVITE 回 488／415／606（或 2xx 但 `m=image` port 為 0）→ 記錄「G.711 passthrough not yet supported」、送 BYE，失敗原因 `g711_passthrough_unsupported`。其他失敗（5xx、32 s 逾時）為 `t38_reinvite_failed`。`audio-only` 的通話無法真正收傳真，結束時同樣記為 `g711_passthrough_unsupported`。這與測試計畫 S07 原文「488 後 dialog 仍然存在」不同：因為 fallback 政策，我方在 ACK 之後立即 BYE；「dialog 在 488 之後仍存活」改由 S07b（audio-only 對 SIPp 的 T.38 re-INVITE 回 488）驗證。
- **per-trunk 旗標的限制**：`Require` 檢查在 endpoint 分派時尚不知道 trunk，因此 endpoint 一律放行，由 fax-in 在對應 trunk 後依 `compat.accept_unknown_require` 回 420（為此在 sip-core 新增 `IncomingCall::reject_with` 以附加 `Unsupported` header）。`Allow`／`Supported`、對外位址（Via/Contact）是全域設定（`[sip]`），不是 per-trunk。
- **來源過濾**：transport 層的 `SourcePolicy` 取所有 trunk `allowed_sources` 的聯集與 `unknown_source` 模式；trunk 對應在 fax-in 內以第一個符合的 trunk（設定順序）決定。
- **FaxSession 介面**：`FaxSession`／`FaxSessionFactory` trait（以 boxed future 保持 object-safe，不引入 async-trait）。M1d 的 `StubFaxSession` 只維持通話到對方 BYE 或 `max_duration_secs`。
- **診斷擷取**：tap 回呼只把事件丟進 channel（超過 20 000 筆排隊就丟棄並計數）；寫入由獨立 task 以 Call-ID 分流。INVITE 先於控制器知道該通話，因此未登記的 Call-ID 只暫存在小型有界緩衝（256 個 Call-ID × 16 則），避免掃描流量塞滿磁碟。`keep = "failed"` 時成功的通話完全不寫檔。
- **re-INVITE 的結果由通話迴圈直接輪詢**（非另開 helper task），確保對方緊跟在 200 後的 BYE 不會搶先於 re-INVITE 結果被處理。
- **測試工具**：`tools/interop` crate 建置並啟動真實的 `sip-faxd`，以 SIPp 3.8.0 驅動；scratch 目錄以行程 id 區分。SIPp 的 `-nr`（關閉自動重送處理）只用於 S04／S11，否則 SIPp 對重複的 180／ACK 會自動重送上一則訊息而形成乒乓。

**代價**
- `auto` 與 `initiate` 暫時沒有行為差異。
- 設定檔的 `peer` 若是主機名稱，於啟動時解析一次（沒有動態 DNS）。
- BYE 結果最多等 5 s（對方不回應時）才回報通話結果。

## ADR-014：t38 crate 的 PER／UDPTL 設計與 SpanDSP 差異（2026-10-21）
**背景**：規格 §1 要求 T.38 v0–v3 的 IFP PER 與 UDPTL；SpanDSP 的編碼在少數地方偏離 X.691。
**決策**
- IFP 採純 ASN.1 `T38(2002)` / ALIGNED PER：`field-type` 在 V0 為 3 bit 無 extension bit，V1+ 為 ext bit + 3 bit；addition 一律用 normally-small number（`0` + 6 bit）。V0/V1 拒絕所有 addition，V2+ 接受。
- 與 SpanDSP 不同之處（已記錄）：jm/ci/v34rate 的 index 版面、V1+ 中無 data 的非末尾 field 緊密排列；SpanDSP 實際發出的 7026 個封包全數逐 byte 相同。
- UDPTL 的 FEC 沿用 SpanDSP／Asterisk 的事實標準（entry m 為 `n+m-span*entries+k*entries`、k=0..span 的 XOR，暖機規則相同），並以參考實作逐 byte 驗證（285 個資料包）。
- 接收端為純狀態機：`on_datagram()` + `flush()`；以 64-bit 展開序號處理 wrap；FEC 恢復的封包帶零填補（`Source::Fec`、`Ifp::decode_padded`）。
**代價**：與外部以純 PER 為準的對端相容，與 SpanDSP 在上述三種罕見 field 不相容（fax-in 不用 V.34）。

## ADR-015：port_range 與每通電話的 UDPTL 埠（2026-10-21）
- `[media] port_range = "low-high"` 取代 `t38_port`；舊鍵給明確錯誤訊息（不靜默相容）。只發範圍內的偶數埠，FIFO 重用，`Drop` 歸還。
- 每通電話在回應前取得一個埠，整通共用；耗盡 → 503 + `error` 日誌 + `FailureReason::NoMediaPort`。
- `audio_port` 暫維持單一固定值（G.711 passthrough 在 M5）。

## ADR-016：K01 的處理方式（2026-10-21）
- dialog task 處理對方 BYE 前，若有進行中的 re-INVITE，先讓出排程最多 64 次並吸收已到達的 client transaction 事件，使「200 先到」的結果回報為 `Accepted`。不使用計時器，BYE 的處理在虛擬時間上不延遲。
- 這是有限度的保證；根本解（單一有序佇列）留待 M6 評估。

## ADR-017：測試映像保留 SpanDSP 的參考 UDPTL（2026-10-21）
- `docker/test-rocky9.Dockerfile` 在映像內保留 `tests/udptl.c`/`udptl.h`（GPL-2，僅在測試映像、不進 repo、不進正式執行檔），由 `spandsp-sys/build.rs` 編成靜態庫供 U01 使用。理由：libspandsp 本身不匯出 UDPTL，而它是 FEC 線上格式唯一的獨立對照。

## ADR-018：t4 crate 的語意與限制（2026-10-21）
- 影像 row 一律是 packed 1-bit、MSB 優先、**1 = 黑**；解碼器逐列以 callback 交出，永遠不自行累積整頁。
- 非 ECM 錯誤處理：MH／MR 壞列以上一列替代、計入 `bad_lines`／`longest_bad_run`，在下一個 EOL 重新同步（`rows` 不變，列數與發送端一致）；MMR 無法重新同步，第一個錯誤即放棄該頁剩餘部分（`EndReason::Aborted` 加 `Summary::error`），已輸出的列保留。T.30 層以 `Summary` 判斷 MCF／RTN。
- RTC＝連續 6 個 EOL；EOFB＝一個 EOL 起即視為結束。結束後的資料一律忽略。
- 硬限制：寬度 ≤ 8192、`max_rows` 預設 16 384，達到時以 `EndReason::RowLimit` 結束。fax-in 應依頁面長度需求設定 `max_rows`，不可關閉。
- TIFF Class F：little endian、每頁一個 G4 strip、`FillOrder=1`、MinIsWhite、`T6Options=0`；`DateTime`／`ImageDescription` 由呼叫端提供；有 bad lines 時寫 Class F 的 `BadFaxLines` 系列 tag。PDF：每頁一個 `CCITTFaxDecode` 影像（`K -1`、`BlackIs1 false`），頁面大小以 `pixels × 72 / dpi` 計算（1/10000 pt 捨去）。
- 測試規範：以 libtiff 比對像素必須用 `tiffcmp -t` 並附負向對照（預設的 `tiffcmp` 在 tag 不同時不比像素）。

## ADR-019：T.30 線上 octet 慣例與識別字串（2026-10-21）
- `t30` 的 frame 一律用 T.38 `hdlc-data` 的 octet 慣例（T.30 表格的書寫順序，最先送出的 bit 為 MSB）：最終 DIS 是 `FF C8 01 …`、DCS 是 `FF C8 C1 …`、X 位元是 FCF 的最高位元；DIS／DCS 的「bit n」＝FIF 第 n 個 bit＝`fif[(n-1)/8] & (0x80 >> ((n-1)%8))`。SpanDSP 記憶體內的 bit 反轉表示（`FF 13 80`）永遠不出現在本 repo。
- CSI／TSI：字元反序、補空白到 20 個 octet，**每個 octet 再 bit-reverse**（`1`＝`0x8C`、空白＝`0x04`）。T.4 非 ECM 影像資料與 HDLC 無須反轉。
- 延伸位元只在第 3 個 octet 起（bit 24、32、…）。依據：SpanDSP 3.1.1 原始碼與其真實封包；ITU 原文離線無法取得，請 Opus 對照 T.30 表 2 複核（尤其是掃描時間碼 21–23 的 5／10／40 ms 對應與 V.33 的 bit 13，這兩項只有 SpanDSP 的對照或沒有）。
## ADR-020：T.30 接收端的事件／動作介面與 localTCF（2026-10-21）
- `Receiver::on_event(now_ms, Event) -> Vec<Action>`；`Event` 有 `Start`、`Frame{data,fcs_ok}`、`Signal`、`ImageData`、`DataEnd`、`TxComplete`、`Timer`、`Abort`；`Action` 有 `SendSignal(Ced)`、`SendFrames`（一個 V.21 burst）、`StartTimer`、`CancelTimer`、`Page`、`Log`、`Done`。計時器 T4／T2 從 `TxComplete` 起算。失敗先送 DCN（`SendingDcn`），`Done` 在 DCN 送出後；Abort 與收到 DCN 直接 `Done`。
- localTCF：收到 DCS 後不回 CFR，直接等影像（`local_tcf_sends_cfr` 預設關）。transferredTCF：零 bit ≥95 %、長度 ≥ `rate×1.2 s`。
- 頁面品質：RTN／RTP／MCF 門檻見報告 §7-8；RTN 的頁面不交付。
## ADR-021：T.38 接收引擎的 sans-IO 介面與 pacing（2026-10-21）
- `fax_in::t38engine::T38Engine`：`start/advance/on_ifp/on_loss/abort/poll_output(poll_timed)/next_wakeup_ms/result/take_pages`；時間為呼叫者給的毫秒，內部以 µs 排程（V.21 300 bit/s 無法整除毫秒）。`UdptlEngine` 為薄包裝。輸出頁面一律重新編成單一 MMR strip（`ReceivedPage`），檔案輸出在 session 層。
- pacing 與重複次數採 SpanDSP `t38_terminal` 的行為（指示 ×3、資料 ×1、sig-end ×3；CED 前 200 ms、CED 3 s、burst 前 75 ms、preamble 1 s、30 ms 一包、frame 尾 stuffed+35 bit、burst 尾 100 ms）。接收端以「情境」丟棄重複的指示與 sig-end，不依賴序號。
## ADR-022：SpanDSP 互通的限制與測試規範（2026-10-21）
- SpanDSP 3.1.1 在 non-ECM 下不會使用 T.6，且無論遮罩都保留 MH；non-ECM 的「MMR 互通」不可能，改在 M3 的 ECM 驗證。
- SpanDSP 的 `NO_TCF` 模式不等 CFR，是 localTCF 對端的行為參考。`t30_terminate` 不送 DCN，只會讓我方走 T2 逾時。
- 協定 crate 的涵蓋率量測不得同時在同一 target 目錄執行兩個 `cargo llvm-cov`（`scripts/ci-local.sh` 已加鎖）。

## ADR-023：T.38 session、輸出與通話結果規則（2026-10-21）
- `fax-in::t38session::T38FaxSession` 是 `FaxSession` 的正式實作；socket 在 `SessionContext.t38.local_port`（pool 埠）、bind 於 `[sip] bind` 的 IP；先送往 SDP 位址，第一個能解析的 UDPTL 封包 latch；EC 與版本取自 SDP 協商結果（`udp_ec`、`version`、`rate_management`、`max_bit_rate`、`peer_max_datagram`），redundancy 深度與 FEC 參數取自 `[trunks.t38]`（`redundancy_depth`／`fec_span`／`fec_entries`）。
- 輸出 `<data_dir>/faxes/YYYY/MM/DD/<call-id>.tif|.pdf`（UTC 日期、`.part`＋rename、不覆蓋、Call-ID 淨化加 hash）；部分頁面也寫並 `partial=true`。
- `CallResult.fax: FaxReport`；`Completed` 的通話若 `fax.code≠0` 或 `store_error` → `Failed(Session)`；EOP 的 MCF 之後對方未送 DCN 就掛斷視為成功。
- `[call] fax_session = "hold"` 保留只維持通話的 stub。`udptl.log` 為 `<time> RX|TX <seq> <hex>`，緩衝 16 KB，與 `sip.log` 各自受 `max_bytes_per_call` 限制。
## ADR-024：T.38 引擎對「對方比我方 burst 尾巴早開始」與重複 sig-end 的處理（2026-10-21）
- 對方的 frame／影像資料／資料結束／V.21 preamble／training 指示到達，而 T.30 仍在 Sending* 狀態，引擎立即完成我方排程中的 `TxComplete`（取出佇列中的標記並餵給 T.30）；尚在佇列的 `no-signal` 等封包照常送出。
- `t4-non-ecm-sig-end` 在影像已結束後一律是重複（無論是否帶資料）。
- 回歸測試：`r16_peer_that_answers_before_our_burst_tail_is_over_loses_nothing`、`r14_sig_end_copies_that_repeat_the_last_octets_are_duplicates`；E 測試是獨立證據。
## ADR-025：端到端測試規範（2026-10-21）
- E 測試對真實 `sip-faxd` 執行檔、SpanDSP 即時 gateway（真實 UDP、牆鐘）；每個圖像以 libtiff（`tiffcmp -t`＋`tiffcp -c none` 後逐 byte）與 poppler（`pdftoppm -mono`）**各比一次**，兩者都要有負向對照；同步一律輪詢條件，不用固定 sleep。遺失用兩個方向各自獨立的帶種子 PRNG，使每個方向的遺失型態只取決於自己的封包序號。

## ADR-026：擷取預設不保存傳真影像內容（2026-10-21）
**背景**

M2d 原本的 `udptl.log` 會寫入完整的封包 hex，等同於複製一份傳真內容。再加上 `keep = "all"` 的預設值，每份傳真都會在 `logs/` 留下一份副本。

**決策**

新增 `[capture] udptl = "off" | "summary" | "full"`，預設為 `summary`：
- 每個封包都記錄摘要，T.30 控制 frame 保留完整 hex，影像資料只記錄長度。
- `full` 只在除錯時開啟，設定範例和 README 都會加上警告。

**理由**

- 傳真常含個資或機密，log 目錄通常沒有和傳真檔相同的存取控管。
- T.30 控制 frame 已經足以診斷對接問題。
