# M2d 測試報告：T.38 session、TIFF/PDF 輸出、capture、fax-sim 即時 gateway、E01–E07

- 實作與執行：Sonnet（M2d，M2 的最後一個階段）
- 計畫：`docs/test-plans/M2.md` 第 6 節（E01–E07）；規格：`docs/design/fax-stack.md` §4
- 狀態：**待 Opus 審核**。沒有 commit、沒有 push。`scripts/ci-local.sh` 最後一次完整執行（含 Docker 的 SpanDSP／SIPp／t4 oracle／E01–E07、涵蓋率門檻、musl 靜態建置）**全部通過**（`ci-local: all checks passed`）。本機沒有 `cargo-deny`，該步驟被腳本略過。
- 沒有新增任何依賴（`fax-in` 本來就有 `serde_json`、`tokio`；`fax-sim` 本來就依賴 `t38`；tools 與 Docker 映像不需要新套件，`libtiff-tools`、`poppler-utils` 早已在映像內）。

## 1. 交付內容
| 項目 | 位置 |
|---|---|
| 真正的 T.38 `FaxSession`：UDPTL socket 綁在該通電話的 pool 埠、先送往 SDP 位址、第一個能解析的 UDPTL 封包 latch 對方實際位址（之後其他來源丟棄並計數）、依 SDP 協商設定 redundancy／FEC 與 T.38 版本／`T38FaxMaxDatagram`／rate management／`T38MaxBitRate`、tokio 在 `next_wakeup_ms` 喚醒、datagram 餵 `UdptlEngine`（`Gap`／reorder flush 在引擎內）、T.30 完成／peer BYE／`max_duration_secs` 結束 | `crates/fax-in/src/t38session.rs` |
| 控制器接線：`SessionContext` 多了 `udptl`（trunk 的 redundancy／FEC 設定）與 `report` 槽；session 被取消後最多等 3 s 讓它寫完檔案才讀結果；「正常結束但 T.30 沒成功」（或檔案寫不出來）的通話判為失敗 | `crates/fax-in/src/{session,controller}.rs` |
| 輸出：`<data_dir>/faxes/<YYYY>/<MM>/<DD>/<call-id>.tif` 與 `.pdf`（先寫 `.part` 再 rename；Call-ID 經 `dir_name` 淨化，若有被改動或截斷則加 `-<8 位 hash>`，已存在則加 `-2`…，絕不覆蓋）；部分頁面也寫出並標記 `partial` | `t38session.rs`、`[storage] data_dir`（`config.rs`） |
| `CallResult.fax: FaxReport`（code／code_name／pages／bit_rate／modem／resolution／compression／remote_ident／bad_lines_per_page／duration_ms／partial／tiff／pdf／store_error／udptl 計數），序列化進 `result.json` 與 `call_result` 日誌行 | `crates/fax-in/src/event.rs` |
| `logs/calls/<call-id>/udptl.log`：每個 datagram 一行 `<RFC3339 ms> RX|TX <seq> <hex>`；`keep = "failed"` 與 `max_bytes_per_call`（與 `sip.log` 各自獨立計算）照舊生效 | `crates/fax-in/src/capture.rs` |
| 設定：`[storage] data_dir`、`[call] fax_session = "t38"｜"hold"`、`[call] station_id`、`[trunks.t38] redundancy_depth／fec_span／fec_entries`（含驗證）；`config check` 顯示輸出位置 | `config.rs`、`apps/sip-faxd/src/main.rs`、`conf/sip-faxd.toml.example` |
| **引擎修正（E 測試發現的真實缺陷，見 §5）**：對方已開始下一個訊號時，視我方尚未播完的 burst 為已送完；`t4-non-ecm-sig-end` 的重複封包（含相同的最後幾個 octet）一律視為重複 | `crates/fax-in/src/t38engine/{mod,rx,tx}.rs` |
| fax-sim 即時 gateway：SpanDSP `t38_terminal`（caller）在真實 UDP socket 與牆鐘上送 TIFF，UDPTL 用我們的 `t38` crate（可設 redundancy／FEC、兩個方向各自獨立的帶種子遺失）；SIP UAC 支援「gateway 發 re-INVITE」「伺服器發 re-INVITE」「初始 INVITE 就帶 T.38」；fax 結束後等伺服器 BYE（`peer_bye_wait`），否則自己 BYE | `tools/fax-sim/src/{gateway,sip_uac}.rs`、`src/bin/sip_uac.rs`（`--fax`、`--redundancy`、`--loss`、`--seed`、`--version`、`--mode initial`） |
| `interop::FaxdOptions` 多 `real_fax`／`data_dir`（預設仍是 `fax_session = "hold"`，M1 的 SIPp／sip_uac 訊號測試不受影響） | `tools/interop/src/lib.rs` |
| 測試 | `tools/fax-sim/tests/e2e_m2d.rs`（E01–E07 + E05b）、`crates/fax-in/tests/t38session.rs`（S01–S09，主機、真實 loopback UDP、pacing 關閉）、`crates/fax-in/tests/real.rs`（2 個 `m2d_*`）、`tests/config.rs`（2 個 `m2d_*`）、`tests/t38engine.rs`（新增 R14b、R16 的「早回應」變體） |
| 文件 | `conf/sip-faxd.toml.example`、`README.md`／`README.zh-TW.md`（同步）、`docs/ARCHITECTURE.md`、`docs/TODO.md`、`CLAUDE.md` |

