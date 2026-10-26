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

## ADR-027：T.38 引擎不得在對方 V.21 訊號未結束時開始回應（2026-10-21）
- 背景：AudioCodes Mediant 1000（6.80A）把 `hdlc-fcs-OK` 與 `hdlc-sig-end` 分成兩個封包（間隔 119–141 ms，之後才是 `no-signal`）。gateway 是半雙工，自己還在調變 V.21 時收到的 preamble 會被丟棄，我方的 CFR／MCF 因而沒有送到傳真機（現場 8 頁只收到 1 頁）。SpanDSP 一律送合併的 `hdlc-fcs-OK-sig-end`，所以 M2 的測試沒有碰到。
- 規則：對方的 HDLC frame 以不帶 sig-end 的 `hdlc-fcs-OK`（或 `-BAD`）結束時，視為對方的 V.21 訊號仍開著。我方的回應保留到 `hdlc-sig-end`、`hdlc-fcs-OK-sig-end`、`no-signal` 指示、任何新的指示（含非 V.21）或影像資料出現，再套用 turnaround 延遲（`silence_before_burst_ms`，預設 75 ms）後排程。若始終沒有結束事件，`peer_signal_wait_ms`（預設 500 ms，0 = 關閉）後照樣回應，再加 turnaround。被更新的回應取代的保留回應視為已送完。
- 結果：SpanDSP 的合併 sig-end 行為與時序不變；E 測試時間不變。
- 測試：F1-01／F1-02（`f1_turnaround.rs`）、F1-04（`f1_trace_replay.rs`）、F1-05（gateway quirk `split_fcs_sig_end` ＋ `half_duplex`）。

## ADR-028：重複的命令重啟等待計時器（2026-10-21）
- 對方重送 MPS／EOP／EOM（或送 CRP）而我方重送上一個回應時，T2 在回應送出期間停止，`TxComplete` 後重新啟動；若狀態已前進則作廢。重複的 DCS 是新的訓練，照舊重啟 T2。階段 B 的 T4 不變。
- 理由：T2 若從第一次回應起算，會在對方還在重試時到期（現場：第一次 MCF 後 6 s ≈ 第二次 MPS 結束後數百 ms），以 `t2_expired_image` 結束通話。
- 測試：F1-03（`f1_repeat_command.rs`）、R01 表格。

## ADR-029：T.4 解碼器忽略第一個 EOL 之前的雜訊（2026-10-21）
- 頁面資料以 EOL 開始；第一個 EOL（或第一個好行）之前解不出來（或解出來但後面沒有 EOL）的位元不算一行、不計 `bad_lines`、不輸出替代列。AudioCodes 在影像前送約 1 s 的 0xFF（1787 octet），舊解碼器因此每頁多一個壞行與一列空白。整份都沒有 EOL 的資料仍算一個壞行。
- 限制：恰好 144 octet 的 0xFF 是一整行合法資料。

## ADR-030：發佈壓縮檔的檔案擁有者固定為 root:root（2026-10-21）
**背景**

使用者用 root 解壓縮 v0.1.0-alpha 後，檔案擁有者顯示為 `mysql:mysql`。原因是 tar 保存了 CI 建置機的 uid/gid，root 解壓縮時會還原這組數字，剛好對應到目標主機上的 mysql 帳號。

**決策**

`scripts/package.sh` 打包時固定寫入 `0/0`（`--owner=0 --group=0 --numeric-owner`），並關閉 macOS 的 AppleDouble 檔。安裝文件建議解壓縮後，用 `chown` 把擁有者改成實際執行服務的帳號。

## ADR-031：ECM 接收的設計與範圍（2026-10-21）
- 依 T.30 Annex A／T.4 Annex A；`t30` 新增狀態 `EcmWaitBlock`／`EcmRxBlock`／`EcmWaitPps`，區塊在 PPS 確認時一次交給 `t4` 解碼器；PPS-NULL／MPS／EOP／EOM、PPR（256 bit，區塊外的位元為 1）、CTC→CTR（更新速率）、EOR→ERR（缺的 frame 補零並計入 `ecm_frames_lost`）、RR 重送上一個回應；重複的命令重送上一個回應；`ecm_max_ppr`（預設 12）保護。結果碼 `EcmFramesLost`（頁面有洞）、`EcmFailed`。
- FCD／RCP 的 frame 號與 PPS 計數器採 LSB first，在本專案 octet 慣例（第一個傳送的位元為 MSB）下是位元反轉值（ADR-019 的延伸）。
- 引擎：image-rate 的 HDLC 欄位與 V.21 走同一條路徑變成 `Event::Frame`；送出端不變（回應都在 V.21）。
- 不做：RNR、T5、PIP／PIN、EOS、流量控制模式、半頁救回。
## ADR-032：DIS 宣告規則與 `[trunks.fax]`（2026-10-21）
- `ecm`（預設 true）決定 bit 27；MMR（bit 31）只在 ECM 時宣告（T.30 表 2 註 9、17）；`resolutions`（預設 `["standard","fine"]`，不得為空、只允許這兩個值）決定 bit 15；DIS bit 7（frame 大小偏好）不設、bit 28 不設。
## ADR-033：T.30 原文核對結論（2026-10-21）
- 已以 ITU-T T.30 (09/2005) 與 T.4 (07/2003) 原文核對 DIS／DCS 位元與 ECM 相關 FCF（`crates/t30/tests/e06_t30_audit.rs`），無差異。V.33：DCS `0010`／`0110` 在 2005 版為 Invalid，依註 31 作為舊機器相容接受、不宣告。掃描時間碼依表 2。ADR-019 末段的「原文離線無法取得」作廢。
## ADR-034：SpanDSP ECM 互通的限制（2026-10-21）
- SpanDSP 3.1.1 一律送 256 octet 的 ECM frame；ECM＋`NO_TCF` 的 T.38 terminal 會停住；EOR/ERR 之後不會接著傳下一頁（直接 DCN）。64 octet frame 與 ECM＋localTCF 只由單元／引擎測試涵蓋。E03 的 PPR 證明以「只讓影像資料包遺失」進行（見本報告 §5-1）。

## ADR-035：ECM 預設關閉，直到經過真實 gateway 驗證（2026-10-21）
**背景**

M3d 完成了 ECM 接收，但只和 SpanDSP 互通過。AudioCodes 現場目前是以 non-ECM 運作並已打通，如果預設宣告 ECM，就等於把一條真機沒有測過的路徑直接推到現場。

**決策**

`[trunks.fax] ecm` 預設為 `false`，需要時再依 trunk 開啟。在真實 gateway（AudioCodes 和 CS210）上驗證 ECM 之後（M6），再重新評估預設值。

## ADR-036：store 以「單一 SQL 實作＋四個薄驅動」實現 `FaxStore`（2026-10-21）
取代「每個方言各寫一個 Repository」。方言差異集中在：`?` 佔位符改寫（`$n`／`@Pn`）、`LIMIT`／`TOP`、`RETURNING`／`OUTPUT INSERTED`／`last_insert_id`、時間型別編碼、整數寬度與 MySQL `_bin` 文字欄位解碼。好處：一份行為、一份測試；風險：不能使用任何方言專屬 SQL（目前不需要）。

## ADR-037：sqlx 0.9 ＋ tiberius 0.13（修訂 ADR-003 的版本）（2026-10-21）
理由見 §6.1（0.8／0.12 的相依有未修補的 advisory）。附帶：根 `rust-version` 需提高到 1.94。