## 2. 指令
```sh
cargo test -p fax-in                                   # 主機：S01–S09、real.rs、config.rs、t38engine.rs
scripts/e2e-docker.sh                                  # tools workspace（Rocky 9 + SpanDSP 3.1.1 + libtiff-tools + poppler-utils），含 E01–E07
# 只跑 E 測試、看每個案例的證據行（開發時用的小幫手，等價於 e2e-docker.sh 內的 cargo test）：
docker run --rm --platform linux/amd64 -v "$PWD":/work -v sip-fax-rs-cargo-registry:/usr/local/cargo/registry \
  -e CARGO_TARGET_DIR=/work/target/docker -w /work/tools sip-fax-rs-test:rocky9 \
  cargo test -p fax-sim --test e2e_m2d -- --nocapture
scripts/ci-local.sh                                    # 全部
cargo llvm-cov -p fax-in --fail-under-lines 70         # 涵蓋率
```
測試數：主機側 `t38session.rs` 9 個、`real.rs` +2、`config.rs` +2、`t38engine.rs` +2（共 30→32 個，R14b 與 R16 的早回應變體為新增）、`t38session.rs` 內單元測試 4 個；Docker 側 `e2e_m2d.rs` 8 個測試（E01、E02、E03、E04、E05（3 個 seed）、E05b（2 個 seed）、E06、E07）。

## 3. 案例結果
### 3.1 比對方法（每個 E 案例都做，缺一不可）
1. `tiffinfo`：目錄數＝頁數、每頁「Compression Scheme: CCITT Group 4」、每頁解析度 204 × (98｜196)。
2. **libtiff 比對**：`tiffcmp -t source.tif ours.tif` 必須成功（ADR-018：一律 `-t`）；**負向對照**：把來源檔第一頁資料中一個 byte 翻轉 1 bit 的副本，`tiffcmp -t` 必須失敗（每個案例都實測失敗，否則該案例失敗）。再以 `tiffcp -c none` 讓 libtiff 解成未壓縮，用測試內獨立寫的最小 TIFF 讀取器取出每頁位圖，與 `fax_sim::tiff_writer::render_bitmap`（SpanDSP 實際收到的同一份來源）逐 byte 相同；對照：翻 1 bit 的位圖必須與解碼結果不同。
3. **poppler 比對**：`pdfinfo` 頁數；每頁 `pdftoppm -mono -rx 204 -ry 98|196`，PBM 的寬高與位圖逐 byte 與來源相同；同樣有翻 1 bit 的對照。
4. SpanDSP 的完成碼必須是 `T30_ERR_OK`（0）；`result.json`（讀自 `logs/calls/<call-id>/result.json`，不是從日誌拼的）的 `outcome.status=completed`、`t38.path`、`fax.code=0`、`code_name=ok`、`pages`、`remote_ident`（TSI）、`bit_rate=14400`、`bad_lines_per_page` 全 0、`partial=false`、`store_error=null`；輸出路徑必須在 `<data_dir>/faxes/YYYY/MM/DD/<call-id>.tif`，`.pdf` 同名。

### 3.2 E 系列（真實 `sip-faxd serve` 執行檔，每個測試自己的設定、埠範圍與資料目錄）
以下是連續 3 次完整執行中的最後一次（三次的資料包數完全相同，因為遺失種子是決定論的；牆鐘時間只差幾十 ms）。「gw」＝fax-sim 的 gateway，「srv」＝sip-faxd。