## ADR-038：接受 ring 與 aws-lc-rs 並存（2026-10-21）
（sqlx 用 ring、tiberius 預設 aws-lc-rs），兩者皆可靜態連結 musl，無 OpenSSL。

## ADR-039：時間欄位統一 UTC 毫秒；MySQL 以 naive UTC 綁定（2026-10-21）
，不使用 `TIMESTAMP`（有 2038 與連線時區轉換問題）。

## ADR-040：資料庫版本比程式新時拒絕啟動；migration 不提供降版（2026-10-21）
（規格已有，建議補入決策紀錄。）

## ADR-041：fax-in 依賴 store；DB 寫入是 best-effort 的記憶體佇列（2026-10-21，M3b）
- `fax-in → store`：紀錄的對映（`summary::classify`、`numbers`）與寫入佇列（`recorder`）放在 fax-in，sip-faxd 只負責接線與 CLI。`sip-core` 仍不依賴任何 fax crate，`store` 不依賴 `fax-in`。
- 寫入佇列依序處理，指數退避（1 s 起、上限 60 s、8 次），佇列上限 10 000（最終寫入不丟）。最終寫入放棄時，若 capture 沒有寫 `result.json` 就補寫一份，供 `sip-faxd db backfill` 回補。**佇列不持久化**：程序在寫入前結束時，靠 `result.json` 與 backfill 補救。
- DB 連不上時 daemon 仍然啟動；schema 版本不符、URL 或帳密錯誤則拒絕啟動。殘留 `receiving` 只在啟動當下連得上 DB 時修正。
- `CallResult` 新增 `bye_by`，`completed` 的掛線方向依實際 BYE 的發送方。`no_fax_signal` 定義為 T.30 結果碼 1 或 2 且沒有頁面；對方在 T.30 進行中掛斷（結果碼 11）判為 `peer_hangup`／`peer_hangup_no_fax`。

## ADR-042：migration 鎖與可重跑（2026-10-21，M3b）
- 鎖綁連線：PostgreSQL `pg_advisory_lock`、MySQL/MariaDB `GET_LOCK`、MSSQL `sp_getapplock`、SQLite `BEGIN EXCLUSIVE`。取得鎖後重新讀版本。
- SQLite／PostgreSQL／MSSQL 每個 migration 一個交易。MySQL/MariaDB 的 DDL 不是交易性的，所以每個陳述式都必須可重跑（`CREATE TABLE IF NOT EXISTS`，索引寫在資料表定義裡），由單元測試強制。MySQL 的 `0001_init.sql` 因此改寫（從未發佈）。

## ADR-043：每日清整排程在 sip-faxd 內（2026-10-21，M3b）
- 以伺服器本地時間的 `retention.run_at` 觸發，每次等待前從 DB 讀設定；時鐘與 sleep 可注入。DST 轉換日可能早或晚一小時，清整是冪等的，不影響結果。

## ADR-044：本機 musl 建置在 Docker 裡進行（2026-10-21，M3b）
- `scripts/musl-docker.sh`（Rocky 9＋zig 0.14.1＋cargo-zigbuild）。`ci-local.sh` 只有在 Linux 且本機已有 zig 0.14.x 時才用本機工具，其他情況（macOS、Windows、zig 0.16）一律走 Docker。

## ADR-045：store 涵蓋率門檻排除三個伺服器驅動（2026-10-21，M3b）
- `backend/{postgres,mysql,mssql}.rs` 只有連到真實伺服器才會執行，由 db-matrix 在四種 DB 上跑同一套行為測試驗證；CI 的 `coverage` job 只跑 SQLite，所以門檻（80%）排除這三個檔案。其他檔案仍需 ≥ 80%。

## ADR-046：ECM 協商結果的記錄（2026-10-21，M3b）
- `FaxReport`／`result.json` 帶 `ecm`、`ecm_ppr_sent`、`ecm_frames_lost`；DB 只存 `ecm`（布林），PPR 次數與遺失 frame 數只在 `result.json`。

## ADR-047：投遞佇列與錯誤分類（2026-10-21，M3c）
- `delivery_attempts` 直接當佇列，**每次嘗試一列**：暫時性失敗把該列標 `failed` 並新增下一列 `pending`；超過 `max_attempts`（預設 10）標 `giving_up`。當機造成的中間態以「已有後繼列就視為 superseded」處理。重啟只需重新查詢。
- 暫時性錯誤（依 1、5、15、60 分鐘，之後每 4 小時重試）：連線錯誤、逾時、HTTP 5xx／408／425／429、SMTP 4xx、TLS 錯誤、資料夾 I/O 錯誤。永久性錯誤（直接 `failed`）：其他 HTTP 4xx、3xx（不跟隨轉址）、SMTP 5xx、來源檔不存在、設定錯誤。
- 投遞是 at-least-once；資料夾投遞以內容比對達到冪等，Email/Webhook 可能重複。目前假設單一 sip-faxd 實例（沒有領取鎖）。
- `delivery_attempts.target` 是穩定識別字（`folder:<dir>`、`email:<收件者>`、`webhook:<去除帳密與 query 的 URL>`），不含密碼，用來在重啟後找回設定。

## ADR-048：投遞相關依賴與格式（2026-10-21，M3c）
- lettre 0.11（`tokio1-rustls-tls`）、reqwest 0.13（`rustls`）、hmac 0.13、sha2 0.11；`deny.toml` 允許 `0BSD`（lettre 的 `quoted_printable`）。沒有 OpenSSL／native-tls。
- Webhook 簽章為 `HMAC-SHA256(secret, 實際送出的 body)`，header 預設 `X-Sipfax-Signature: sha256=<hex>`；multipart 由本專案自行組裝，簽整個 body。
- 資料夾投遞預設自動建立目錄（`create_dir = false` 時，目錄不存在視為暫時性失敗）；同名不同內容的檔案不覆蓋，改加 `_1`、`_2`。
- 檔名範本把 `[字母數字 - _ . + ( )]` 以外的字元換成 `_`，去掉開頭的點與 `..`，長度上限 200 bytes；文字範本把控制字元換成空白（防 header injection）。

## ADR-049：PNG 預覽（2026-10-21，M3e）
- 引入 `png` 0.18（純 Rust，MIT/Apache-2.0），只用在 `t4::png`。正確性由 libpng／libtiff 獨立比對。
- TIFF 讀取（`t4::TiffReader`）放在 t4，仍然不做 I/O（輸入 `&[u8]`）。
- 預覽在第一次開啟時才產生並快取在 `data_dir/cache/png/<id>/`，快取不寫入 DB；以快取檔為單位做 single-flight，`.part`＋rename 原子寫入；清整時整個目錄一起刪。快取沒有容量上限（受保留天數約束），M4 若需要再議。

## ADR-050：投遞接進 sip-faxd 的方式（2026-10-21，M3f）
- 只有設定了 `[[routes]]` 才啟動投遞服務。fax-in 的 recorder 在**最終寫入成功**（包含重試成功）後呼叫 `on_finalized(id)`，投遞服務收到後建立投遞；放棄的寫入不通知。
- 新增 `[delivery]`：`catch_up_hours`（預設 24，0 表示關閉）與 `max_attempts`（預設 10）。啟動時以 `catch_up` 補送這段時間內已 finalize、但還沒有投遞紀錄的傳真，涵蓋「finalize 後、dispatch 前當機」的情況。
- `sip-faxd db backfill` **不會投遞**，避免回補舊紀錄時灌爆信箱；回補的紀錄若落在 `catch_up_hours` 內，會在下次 `serve` 啟動時補送。
- 關閉服務時，已排入的 id 會先 dispatch，進行中的嘗試有 15 秒寬限。