| ID | 案例 | 結果與證據 |
|---|---|---|
| E01 | gateway 發起 re-INVITE，1 頁（標準解析度，MR，14400） | **通過**。SpanDSP＝0（OK）。`t38.path=peer_reinvite`；`fax`＝code 0／pages 1／TSI `0000000001`／rate 14400／modem v17／compression mr／duration 20063 ms；TIFF 5496 B、PDF 5985 B；libtiff 與 poppler 各 1 頁逐像素相同，負向對照皆如預期失敗。牆鐘 21340 ms。UDPTL：gw 送 293／收 72，srv 收 288／送 72（gw 多的 5 個是 srv 拿到 DCN 後 gw 仍送出的收尾封包，見 E06）。 |
| E02 | 我方（伺服器）發起 re-INVITE，3 頁（fine，196 dpi） | **通過**。`t38.path=our_reinvite`（`initiate_after_secs=0.3`）；pages 3；duration 32433 ms；TIFF 19418 B、PDF 20426 B；3 頁 × 2 工具逐像素相同；每頁解析度 204×196。gw 送 566／收 96，srv 收 561／送 96。牆鐘 33765 ms。 |
| E03 | 初始 INVITE 就帶 T.38（只有 `m=image`） | **通過**。`t38.path=initial_invite`；2 頁；duration 28199 ms；TIFF 12186 B、PDF 12933 B；逐像素相同。gw 送 496／收 84，srv 收 491／送 84。 |
| E04 | 5 通並行（同一個 `port_range`，3 通 gateway re-INVITE、2 通初始 INVITE；頁數 1/2/3/1/2、解析度 standard／fine 混合、各自不同的高度與 TSI `0000000100`…`0000000104`） | **通過**。5 通 SpanDSP 皆 0；每通 `result.json` 的 TSI 與頁數對應自己的來源；5 個不同的 UDPTL 本地埠（`t38.local_port` 兩兩不同）；5 個不同的 TIFF 路徑；每通的 TIFF／PDF 與**該通自己的**來源逐像素相同（若混淆，高度與內容不同必然失敗）。duration 18971／25418／40186／20355／34630 ms。 |
| E05 | 5 % 遺失（兩個方向各自獨立的 PRNG）＋ redundancy 3（兩邊都送），2 頁，3 個 seed（0x5eed0001..3） | **通過**。三個 seed 皆 SpanDSP＝0、`bad_lines_per_page=[0,0]`、逐像素相同。遺失：seed1 gw→srv 掉 25／540、srv→gw 掉 6／84；seed2 23＋4；seed3 31＋8；srv 以 redundancy 補回 25／23／31 個封包，`lost=0`，gw 側 `rx_gaps=0`。duration 約 29.5 s。 |
| E05b（額外） | 3 % 遺失＋**UDPTL FEC**（span 3、entries 3；trunk `fec = true`、gateway 提供 `t38UDPFEC`），2 頁 fine，2 個 seed | **通過**。`t38.udp_ec=t38UDPFEC`；seed 0x5eed0011：gw→srv 掉 10／419、srv→gw 掉 1／84，srv 以 FEC 補回 9 個；seed 0x5eed0012：10＋2，補回 10 個；皆 SpanDSP＝0、逐像素相同。 |
| E06 | capture | **通過**。`udptl.log` 360 行＝srv 收 288＋送 72（由 `result.json` 的計數器）；每行 4 欄、時間遞增、`hex` 可由 `t38::UdptlPacket::decode` 解開、seq 欄＝封包內 seq；TX seq＝0..71 連號、RX seq＝0..287 連號；srv 送＝gw 收（72＝72），srv 收 288 ≤ gw 送 293，缺的 5 個是 srv 拿到 DCN 就停止讀取之後 gw 才送出的最新 seq（RX 前綴無缺口）。`sip.log`（9 則訊息）依序含 `INVITE`→`180 Ringing`→`200 OK`→`ACK`→`m=image`→`BYE`→其 `200 OK`，兩次 `INVITE`（初始＋T.38 re-INVITE）、兩個 `m=image`（offer＋answer）。 |
| E07 | `port_range` 只有 2 個埠；兩通佔住、第 3 通 503、釋放後下一通成功 | **通過**。兩通 holder（初始 INVITE 帶 T.38，保持 4 s）後，以日誌 `UDPTL port leased` ×2 為同步點（沒有固定 sleep）；第 3 通 UAC 收到 `503`；srv 日誌有 `ERROR … no free UDPTL port in [media] port_range: 503 … capacity=2`；`call_result` 有一筆 `reason.kind=no_media_port`（`t38`／`fax` 皆 null）；兩個 holder 結束後第 4 通成功：SpanDSP＝0、TSI／頁數正確、逐像素相同、duration 20046 ms。 |

`call_result` 日誌行（E04 其中一通，節錄）：
```json
{"call_id":"b65407313a33520461e363fe@127.0.0.1","trunk":"sipp","t38":{"path":"peer_reinvite","version":0,"max_bit_rate":14400,"rate_management":"transferredTCF","udp_ec":"t38UDPRedundancy","peer_media":"127.0.0.1:46928","local_port":31006,…},
 "fax":{"code":0,"code_name":"ok","pages":1,"bit_rate":14400,"modem":"v17","resolution":"standard","compression":"mr","remote_ident":"0000000100","bad_lines_per_page":[0],"duration_ms":18963,"partial":false,
        "tiff":"…/data/faxes/2026/10/21/b65407313a33520461e363fe@127.0.0.1.tif","pdf":"….pdf","store_error":null,
        "udptl":{"rx_datagrams":252,"tx_datagrams":72,"rx_foreign":0,"rx_malformed":0,"rx_duplicates":0,"recovered_redundancy":0,"recovered_fec":0,"lost":0}},
 "outcome":{"status":"completed"}}
```

### 3.3 flakiness
`cargo test -p fax-sim --test e2e_m2d`（8 個測試全部並行，約 45–55 s）**連續執行 3 次，3 次全部通過**（53.35 s、45.02 s、45.22 s）；第 4 次是 `scripts/ci-local.sh` 內的完整 Docker 執行（8 個測試通過，45.44 s），第 5 次是修正 `sip_uac.rs` 後的最終 `ci-local`（同樣通過）。沒有固定 sleep：所有同步都是「輪詢直到條件成立」（`wait_results`、`wait_for` 30 s 上限、SpanDSP 逾時 120 s）。已知的時間相依：E 測試是真實時間，一通 1 頁傳真約 20 s（CED 3.2 s、DIS、1.5 s TCF、約 8 s 的 V.17 影像與各個 T.30 間隔）；若 CI 主機慢到 3 倍以上才會碰到 120 s 上限。**但**：修正之前（§5 第 3、4 項）同樣的測試有明顯的不穩定（失敗者隨機），那是引擎的真實競爭條件，不是測試的毛病。

### 3.4 主機側測試（`crates/fax-in/tests/t38session.rs`，pacing 關閉、真實 loopback UDP、手寫 IFP 的 scripted caller）
| ID | 內容 |
|---|---|
| S01 | 2 頁完整收完：fcf 序列 CSI,DIS,CFR,MCF,MCF；`Completed`；code 0、pages 2、rate 14400、TSI、bad lines 全 0、compression mh、resolution standard；檔案路徑 `faxes/YYYY/MM/DD/call_1@host-<hash>.tif`（Call-ID 的 `/` 被淨化所以加 hash）；TIFF 以 `II*\0` 開頭且 ImageDescription 含 `TSI: 12345678`；PDF 以 `%PDF` 開頭；目錄內只有 .tif 與 .pdf（沒有 `.part` 殘留）。 |
| S02 | 第 2 頁中途收到 DCN：`Failed(… partial_dcn …)`、pages 1、`partial=true`、已完成的 1 頁照樣寫出 TIFF 與 PDF。 |
| S03 | 一開始就取消（peer BYE）：`Aborted`、code `aborted`、pages 0、沒有檔案也沒有 `faxes/` 目錄。 |
| S04 | 最後一個 MCF 之後 peer 沒送 DCN 就掛斷：code 0、pages 1、`partial=false`（見偏離 2）。 |
| S05 | `max_duration` 到期：`MaxDuration`，仍產生報告。 |
| S06 | UDPTL 埠被佔用：`Failed("cannot bind UDPTL port …")`。 |
| S07 | SDP 指向沒人監聽的埠、真正的 caller 從另一個埠送來：第一個可解析封包 latch 後 DIS 的重送送到 caller（DIS 要等 T4＝3 s 才重送，所以此測試耗時 3.1 s）；第三方來源被丟棄（`rx_foreign=1`），來自 caller 的垃圾計入 `rx_malformed`。 |
| S08 | `udptl.log`：`keep=all` 有檔、`keep=failed` 且成功的通話沒有檔、失敗的有；行數＝`rx_datagrams＋tx_datagrams`、TX 行數＝`tx_datagrams`、每行格式；`max_bytes_per_call` 超過時截斷並附 `further datagram(s) not stored` 一行。 |
| S09 | `udp_ec` → `ErrorRecovery` 對應。 |
| real.rs `m2d_*` | session 放進 `ReportSink` 的 `FaxReport` 原樣出現在 `CallResult.fax` 與 `result.json`；session 回 `Aborted`（對方掛斷）但 T.30 碼≠0 → 通話判失敗（`Session{detail}` 含碼名與頁數）；`store_error` 同樣判失敗。 |
| R14b／R16 變體（`t38engine.rs`） | 見 §5 第 3、4 項的回歸測試。 |