## ADR-051：執行設定改存 DB，設定檔只留啟動設定（2026-10-21，M4 設計）
**背景**

使用者希望清整、帳號、trunk／SIP、投遞路由等設定都能在 Web 上修改，不必改設定檔再重啟。

**決策**

- 設定檔只保留啟動必需的 `[log]`、`[database]`、`[storage]`、`[web]`、`[service]`。其餘（`[sip]`、`[media]`、`[call]`、`[capture]`、`[[trunks]]`、`[[routes]]`、`[delivery]`、`[retention]`）存在 DB 的 `config_sections`，以 JSON 表示、由 Rust 型別驗證、樂觀鎖、每次修改寫入 `config_audit`。
- 第一次啟動時從設定檔匯入；之後以 DB 為準，設定檔中的這些區段只會產生 warning。`config import/export` 供備份與還原。
- 執行設定以整份快照熱替換，每通來電在 INVITE 時取一次快照；`[sip]` 的 bind／transport 與 `[media]` 的 port_range 需要重啟。
- 秘密欄位以 `conf/secret.key` 做 AES-256-GCM 加密後存入 DB，API 只寫不讀。
- 帳號分 `admin`／`agent`，argon2id，session 存 DB。Web 可選 HTTP 或 HTTPS；HTTPS 沒有提供憑證時以 rcgen 產生自簽憑證（有效期原訂 10 年，ADR-056 改為 825 天並自動更新）。介面提供繁中與英文。

**影響**

`config check` 與 `field-test.md` 的設定說明要改寫；ADR-013 中「設定全部在設定檔」的部分由本 ADR 取代。

## ADR-052：M4a 新增的依賴（2026-10-22，M4a 審核）
**決策**

- `arc-swap`：執行設定快照，每通來電在 INVITE 時取一次。
- `argon2` 0.5（RustCrypto，純 Rust）：密碼雜湊，argon2id，`m=19456,t=2,p=1`。
- `ring` 0.17：AES-256-GCM（秘密欄位）、SHA-256（session token 只存雜湊）、隨機數。原本就在依賴樹裡，現在改為直接依賴。
- `base64`、`serde_path_to_error`：驗證錯誤帶欄位路徑。
- 僅測試使用：`rust-argon2`，作為獨立實作互相驗證。
- `profile.dev` 對 argon2／blake2 設 `opt-level = 3`，否則除錯建置的雜湊要好幾秒。

全部是純 Rust 或只依賴 ring，`cargo deny check` 通過，musl 靜態連結成立。

## ADR-053：執行設定的儲存、秘密與熱套用細節（2026-10-22，M4a 審核）
**決策**（補充 ADR-051）

- trunk 的比對順序存在 `trunk_order` 區段（名稱陣列）；規格只有 `trunk:<name>`，但順序也是設定的一部分。
- 秘密欄位以 AES-256-GCM 加密，存成 `enc:v1:<base64>`，AAD 為 `<區段>/<欄位路徑>`，所以密文搬到別的欄位會解不開。API 只回 `{"set": true, "ref": "<路徑>"}`，原樣送回代表保留；`ref` 讓重新排序路由或投遞時，秘密跟著項目走。
- `conf/secret.key` 遺失或損壞時，解不開的秘密以佔位字串 `***undecryptable-secret***` 代替，對方會因認證失敗而拒絕；其他設定照常可用，不會因為一把金鑰讓整個區段失效。匯出時略過佔位字串。
- 需要重啟才生效的設定（`fax_in::config::restart_required`）：`[sip]` 的 `bind`、`transports`、`advertised`、`user_agent`、`allow`、`supported`；`[media]` 的 `address`、`audio_port`、`port_range`；整個 `[capture]`；以及 `[call]` 的 `fax_session`、`reinvite_timeout_secs`、`max_491_retries`（端點參數）。其餘設定對下一通來電立即生效；進行中的通話保留原本的快照。transport 的來源過濾器可以替換；投遞引擎在路由或 `[delivery]` 變更時整個換掉。
- 已知取捨：稽核列與修改是兩個陳述式，當機時可能少一列稽核，但不會少修改；登入鎖定與每 IP 限流的狀態在記憶體，重啟會歸零，多個實例不共用；webhook URL 本身（userinfo、query 中的 token）不加密，只在顯示時遮蔽。

## ADR-054：預覽快取與清整的競態、容量上限（2026-10-22，M4a 審核）
**決策**

- 轉檔完成後再確認一次紀錄；若這段期間已經清整（`files_purged_at`），就刪掉剛寫的快取並回 `Purged`（HTTP 410）。轉檔因目錄被刪而出現 I/O 錯誤時，也判為 `Purged`。清整標記之後再掃一次快取目錄。
- `[storage] preview_cache_mb`（預設 2048）：超過時依最後使用時間淘汰（以檔案修改時間代表，快取命中時更新）；60 秒內用過的檔案不刪；上限是軟性的。

## ADR-055：服務管理、控制 socket、log 輪替與 TUI（2026-10-22，M4d）
**背景**

使用者要求 `sip-faxd start | stop | restart | status | logs`，stop 要等進行中的傳真收完（`docs/TODO.md` M4），以及本機的 TUI（`docs/design/m4-web.md` §7）。

**決策**

- **PID 檔**：`run/sip-faxd.pid` 以 `flock` 鎖住，鎖由 kernel 管理，當機只會留下檔案、不會留下鎖；「是否在執行」一律看鎖，不看檔案內容。`flock` 在 Docker Desktop 的 bind mount 和某些網路檔案系統上不會互斥，所以 `run/` 必須放在本機檔案系統（E2E 把 PID 檔放在容器內的暫存目錄）。
- **控制 socket**：`run/sip-faxd.sock`，權限 0600，一行一個 JSON（`status`、`calls`、`stop`、`reload`、`subscribe`）。能連上就等同管理員，不另外登入。socket 在 SIP 開始監聽之後才建立，所以 `start` 看到它回應 `status` 就代表服務已就緒。`reload` 呼叫 M4a 的 `ConfigService::reload`。
- **stop**：fax-in 的 `drain` 讓新的 INVITE 立刻回 503，再等進行中的通話結束，最多 `stop_timeout_secs`；逾時、`--force` 或第二次收到訊號時，結束剩下的通話，已收到的頁面照樣寫出（`partial`、`hangup_code = shutdown`）。`serve` 收到 SIGTERM 時走同一套流程。
- **systemd**：偵測到 `INVOCATION_ID`，或 PID 檔指向的程序的父程序是 PID 1 且有 `NOTIFY_SOCKET` 時，`start`／`stop`／`restart` 提示改用 `systemctl` 並回非 0，`--ignore-systemd` 可略過。
- **log 輪替**：自行實作，不用 `tracing-appender`，因為它不支援依大小輪替與 gzip。依大小或日期輪替，舊檔在背景執行緒以 gzip 壓縮，保留 `[log] keep` 份；時鐘可注入。`logs -f` 以 inode 判斷檔案是否已輪替。
- **TUI**：`sip-faxd tui` 使用 ratatui＋crossterm。傳真資料（列表、詳細、已讀、備註、投遞）直接讀寫 DB，操作者記為 `cli:<作業系統帳號>`，與 CLI 相同；狀態、通話和即時事件來自控制 socket。服務沒有執行時，TUI 仍可操作 DB。語言跟隨 `LANG`。
- **新增依賴**：`rustix`（flock、kill、umask；純 Rust）、`flate2`（`rust_backend`，log 壓縮）、`ratatui` 0.30（`default-features = false`，只開 `crossterm`）、`crossterm` 0.29。全部是純 Rust，`cargo deny` 通過。
- **投遞**：投遞 worker 啟動時先執行一次 `run_due`，讓 catch-up 排入的投遞立即送出，不必等一個 `poll_interval`（10 秒）。

**影響**

`[service]`（`pid_file`、`socket`、`stop_timeout_secs`、`start_timeout_secs`）與 `[log]` 的輪替設定屬於啟動設定，留在設定檔（ADR-051）。這些服務指令與 TUI 只支援 unix；其他系統只能使用 `serve`。

## ADR-056：Web 伺服器、TLS 與自簽憑證（2026-10-22，M4b）
**背景**

規格原訂自簽憑證有效期 10 年。實作時發現 macOS 的憑證驗證（Safari、macOS 上的 Chrome，以及 reqwest 用的系統驗證）拒絕有效期超過 825 天的 TLS 伺服器憑證，即使使用者已經信任它也一樣，而且無法略過（`not standards compliant`；改成 2 年有效期就能通過）。

**決策**

- **自簽憑證**（2026-10-22 使用者決定）：ECDSA P-256、**有效期 825 天**、EKU `serverAuth`、key usage `digitalSignature`。存放在 `<設定檔目錄>/tls/self-signed.{crt,key}`，私鑰 0600。已存在就沿用；距離到期不到 30 天、已過期或檔案無法使用時，啟動時自動重新產生，並記 warning（附舊指紋）。使用者自備的憑證不會被更換，只記 warning。每次啟動都在 log 印出 SHA-256 指紋（格式與 `openssl x509 -fingerprint -sha256` 相同）。
- ~~**`[web]` 預設關閉**~~：已由 ADR-057 改為預設開啟。`[web]` 是啟動設定（ADR-051）。
- **TLS**：rustls，明確指定 ring provider（依賴樹裡同時有 ring 與 aws-lc-rs，不指定時 rustls 會在執行時 panic），只允許 TLS 1.2 與 1.3，ALPN 提供 `h2` 與 `http/1.1`。
- **伺服器**：自寫 accept loop（tokio-rustls＋hyper-util 的 auto builder），保留 HTTP upgrade 給 WebSocket；TLS 握手 10 秒逾時；graceful shutdown 時讓進行中的請求在寬限時間內完成。每個請求帶 `ConnectInfo<SocketAddr>`（登入限流用）。Web 在控制 socket 之前啟動，所以 `start` 看到服務就緒時，Web 也已經在監聽；Web 無法啟動（憑證不可用、port 被占用）時 `serve` 回非 0。
- **新增依賴**：`axum` 0.8、`tower` 0.5、`tower-http` 0.6、`hyper` 1、`hyper-util` 0.1、`tokio-rustls` 0.26、`rustls` 0.23、`rustls-pki-types`（PEM 讀取）、`rcgen` 0.14（ring）、`x509-parser` 0.18（讀取既有憑證的到期日）、`if-addrs`（bind 為 `0.0.0.0` 時列出本機位址放進 SAN）。axum、tower、hyper、rustls 原本就在依賴樹裡。全部是純 Rust 或只依賴 ring，`cargo deny check` 通過。

## ADR-057：Web 預設開啟；PNG 預覽在收完傳真時產生，和 PDF 放在一起（2026-10-22，M4b 之後）
**背景**

使用者決定（2026-10-22）：
1. sip-fax-rs 要預設開啟 Web。
2. 預覽 PNG 要由伺服器產生，和 PDF 放在一起。

原本的做法：`[web]` 預設關閉（ADR-056）；PNG 在第一次開啟時才從 TIFF 轉出，放在 `data_dir/cache/png/<id>/`，有容量上限 `[storage] preview_cache_mb`（ADR-049、M4a R02）。

**決策**

- **Web 預設開啟**：設定檔沒有 `[web]` 時也會以預設值啟動（HTTPS、`0.0.0.0:8443`、沒有憑證時產生自簽憑證）；`[web] enabled = false` 才關閉。升級的現場會多開 8443 port，發佈說明要寫明。測試用的設定檔一律寫 `[web] enabled = false`，避免多個 `serve` 同時搶 8443。
- **PNG 在通話結束時產生**：`fax-in` 寫完 TIFF 與 PDF 後，從剛寫的 TIFF 轉出每頁的整頁 PNG 與 300 px 縮圖（`store::render_pages`，在 blocking pool 執行），寫在同一個目錄：`<name>-p<頁>.png`、`<name>-p<頁>-thumb.png`（`store::png_rel_path`；`.part`＋rename）。檔名由 TIFF 路徑推出，資料表不變。產生失敗只記 warning，不影響收傳真與紀錄。
- **預覽 API**：`Preview::page_png` 直接回傳 TIFF 旁的檔案；不存在時（舊版收到的傳真、或當時寫入失敗）才從 TIFF 轉一次並寫在同一處。single-flight、與清整的競態處理（R01）照舊。
- **取消快取上限**：PNG 和 TIFF／PDF 一樣依 `[retention] files_days` 保存，不再另外淘汰。`[storage] preview_cache_mb` 仍可解析（舊設定檔照常載入），但不再使用，`config check` 會提示移除。`Preview::with_cache_limit`／`trim_cache`／`with_clock` 刪除。
- **清整**：刪除 TIFF 所在目錄中名稱完全符合 `<name>-p<數字>.png` 或 `<name>-p<數字>-thumb.png` 的檔案（`store::is_png_of`；同目錄裡 `<name>-2` 等其他傳真的檔案不會被刪），再刪 TIFF／PDF；舊版留下的 `cache/png/<id>/` 也一併刪除。
- **代價**：每份傳真多出 2 × 頁數個檔案；整頁 PNG 是 1-bit，一頁約數十 KB。收完傳真到寫入最終紀錄之間多了轉檔時間（每頁數毫秒到數十毫秒）。
## ADR-058：Web 前端的實作方式（2026-10-22，M4c）
**背景**

ADR-005 決定以 axum 加內嵌的 Svelte SPA 提供介面、只發佈一個執行檔；`docs/design/m4-web.md` §6 規定前端的頁面、i18n 與建置規則。實作時需要決定嵌入方式、路由、設定表單的產生方式，並補幾個後端端點。

**決策**