## 4. 涵蓋率（`cargo llvm-cov`，行；`scripts/ci-local.sh` 最後一次完整執行）
| crate／檔案 | 行涵蓋率 |
|---|---|
| **fax-in（門檻 70）** | **93.76 %**（2790 行、漏 174） |
| t38session.rs | 93.90 %（344 行、漏 21） |
| capture.rs | 89.69 % |
| controller.rs | 90.10 % |
| t38engine/{mod,rx,tx,udptl}.rs | 96.46 % ／ 97.37 % ／ 100 % ／ 98.97 % |
| sip-core（門檻 85） | 93.35 % |
| t38（門檻 85）／t4（門檻 85）／t30（門檻 85） | 98.50 % ／ 98.52 % ／ 97.62 % |

E 系列在 Docker 內執行，不計入 llvm-cov；所以 `t38session.rs` 的主機測試（S01–S09）是涵蓋率的來源，E 系列則是與 SpanDSP／libtiff／poppler 的獨立驗證。fuzz 目標沒有新增（沒有新的解析器；UDPTL／IFP／T.4 的目標原本就有）。

## 5. 失敗與根因（原始輸出摘要）
1. **`recv: Interrupted system call (os error 4)`**（第一次執行 E 測試，7 個全掛）。fax-sim 的 gateway 迴圈以阻塞 `recv_from`（5 ms 逾時）讀 socket，SpanDSP（或其 timer）使行程收到訊號而 `EINTR`。根因在測試工具：把 `Interrupted` 視為可重試。
2. **`gateway timed out: FaxRun { caller_code: None, timed_out: true, wall_ms: 20088, … }`**，但伺服器那邊 `code=0 ok after 1 page`。根因：伺服器收到 DCN 就完成並 BYE，gateway 的 `stop` 旗標在 SpanDSP 回報自己的 phase E **之前**就被設起，迴圈直接退出。修正：伺服器掛斷只是開始一段 `hangup_grace`（10 s）的等待，SpanDSP 回報完成碼才算完成。
3. **頁面有 1～3 個 bad lines，隨機出現在第 2 頁之後**（引擎真實缺陷）。修好上一項後：`e03: left: [0, 1] right: [0, 0]`、`e04: [0, 3]`、`e04: [1]`、`e05: [1, 0]`，多次執行中 E03／E04／E05 隨機有 1～3 個失敗、失敗者不固定（單獨重跑 E04 一次又通過）。根因：T.30 狀態機在 `SendingCfr`／`SendingPostPage` 狀態下丟棄影像資料與 frame；而引擎把我方 burst 的「播完」時間模型化為最後一個 frame 之後再加 flag／FCS（35 bit ÷ 300 bit/s ≈ 117 ms）＋ `sig_end_hold_ms`（100 ms）≈ 217 ms 才送 `TxComplete`。真實 gateway（SpanDSP 在 loopback 上）聽到 CFR／MCF 的最後一個 frame 後立刻開始下一頁的 training 與資料，頭幾包就落在這 217 ms 內被丟棄；M2c 的模擬時鐘每步 20 ms 延遲、X 測試的 SpanDSP 反應較慢而碰不到。修正（`T38Engine::peer_is_transmitting`）：只要收到對方的 frame、影像資料、資料結束或 V.21 preamble／training 指示，而 T.30 還在 `SendingDis`／`SendingCfr`／`SendingFtt`／`SendingPostPage`，就把排程中尚未送出的 `TxComplete` 取出並立刻餵給 T.30（對方已聽完我們的訊號）。回歸測試：`t38engine.rs::r16_peer_that_answers_before_our_burst_tail_is_over_loses_nothing`（對方在我方最後一個 frame 後 5 ms 就開始下一個訊號，3 頁逐像素相同）；**把修正拿掉後該測試失敗**：`left: [1, 1, 1] right: [0, 0, 0]`（每頁 1 個 bad line，與 E 測試的症狀一致）。這同時需要修改 M2c 的測試台（見偏離 6）：舊的 `full_call` 在「過去的時間點」排程對方封包，所以從沒真的測到早回應。
4. **修正第 3 項之後，所有 E 測試反過來失敗**：SpanDSP 的完成碼 `38 (Unexpected DCN after EOM or MPS sequence)` 或 `15 (Received a DCN from remote after sending a page)`；伺服器：`T.30 done: t2_expired_command after 0 page(s), 17908 ms`。根因：SpanDSP 的 `t4-non-ecm-sig-end` 與其重複封包**都帶著最後幾個 octet**（重複 3 次是對付 UDP 遺失）。舊引擎之所以「沒事」，是因為這些重複封包落在 T.30 的 `SendingCfr`／`WaitPostPage` 狀態而被意外丟掉；現在狀態會提前前進，重複封包被當成新資料 → 在 `WaitImage` 啟動一個空白頁 → `T2ExpiredCommand`。修正（`RxAdapter`）：影像已結束（`image_active=false`）之後再收到的 `t4-non-ecm-sig-end` 一律是重複，不論有沒有資料。回歸測試：`r14_sig_end_copies_that_repeat_the_last_octets_are_duplicates`。M2c 測試台的 `send_image` 原本也把資料附在 3 個 sig-end 上（與 SpanDSP 的線上行為相同，但依賴上述意外），改成「資料一次、empty sig-end ×3」。
5. **E06**：`server rx == gateway tx` 失敗（left 288、right 293）：srv 拿到 DCN 就停止讀取，gw 之後仍送出收尾封包（indicator 重複）。這不是遺失；斷言改為 `srv_rx ≤ gw_tx ≤ srv_rx＋8`，並另外要求 srv 的 RX seq 是無缺口的連號前綴。另一個：`sip.log lacks "SIP/2.0 100 Trying"`——sip-core 在 180 之前不送 100（transaction 層 200 ms 後才送），斷言移除 100 Trying。
6. **E07 的 `late call` 與 E05 在第 1 項的 EINTR 下一起失敗**——同一根因，不另列。
7. 格式：`scripts/ci-local.sh` 在我於其執行中途新增 E05b 時，Docker 步驟的 `cargo fmt --check` 失敗（我的檔案還沒格式化）；格式化後重跑。