- **嵌入**：不用 `rust-embed`，改由 `web` crate 的 `build.rs` 走訪 `web-ui/dist`，為每個檔案產生 `include_bytes!` 與 content type（`$OUT_DIR/ui_assets.rs`）。不增加 Rust 依賴，內容在編譯時固定（`rust-embed` 的 debug 模式會在執行時讀磁碟）。沒有 `dist` 時嵌入「介面沒有建置」的說明頁並給 cargo warning；設 `SIPFAX_REQUIRE_WEB_UI=1`（CI 的 `build-musl`）時直接失敗。`scripts/package.sh` 確認執行檔確實含有這次 `dist` 的入口檔名。仍符合 ADR-005「一個執行檔」的決定。
- **快取**：`assets/` 下的檔名含內容雜湊，回 `Cache-Control: public, max-age=31536000, immutable`；`index.html` 等其他檔案回 `no-cache`。
- **路由**：以 URL fragment（`/#/faxes/12`）路由，伺服器只需提供 `/` 與檔案，`/api` 以外不存在的路徑仍回 API 格式的 404。
- **前端依賴**（只在建置時使用，不進執行檔）：Svelte 5、Vite 8、TypeScript 6（typescript-eslint 尚不支援 7）、svelte-check、ESLint 10、vitest 5（jsdom、v8 coverage）、Playwright。執行時沒有任何第三方套件：路由、i18n、API client、WebSocket 重連都自己寫。`package-lock.json` 進 repo，CI 以 `npm ci` 安裝；Node 24。
- **CSP**：維持 `default-src 'self'`，不放寬。Vite 關閉 modulepreload polyfill、`assetsInlineLimit = 0`，`index.html` 沒有 inline script；頁面不使用 `style` 屬性。
- **設定表單**：前端有一份欄位定義表（`web-ui/src/lib/schema.ts`），決定每個欄位的輸入方式（文字、數字、勾選、選單、清單、秘密、鍵值），因為值為 `null` 的欄位無法從 JSON 判斷型別。驗證仍只由後端做（規格 §0），422 的錯誤依欄位路徑顯示在欄位旁。定義表沒有列出的欄位原樣保留、原樣送回。秘密欄位顯示「已設定／未設定」，沒有輸入新值時送回伺服器給的 `{"set": true, "ref": …}`（保留原值）。
- **需要重啟的欄位**：新增 `fax_in::config::RESTART_FIELDS`（`restart_required` 可能回報的全部名稱，debug build 會檢查兩者一致），`GET /config` 以 `restart_fields` 回傳，表單在修改之前就標示。
- **新增 API**：`GET /faxes?q=`（ani、dnis、remote_ident、note 任何位置，不分大小寫，`LOWER(col) LIKE ? ESCAPE '!'`，四種 DB 共用同一段 SQL）；`PUT /auth/me {locale}`（自己的介面語言，`null` 表示依瀏覽器）；`locale` 只接受 `zh-TW`、`en` 或 `null`（`/users` 一併檢查，否則 422 `bad_locale`）。
- **語言**：帳號設定 → 這個瀏覽器上次的選擇（localStorage）→ 瀏覽器語言 → zh-TW。介面上方的語言選單只改這個瀏覽器；「我的帳號」頁可存進帳號。伺服器回的英文訊息與設定提醒（`warnings`）不直接顯示：錯誤依 `code` 翻譯，提醒收在預設收合的區塊。
- **保留的秘密不重新加密**（M4c ⑥ 測試發現，修正 ADR-053 的行為）：送回 `{"set": true, "ref"}` 且 `ref` 與目前路徑相同時，DB 中的密文原樣保留（仍先解密一次，確認金鑰可用）；只有秘密換了位置（例如路由重新排序）才以新路徑重新加密。原本每次儲存都以新的 nonce 重新加密，改別的欄位也會讓密文改變。
- **儲存後重讀區段**：設定表單儲存成功後以 `GET /config/{section}` 重讀，秘密欄位立刻顯示新的「已設定／未設定」，也不會在下一次儲存時再送出明文。
- **`PUT /auth/me` 必須帶 `locale`**：沒有這個欄位回 400（`null` 才是改回依瀏覽器），避免空 body 把語言清掉。
- **列表的即時事件在登入期間一直訂閱**（`App.svelte`），不在列表頁時新傳真進「有 N 份新傳真」。

## ADR-059：換資料庫的工具 `sip-faxd db copy`（2026-10-22，M4e）
**背景**

使用者問到：一開始用 SQLite，之後想改用 MSSQL 要怎麼辦。原本只能以 `config export/import` 搬設定、`db backfill` 從 `result.json` 重建通話，已讀、備註、投遞紀錄、稽核與帳號都會遺失。2026-10-22 使用者同意新增搬移功能。

**決策**

- `sip-faxd db copy --to <URL>`：來源是設定檔的 `[database] url`，四種資料庫任兩者之間都可以（含同種）。
- **前提**：服務停止（PID 檔的 flock）；來源 schema 等於執行檔版本；目標不是來源；目標建立 schema 後七個資料表都必須是空的。不提供覆寫目標的選項，避免誤刪。
- **原樣複製、保留 `id`**：傳真的網址、投遞與 session 的外鍵、Web 的事件與連結都維持有效。`schema_migrations` 不複製，由目標自己的 migration 寫入。秘密欄位原樣複製，沿用同一份 `conf/secret.key`。
- **單一交易、單一連線**：在目標的 migration session（專用連線，同時持有 migration 鎖，避免同時有服務在目標上啟動）中完成全部寫入，失敗就 rollback。各資料庫的差異：PostgreSQL `OVERRIDING SYSTEM VALUE` 加 `setval`；MSSQL 在每個 `INSERT` 的同一個 batch 內 `SET IDENTITY_INSERT`（參數化語句以 `sp_executesql` 執行，自成 batch）；MySQL／MariaDB 以 `SET autocommit = 0`／`COMMIT`／`ROLLBACK`（`START TRANSACTION` 不能以 prepared statement 執行）；SQLite 的 session 本身就是 exclusive 交易。
- **分批**：來源以主鍵分頁讀取（每頁 500 列）；寫入以多列 `INSERT`，每批參數不超過 2000（MSSQL 上限 2100）。
- **核對**：交易提交後，另以一般連線比對每個表的 `COUNT(*)` 與 `MAX(id)`。
- 欄位清單寫在 `store::copy` 的表格中；之後新增欄位或資料表的 migration，必須同時更新這份清單（測試會比對 schema 的欄位）。

## ADR-060：資料庫分欄位設定、`db check`、設定檔只留啟動設定（2026-10-22，M4f）
**背景**

2026-10-22 使用者回饋：資料庫帳密要自己做百分比編碼，對現場人員太難；存在資料庫的設定（ADR-051）同時也寫在設定檔裡，讓人以為改設定檔有用。使用者決定：資料庫可以分欄位填寫；新增單純測試連線的指令；設定檔只放啟動設定，存在資料庫的設定不再出現在設定檔，舊設定檔的這些區段保留相容匯入。

**決策**

- **`[database]` 分欄位**：`type`、`host`、`port`（依類型預設）、`name`、`user`、`password`／`password_file`（擇一；檔案取第一行，權限寬於 0600 給 warning）、`trust_server_certificate`（只限 mssql）、`path`（只限 sqlite）。程式組出 URL，帳號與密碼以 `NON_ALPHANUMERIC` 百分比編碼（四種後端都會解碼）；IPv6 主機加上方括號。`url` 照樣可用，但不能與分欄位同時出現。錯誤一律以 `[database] <欄位>: <原因>` 報告。程式其他地方仍只看組好的 `url`，密碼一律以 `mask_url` 遮蔽。
- **`sip-faxd db check`**：只連線並讀伺服器版本與 `schema_migrations`，不建立或升級資料表；SQLite 檔案不存在時不建立（新安裝的正常情況，回 0 並說明）。15 秒上限。連線錯誤依驅動的訊息分類給提示。`--url` 加 `--password-stdin` 測試另一個資料庫，密碼不用編碼。
- **`db copy` 的目標**：`--to` 加 `--to-password-stdin`，或 `--to-config <檔案>` 取另一個設定檔的 `[database]`。
- **設定檔只留啟動設定**（修訂 ADR-051）：`conf/sip-faxd.toml.example` 只有 `[database]`、`[web]`、`[storage]`、`[log]`、`[service]`；執行設定的說明移到 `conf/runtime.toml.example`，給 `config import --file` 使用。第一次啟動沒有執行設定時，以預設值寫入資料庫（沒有中繼）；啟動 log、`config check` 與 Web（`/status` 的中繼數為 0 時顯示橫幅）都提示「沒有中繼，來電都會被拒絕」。
- **相容**（使用者決定保留）：設定檔仍有執行設定區段、資料庫還沒有時，照舊匯入一次；資料庫已有時，啟動 log 與 `config check` 提示這些區段沒有使用、可以刪除。

## ADR-061：G.711 passthrough 的設計與範圍（2026-10-23，M5）
- **範圍（使用者決定）**：高速數據機只做 V.27ter（2400／4800）與 V.29（7200／9600），**不做 V.17**。DIS 只宣告這兩種，最高 9600 bit/s；一頁約多花幾秒，換來省下 TCM／Viterbi 這個最大的工程量。
- **分層**：`fax-modem` 是純 DSP、sans-IO 的 crate（tone、V.21、HDLC、V.27ter、V.29 與共用的 AGC、時脈回復、載波回復、等化器、scrambler），不依賴任何其他 fax crate；`fax-in::audioengine` 把 8 kHz PCM 轉成 `t30::Event`、把 `t30::Action` 轉成 PCM；RTP 與 G.711 編解碼放在 `sip-core::rtp`（從 sipress 抽取 `RtpPacket` 並修正 decode）。`t30` 不改：音訊引擎看到交給 t30 的 DCS 時自己解析出數據機與速率。
- **只收高速**：收傳真只需要高速數據機的收端；我方只送 CED 與 V.21。
- **半雙工**：我方送 V.21 期間與之後 75 ms 忽略輸入，避免把自己的回音當成對方的訊號。
- **進入 G.711 的條件**：對方拒絕我方 T.38 re-INVITE（488／415／606，或 `m=image` port 0），或 `t38_policy = "audio-only"`。`auto` 時對方在 audio 上直接送 CNG 而沒有 re-INVITE，仍照現行政策在 `initiate_after_secs` 由我方發 T.38 re-INVITE，被拒才走音訊（使用者決定：T.38 對封包遺失較耐受）；這段期間音訊引擎已經在收，不會漏掉 CNG 或 V.21。原本的 `g711_passthrough_unsupported` 改為走音訊。
- **清潔室實作**：依 ITU-T V.21、V.27ter、V.29、T.30、G.711 規格自行撰寫，不參考也不複製 SpanDSP 原始碼；SpanDSP 只當測試對照（各數據機的送端、`fax_state`）。
- 測試計畫：`docs/test-plans/M5.md`（分期 M5a–M5e）。


## ADR-062：V.27ter 收端的三項修正（2026-10-24，M5c 測試發現）
M5c 測試以 SpanDSP 的 `v27ter_tx` 與 `fax_state` 當獨立對照，找到下列產品問題，已做最小修正（細節與證據在 `docs/test-reports/M5c.md` §3）：
- **擾碼器回授用線上的位元**（傳送後、guard 反轉之後的位元），重複樣式計數器仍看 guard 之前的位元。原本回授 guard 之前的位元，與 SpanDSP 在 guard 觸發時（隨機資料約每 700 bit 一次）不同，約 1‰ bit 錯誤。兩邊自洽，所以我方自己的送端測不出來。
- **收端不再被雜訊或帶外訊號騙進 Training**：Segment 3 的 180° 反轉要求連續 10 個（原 8）、角度在 ±46° 內（原 ±60°）、相鄰符號大小相近（2 倍功率內），且匹配濾波器輸出能量至少是輸入能量的 0.25 倍（正常訊號約 2 倍；每 5 ms 一塊量測，擋掉 AGC 放大的直流與帶外音）；Segment 4 前 50 個符號內非軸向符號超過 8 個就放棄、回到找 Segment 3。原本一般白雜訊約每 10 分鐘一次假 Training，之後永遠卡在資料狀態，真的訊號來了也收不到。
- **Segment 4 的載波迴路積分增益加快**（0.002 → 0.006，資料階段維持 0.002）：+7 Hz 偏移加雜訊時，PLL 在第 50 個符號還沒拉進，4800 的 45° 奇數步階（容許 ±22.5°）被誤判為 Segment 5。
- `fax-in::audioengine` 辨識 DCS 時要遮掉 X 位元（T.30 記法的最高位元）：撥出端在收到 DIS 後送的 DCS 是 `FF C8 C1 …`，原本的遮罩 `& 0xFE` 讓它不被認得，不會建高速收端。

## ADR-063：V.29 收端的 Segment 3 擬合檢查（2026-10-25，M5d 測試發現）
M5d 的 B09（1 小時白雜訊）在 9600、−43 dBm0 有 1 次假 Training，之後卡在資料狀態輸出 2700 萬個垃圾位元；800／2600 Hz 等單音（每符號轉 135°，像 Segment 2）也會騙過。原因是 Segment 3 結束時無條件宣告 `Trained`。決定：
- Segment 3 最後 128 個符號累計與已知 C／D 點的平方誤差，大於已知點功率的 0.25（MER 低於 6 dB）就放棄，回到找 Segment 2。真實訊號在可用 SNR 下約 15 dB 以上，雜訊硬套約 0 dB，餘裕足夠。
- `Training` 事件改在 Segment 3 結束、與 `Trained` 同時送出（晚 160 ms），誤判時連 `Training` 都不送。t30 不依賴 `Training` 的時間；M5e 的 `audio.log` 若要記載波出現時間，應以高速收端的訊號存在為準。
- 細節與證據在 `docs/test-reports/M5d.md` §3。

## ADR-064：V.27ter 4800 損傷案例的 BER 門檻（2026-10-25，使用者交由 Opus 決定）
M5 測試計畫的 A04／A05 要求 4800 在 SNR 15 dB 加 ±100 ppm、±7 Hz 或 G.711 量化時 BER ≤ 10⁻⁵，實測 2–5×10⁻⁵。改用 V.29 的時脈做法後沒有改善（TODO 有數字）；A05 沒有時脈偏差也一樣，只有雜訊的 A03 本身已是 6.9×10⁻⁶，可見 15 dB 時 4800 受雜訊限制，任何額外損傷都會越過 10⁻⁵。這個門檻是 Opus 寫計畫時的估計，不是 GOALS 或 T.30 的要求。決定：
- 4800 的 A04／A05 改為 SNR 17 dB（計畫 SNR＋2 dB）時 ≤ 10⁻⁵，15 dB 時 ≤ 10⁻⁴。A03（只有雜訊）與 2400、V.29 的門檻不變。
- 理由：誤碼率 5×10⁻⁵ 時，一頁約 40 萬 bit 的影像大約有 20 個錯誤 bit，t30 會把它們算成壞行，比例遠低於 RTN 的門檻；線路不好時，對方收到 FTT 後也會降到 2400。為此追求理論邊緣上的 1 dB，不值得付出收端的複雜度和退步的風險。