## 6. 與規格／計畫的偏離與取捨
1. **`[call] fax_session = "t38"｜"hold"`（規格沒有）**：預設 `t38`。`hold` 保留 M1d 的「只維持通話」行為，讓 M1 的 SIPp 互通測試（S01–S11、C03…）與 `sip_uac` 訊號測試——它們的對端不會講 T.38——繼續驗證訊號層；`interop::FaxdOptions::real_fax` 預設關。lib 的 `StartOptions::default()` 與 detached 測試在測試碼內設為 `Hold`。
2. **對方在最後一個 MCF 之後（EOP 已確認）沒送 DCN 就 BYE，視為成功**（`Aborted` 且當時狀態是 `WaitDcn`、已有頁面 → 結果碼改寫為 `ok`，S04）。規格 §3 只說「收到 DCN 要結束」；T.30 在 EOP/MCF 之後傳真本來就完成，而 gateway 在 T.38 結束時直接 BYE、DCN 封包被 BYE 搶先是常見的。T.30 層的 `Aborted` 語意不變。
3. **通話結果的規則**：session 回 `Completed`／`Aborted`（對方掛斷）但 `FaxReport.code ≠ 0` 或 `store_error` 不為空 → 通話判失敗（`FailureReason::Session`）。規格沒有說；理由是「T.38 建立後對方掛斷」不再自動等於成功。
4. **`udptl.log` 為了不在每個封包 open/append/close，對 `keep = "all"` 也先緩衝在記憶體，累積 16 KB 才寫、結束時一次寫完**；程序當掉最多遺失 16 KB。上限仍是 `max_bytes_per_call`（與 `sip.log` 各自計）。
5. **session 取消後控制器最多等 3 s**（`SESSION_FINISH_WAIT`，原本 peer BYE 後只等 1 s）讓它寫完 TIFF／PDF；關機路徑也會等。
6. **改動了 M2c 的測試台**（`crates/fax-in/tests/t38engine.rs`）：`full_call` 現在在對方回應的實際時間點排程（`run_until_fcf`），預設間隔 400 ms（> 我方 burst 尾巴）；`send_image` 的 sig-end 改為空資料 ×3。原因見 §5 第 3、4 項。其餘 M2c 測試未改，全部仍通過。
7. **UDPTL socket 綁在 `[sip] bind` 的 IP**（萬用位址則綁萬用）；沒有另設 `[media] bind`。
8. **latch 規則**：第一個「能解析成 UDPTL」的封包決定位址；之後來自其他位址的一律丟棄（計入 `rx_foreign`）。latch 之前的垃圾封包不會劫持。尚未支援 NAT 後對方位址再改變（例如 gateway 換埠）。
9. **檔名**：Call-ID 以 `dir_name` 淨化，若被改動或超過 100 字元則附 `-<hash8>`（避免 `a/b` 與 `a_b` 同名）；已存在則附 `-2`。規格只說「sanitize」。
10. 計畫 E01–E07 的「CallResult 中 pages／code／TSI」以 `result.json` 的 `fax.*` 驗證；沒有再包一層同名欄位於 `CallResult` 頂層（用巢狀的 `fax` 物件，避免與 M3 的 DB 欄位提前綁死）。
11. **E 測試固定使用 14400 bit/s、non-ECM、MH／MR**（SpanDSP 預設協商結果）。速率／編碼矩陣在 M2c 的 X01（24 格）已涵蓋，E 系列專注在 SIP→T.38→檔案的整條路徑。
12. 附帶：M1d 的 `sip_uac` 測試原本斷言 `peer_media=127.0.0.1:5004`（舊 UAC 寫死的假埠）；UAC 現在提供真的媒體 socket，埠改為 `UacReport.media_port`，測試跟著改。
13. 只有 UDP 做 SIP 傳輸的 E 測試（TCP 的訊號層在 M1d 的 `sip_uac` 測試涵蓋）。

## 7. 擬議 ADR（請 Opus 合併到 `docs/DECISIONS.md`，編號自行調整）
### ADR-023：T.38 session、輸出與通話結果規則（2026-10-21）
- `fax-in::t38session::T38FaxSession` 是 `FaxSession` 的正式實作；socket 在 `SessionContext.t38.local_port`（pool 埠）、bind 於 `[sip] bind` 的 IP；先送往 SDP 位址，第一個能解析的 UDPTL 封包 latch；EC 與版本取自 SDP 協商結果（`udp_ec`、`version`、`rate_management`、`max_bit_rate`、`peer_max_datagram`），redundancy 深度與 FEC 參數取自 `[trunks.t38]`（`redundancy_depth`／`fec_span`／`fec_entries`）。
- 輸出 `<data_dir>/faxes/YYYY/MM/DD/<call-id>.tif|.pdf`（UTC 日期、`.part`＋rename、不覆蓋、Call-ID 淨化加 hash）；部分頁面也寫並 `partial=true`。
- `CallResult.fax: FaxReport`；`Completed` 的通話若 `fax.code≠0` 或 `store_error` → `Failed(Session)`；EOP 的 MCF 之後對方未送 DCN 就掛斷視為成功。
- `[call] fax_session = "hold"` 保留只維持通話的 stub。`udptl.log` 為 `<time> RX|TX <seq> <hex>`，緩衝 16 KB，與 `sip.log` 各自受 `max_bytes_per_call` 限制。
### ADR-024：T.38 引擎對「對方比我方 burst 尾巴早開始」與重複 sig-end 的處理（2026-10-21）
- 對方的 frame／影像資料／資料結束／V.21 preamble／training 指示到達，而 T.30 仍在 Sending* 狀態，引擎立即完成我方排程中的 `TxComplete`（取出佇列中的標記並餵給 T.30）；尚在佇列的 `no-signal` 等封包照常送出。
- `t4-non-ecm-sig-end` 在影像已結束後一律是重複（無論是否帶資料）。
- 回歸測試：`r16_peer_that_answers_before_our_burst_tail_is_over_loses_nothing`、`r14_sig_end_copies_that_repeat_the_last_octets_are_duplicates`；E 測試是獨立證據。
### ADR-025：端到端測試規範（2026-10-21）
- E 測試對真實 `sip-faxd` 執行檔、SpanDSP 即時 gateway（真實 UDP、牆鐘）；每個圖像以 libtiff（`tiffcmp -t`＋`tiffcp -c none` 後逐 byte）與 poppler（`pdftoppm -mono`）**各比一次**，兩者都要有負向對照；同步一律輪詢條件，不用固定 sleep。遺失用兩個方向各自獨立的帶種子 PRNG，使每個方向的遺失型態只取決於自己的封包序號。

## 8. 未解事項（誠實清單）
1. **`udptl.log` 含有傳真內容（影像資料的 hex）**，而 `[capture]` 預設就是 `enabled = true`、`keep = "all"`——預設設定會把每份傳真的影像另存在 `logs/calls/` 一份。需要 Opus／產品決定：預設關閉 UDPTL 內容、只記標頭，或加選項 `udptl_payload = false`；目前依任務書「seq＋hex」照做，沒有擅自改。
2. 速率矩陣、MR／MH／fine 組合只在 X 系列（M2c）驗證，E 系列只跑 14400 bit/s；ECM 與 MMR 在 M3。
3. 真實 gateway（Sonus／AudioCodes／CS210）從未連過；特別是 localTCF 的 CFR 行為（M6，TODO 已有）與「早回應」的實際時間分布。引擎修正是對 SpanDSP 的行為做的，對其他實作應該更寬鬆，但沒有實證。
4. 對方在通話中把 T.38 再 re-INVITE 回音訊、或換 UDPTL 位址（NAT 重新綁定），目前 session 不處理（re-INVITE 回音訊在 T.38 生效後仍回 488；位址只在第一個封包 latch）。
5. E 測試是真實時間，整組約 45–55 s（並行），CI 主機非常慢時要留意 120 s 的單通上限。
6. `peer_is_transmitting` 對 `V21Preamble` 也會提前結束我方 burst：若對端在我方 burst 還沒播完時就開始送自己的 preamble（全雙工的實作），我方的尾巴會被視為已送完。對 T.38 這是合理的（T.30 本來就是半雙工），但沒有真實 gateway 實證。
7. `cargo-deny` 本機未安裝，該步驟被 `ci-local.sh` 略過（CI 上會跑；沒有新增依賴，預期不受影響）。