## ADR-065：V.29 收端資料段的時脈改回 Gardner 二階迴路（2026-10-25，M5d B03）
M5d 開發時（審核掃參數）Segment 3 之後改用等化器係數重心帶時脈（只有比例項），解決了 Gardner 只有比例項時 100 ppm 的 MER 損失。M5d 測試發現 7200 在 SNR 17 dB 的 47 秒頁面會在 30–37 秒後失去資料。逐秒量測：沒有時脈偏差時，時脈仍穩定地走（30 秒約 1 個 T/2），等化器把 tap 往兩側對稱攤開，重心一直維持在中心，所以迴路看不到；最後等化器表示不了而崩潰。關掉重心時脈後 tap 就穩定，可見是時脈和等化器互相牽動造成的。決定：
- Segment 2 維持 Gardner（kp 0.1）；Segment 3（160 ms）時脈保持不動；Segment 4 起改用 Gardner 二階迴路 kp 0.005、ki 1e-5，由積分項追 ±100 ppm，資料段等化器步階維持 0.05。
- 實測可用範圍 kp 0.002–0.01、ki 2·10⁻⁶–10⁻⁵；kp 0.02 會讓 9600 在 100 ppm 時的 MER 比雜訊底低 1.6 dB 以上，ki 4·10⁻⁵ 則會讓 B04 失敗。
- 試過但不採用：tap 洩漏，包括往 0 洩漏（AGC 凍結後增益縮水，等化器改由其他方向補回，攤得更快）和往訓練後的係數洩漏（1e-3 時長頁勉強可以，但 B04 失敗）。
- 驗證：SpanDSP 的 B03 長頁（17／19 dB × 3 seed）、B01–B07、B10、M02–M08，以及單元測試 B04 的 MER 門檻全部通過。
- V.27ter 不受影響，它一直用 Gardner。另外 ADR-064 已經記錄，把重心時脈搬到 V.27ter 也沒有好處。

## ADR-066：音訊引擎的五項 M5e 修正（2026-10-26，M5e 設計待辦 ①③④⑤⑥⑦）
M5c／M5d 測試報告 §5 留下的設計問題，在接上 RTP 之前於 `fax-modem` 與 `fax-in::audioengine` 處理：
- **① 訊號結束改為絕對或相對**：`tone::PresenceDetector` 與 V.27ter／V.29 收端的載波結束，除了低於 −48 dBm0，也接受「比訊號本身的電平低 10 dB」（`tone::END_DROP_DB`、`SignalLevel`，電平是約 50 ms 的平均，已經低於門檻的區塊不算進去）；連續 10 ms 成立才算結束。訊號開始除了 ≥ −43 dBm0，還要比之前的雜訊高 10 dB（`START_RISE_DB`；雜訊估計往下立即跟、往上緩慢跟，訊號結束時以結束那一塊的電平重設）。雜訊高於 −48 dBm0 的線路（M01 的 SNR 30 dB 以下、M04 固定雜訊）因此聽得到 TCF 與頁面的結束。代價：從 −30 驟降到 −45 這種 15 dB 的落差現在算結束（M5a N04 的遲滯案例改從 −38 開始）；串流的第一個區塊只用來估雜訊，若那時訊號已經在（實務上只有 CNG），要等它結束後才聽得到。
- **③ 不處理**：1100 Hz 會符合 V.27ter Segment 3 只在收端提前建立時才是問題；收端仍在 DCS 之後才建立。
- **④ 封包遺失不再截斷頁面**：`AudioEngine::process_lost(n, tx)` 表示這段音訊沒有到；連續 60 ms（`max_lost_skip_ms`）以內收端不餵任何樣本，之後照常接續。20 ms 對所有速率都是整數個符號、整數個載波週期，差分解碼只有缺口後第一個符號錯，擾碼器 23 bit 內自我同步，所以頁面只多幾條壞行。超過 60 ms 的中斷當成靜音餵入，訊號結束仍然聽得到（對方用靜音抑制時不會卡住）。
- **⑤ 音訊路徑沒有 ECM**：`AudioEngine::new` 一律把 `t30.ecm`、`t30.mmr` 設為 false，trunk 開了 ECM 也一樣（只影響 G.711，T.38 照舊）。
- **⑥ DCS 指定沒有收端的數據機**（例如 V.17）：引擎記下 DCS 的 V.21 結束後的下一段訊號就是 TCF，TCF 一結束就以「沒有資料的 TCF」交給 T.30，T.30 立刻回 FTT，對方降速；以前要等 T2（6 秒）。
- **⑦ V.29 的 Segment 4 不算進 TCF**：收端輸出的位元從 Segment 4（48 個符號的擾碼 1）開始，引擎在 `Trained` 之後丟掉這 192／144 bit。9600 全 0 TCF 的 1 從約 3% 降到 0.5% 以下，T.30 5% 門檻的餘裕從 1.9% 變成約 4.5%。收端本身不變（SpanDSP 的 B 測試仍以 Segment 4 開頭比對）。

## ADR-067：G.711 接進 fax-in 的做法（2026-10-26，M5e）
- **每通一個 pool 埠**：UDPTL 仍用 pool 的偶數埠，RTP 用它的下一個奇數埠（pool 不會發出奇數埠）。原本試過另租一個偶數埠給 RTP，同時通話數減半，`e2e_m2d` 的 E04（5 埠 5 通）回 503，所以改用奇數埠。代價：RTP 不在偶數埠（RFC 3550 是 SHOULD），我方不送 RTCP；防火牆要開整個範圍。`[media] audio_port` 只剩 `fax_session = "hold"` 使用。
- **接通就監聽**：以音訊接通的來電立刻建 G.711 session，在 `go` 之前只聽（CNG、V.21）並送靜音 RTP，`go` 時才啟動 T.30（CED、DIS）。T.38 建立時取消它；只監聽過的 session 不留報告。
- **何時 `go`**：`audio-only` 接通時；`auto`／`initiate` 在我方 T.38 re-INVITE 被拒（488／415／606、answer 沒有可用 `m=image`、2xx 沒有 SDP）時；`wait-peer` 在 `initiate_after_secs` 內沒等到對方的 T.38 re-INVITE 時（ADR-061 沒寫到 `wait-peer`，這裡定案：不然這種 trunk 遇到不發 re-INVITE 的對方就一定失敗；送出的 CED 也常是 gateway 切 T.38 的觸發）。`go` 之後對方發 T.38 re-INVITE 仍接受並由 T.38 接手。re-INVITE 逾時或 5xx 仍記為失敗。
- **沒有音訊 session 的 factory**（`hold`、測試用的 factory）維持 M5 之前的行為：不另配 RTP 埠、`g711_passthrough_unsupported`。
- **紀錄**：`CallResult.audio`（codec、RTP 埠、`reason`）、`FaxReport.media` 與 `rtp` 計數、`audio.log`；DB migration 0003 加 `faxes.modem`、`faxes.media`（MySQL 的 `ADD COLUMN` 不能重跑，寫成單一陳述式，重跑時的 1060「Duplicate column name」視為完成）。