## 8b. Review 追加（Opus 決議：傳真內容預設不得寫進日誌）
- 新選項 `[capture] udptl = "off"｜"summary"｜"full"`，預設 `summary`。`summary`：每個 datagram 一行 `<time> RX|TX <seq> len=<n> <IFP 摘要>`——indicator 名稱，或 data type＋各 field type 與長度；V.21（HDLC）的 field 附十六進位，frame 結束時再附 `frame=<整個 frame 的 hex>`（伺服器每包一個 octet 地送 frame，所以 frame 要由同方向的封包組起來）；非 V.21 的資料（`t4-non-ecm-data`）只有長度，不含任何 payload。`full`＝原行為（完整 hex），`conf/sip-faxd.toml.example` 與 README 都寫了「會存下傳真內容」的警告；`off` 不產生 `udptl.log`。
- 測試：主機 `s10`（summary：每行 < 200 字元、有 `frame=ffc801`（DIS）、`ffc8c100441e`（DCS）、CFR、MCF、EOP、DCN，只有 `T4NonEcmData(54)` 長度，且已知影像 payload 的前 16 octet 的 hex **不在** log 內）、`s11`（full：該 hex 在 log 內；off：沒有 `udptl.log`）、`s12`（預設值與設定解析）；S08 的行數／keep／大小規則改以 `full` 驗證。E06 改用 `udptl = "full"`（逐行解 UDPTL 並與兩端計數器對帳，行為不變）；新增 **E06b**：預設 summary，行數＝計數器、每行 < 200 字元、含各 T.30 frame 的 hex、最長的 hex 連續字串 ≤ 80 字元（沒有任何影像 payload）。
- 最後一次 `scripts/ci-local.sh` 的結果見交付訊息。

## 9. 異動的檔案
新增：`crates/fax-in/src/t38session.rs`、`crates/fax-in/tests/t38session.rs`、`tools/fax-sim/src/gateway.rs`、`tools/fax-sim/tests/e2e_m2d.rs`、本報告。
修改：`crates/fax-in/src/{lib,config,controller,event,session,capture,timefmt}.rs`、`crates/fax-in/src/t38engine/{mod,rx,tx,udptl}.rs`、`crates/fax-in/tests/{common,real,config,t38engine}.rs`、`apps/sip-faxd/src/main.rs`、`tools/fax-sim/src/{lib,sip_uac}.rs`、`tools/fax-sim/src/bin/sip_uac.rs`、`tools/interop/src/lib.rs`、`conf/sip-faxd.toml.example`、`README.md`、`README.zh-TW.md`、`docs/ARCHITECTURE.md`、`docs/TODO.md`、`CLAUDE.md`。未編輯 `docs/DECISIONS.md`、`docs/ROADMAP.md`（M2 要等 Opus 審核才標完成）。

## Opus 審核結論（2026-10-21）
**結論：通過，M2 完成。**

審核時重新執行 `scripts/ci-local.sh`，全部通過，Docker E 套件（9 個測試）也包含在內。

已檢查：
- **雙重逐像素比對**：每一頁都分別用 libtiff 和 poppler 比對，而且每一項都有反例檢查。
- **測試抓到的真實 bug**：E 測試抓到兩個實際收傳真時才會出現的時序 bug，分別是 burst 尾段丟資料，以及重複的 sig-end。兩個都已修正並加上回歸測試，而且 r16 在修正前確實會失敗。
- **M2c 測試台的盲點**：M2c 的測試台把對方的回應排程在過去的時間點，所以完全沒測到「對方提早回應」的情況。這次已經修正。這也說明了只有 sans-IO 測試是不夠的，真實時鐘的端到端測試有它的必要。
- **隱私**：審核時要求修正，ADR-026 已經實作。
  - E06b 驗證了預設的 summary 模式，T.30 frame 完整、log 裡沒有影像資料。
  - 摘要 log 是 24 KB，full 模式大約 117 KB。

接受的偏離：
- `fax_session = "hold"` 設定。
- 對方在 MCF 之後送 BYE、沒有先送 DCN 時，視為成功。這是常見的情況。
- T.30 回傳碼不是 0，或寫檔失敗時，判定為失敗。

已知限制，列入後續：
- 通話中從 T.38 切回音訊的 re-INVITE 尚未處理（M5，G.711 passthrough）。
- latch 之後對方位址改變的情況尚未處理（M6）。
- ECM 和 MMR 尚未支援（M3）。