## ADR-068：`audio.log` 記錄 T.30 frame；G.711 遺失 0.5 % 的門檻（2026-10-26，M5e 測試與審核）
- **`audio.log` 記錄每個 T.30 frame**：音訊引擎把每個收到（FCS 正確）與送出的 V.21 frame 記成一行 `V.21 rx|tx <名稱> <hex>`，與數據機切換、TCF 結果、T.30 結束的行一起進 `audio.log`（計畫 E06；G.711 的現場問題只能靠它還原交握，T.38 的 `udptl.log` 已有同等資訊）。每通約多 30 行；FCS 錯的 frame 仍只記長度。
- **E04 門檻**：計畫要求 RTP 雙向遺失 0.5 % 加抖動時每通都收下，實測 3 個 seed 有 1–2 個失敗（遺失碰上交握或頁面 training，之後 T2 逾時）。同樣條件下 SpanDSP 對 SpanDSP 失敗 17/40、我方 7/40，所以這是沒有 ECM 的 G.711 的本質，不是收端缺陷。決定：E04 改為「沒有遺失時抖動 ±30 ms 全部收下；遺失 0.5 % 時每通都乾淨結束、收下的頁在壞行門檻內，且失敗率不高於 SpanDSP 對 SpanDSP」（`e04c`），計畫字面版保留為 `#[ignore]` 的紀錄。T.38 仍是預設與優先（ADR-061）。
- **後續改善（不擋發版）**：頁面的 training 遺失時，等影像訊號結束就回 RTN，而不是等 T2 逾時後 DCN，可讓對方重送該頁。


## ADR-069：音訊引擎的時序與門檻修正（2026-10-26，M5b 測試與審核）
- **CED 之後 75 ms 靜音**：`queue_ced` 的 CED 結尾加上 `turnaround_ms`（預設 75 ms）的靜音，符合 T.30 5.3.2（75 ± 20 ms）；以前 DIS 的前導直接接在 CED 後面（M5b V03 發現）。
- **偵測器的位置與引擎時鐘一致**：`PresenceDetector::skip(n)` 讓沒有聽到的樣本（我方傳送中、回音保護期、遺失的封包）也計入位置。以前位置只算聽到的樣本，引擎卻拿它和自己的時鐘比較，所以「對方訊號結束後 75 ms 才回應」（`queue_frames` 的 `peer_off_at + turnaround`）實際上沒有作用，回應在偵測到結束後就送出（審核 V05 時發現）。
- **先聽線路雜訊再送 CED**：ADR-066 假設偵測器聽到的第一個區塊是通話開頭（只會是 CNG），但引擎送完 CED＋第一個 DIS（約 4.5 s）才開始聽，而對方的 DCS 在 55–95 ms 內就開始，於是被當成雜訊底，整個 TSI／DCS 漏掉，要等 T4（3 s）重送 DIS（M5b V05 發現）。現在 CED 之前的 200 ms 靜音（`ced_lead_silence_ms`）期間，偵測器照樣聽線路（事件不使用），雜訊底在第一次傳送前就建立。沒有改成「沒有雜訊底時當作安靜」，因為那會讓開頭就有 ≥ −43 dBm0 固定雜訊的線路被當成訊號一直在（ADR-066 的 M04 條件）。
- **偵測器開啟門檻 −45 dBm0**：`tone::ON_DBM0` 由 −43 改為 −45。T.30 要求收端收得到 −43 dBm0 的訊號，門檻剛好等於它時，區塊電平在門檻上下跳，前導常常偵測不到（M5b V04）。−45 仍在關閉門檻 −48 之上。CNG 偵測不受影響（它的能量門檻本來就是 −48，ADR 見 M5a 報告）。
- **影響**：整通時間軸往後移 75 ms 起，M5d 的 M04「固定 SNR 讓 9600 TCF 失敗」案例原本卡在 95 % 門檻邊（11 dB 時 94.6–94.9 %），改為 10.5 dB（92.9–94.4 %）。

## ADR-070：給專案同事的串接介面：API 金鑰、`fax_records` view、增量查詢（2026-10-27，M6a）
需求（2026-10-27 使用者）：專案同事要串接客製，讀傳真的狀態與資訊、取傳真檔與 PNG 縮圖；他們的程式在**別台機器**，用 **Java 或 Node.js**，可以讀資料庫或呼叫 API。以前只有給瀏覽器用的 session（cookie＋CSRF）、內部資料表，以及只在伺服器本機有效的檔案路徑。
- **API 金鑰**：管理員在 Web 介面（「API 金鑰」頁）或 CLI（`sip-faxd apikey create|list|revoke`）建立。金鑰是 `sfx_` 加 32 個隨機 byte 的 base64url（共 47 字元），只在建立時顯示一次；資料庫只存 SHA-256 與前 12 個字元（列表用來辨認）。呼叫時帶 `Authorization: Bearer <金鑰>`，不需要 cookie 與 CSRF（不是瀏覽器自動帶的憑證，沒有 CSRF 問題）；會改資料的請求仍要 `application/json`。
- **權限逐把選擇**（使用者決定）：`fax:read`（列表、增量查詢、單筆、TIFF／PDF、PNG、計數）一定有；`fax:write`（已讀、備註）建立時勾選。金鑰**只能用在 `/api/v1/faxes` 底下**，設定、帳號、狀態、事件、金鑰管理等一律不接受金鑰（401）。投遞紀錄與重新投遞不開放。以金鑰寫入的已讀與備註記為 `api:<名稱>`。
- **生命週期**：可設到期日；撤銷後保留紀錄（列表看得到誰在何時撤銷），不能再用，也不能復原；名稱不可重複（含已撤銷的）。建立與撤銷寫進稽核（`config_audit`，`api_key:<名稱>`）。最後使用時間與 IP 每把金鑰最多每分鐘寫一次。無效、撤銷、過期的金鑰回同一個 401 `invalid_api_key`，不透露是哪一種。
- **增量查詢**：`GET /api/v1/faxes/changes?after=<cursor>&limit=`，依 `(updated_at, id)` 由舊到新，回傳 `next` cursor；同事每隔一段時間帶上一次的 cursor 呼叫，就能拿到新進與有變動的傳真（接收中、完成、已讀、備註）。只回傳 `updated_at` 早於現在 2 秒的列，避免與正在寫入的交易競態而漏掉；第一次可用 `since=<RFC 3339>` 從某個時間開始。
- **資料庫 view `fax_records`**（migration 0004，四種 DB）：對外只承諾這個 view 的欄位（不含檔案路徑），之後的版本只增加欄位、不改名、不刪除；內部的 `faxes` 表可以照常改。另加 `faxes (updated_at, id)` 索引供增量查詢。文件附四種 DB 建立「只能讀這個 view」帳號的 SQL。檔案與縮圖一律走 API（路徑只在伺服器本機有效）。
- **文件與範例**：`docs/integration.md`（繁中）、`docs/openapi.yaml`（OpenAPI 3.1，服務也在 `/api/v1/openapi.yaml` 提供，不需登入）、`examples/java`（JDK 17 以上，只用內建 `HttpClient`，單檔可直接 `java FaxClient.java` 執行；JDBC 讀 view 的範例）、`examples/node`（Node 18 以上，只用內建 `fetch`）。自簽憑證約兩年更新一次、指紋會變，串接時建議在 `[web]` 設定公司自己的憑證；文件說明 Java 匯入 truststore、Node 用 `NODE_EXTRA_CA_CERTS`。
