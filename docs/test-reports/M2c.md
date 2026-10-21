# M2c 測試報告：t30 接收端、T.38 接收引擎、與 SpanDSP 互通

- 實作與執行：Sonnet（M2c）
- 計畫：`docs/test-plans/M2.md` 第 4、5 節（R01–R10、X01–X05）；規格：`docs/design/fax-stack.md` §3、§4
- 狀態：**待 Opus 審核**。`scripts/ci-local.sh` 最後一次完整執行通過（含 Docker 的 SpanDSP／SIPp／t4 oracle，涵蓋率閘門 t30 97.62 %）。沒有 commit、沒有 push。

## 1. 交付內容
| 項目 | 位置 |
|---|---|
| T.30 接收端（non-ECM）狀態機 | `crates/t30/src/{frame,caps,config,event,receiver,result}.rs`（純函式 `Receiver::on_event(now_ms, Event) -> Vec<Action>`） |
| sans-IO T.38 接收引擎 | `crates/fax-in/src/t38engine/{mod,tx,rx,udptl}.rs`（`T38Engine`、`UdptlEngine`、`Pacing`、`ReceivedPage`） |
| 與 SpanDSP 互通的 loopback 與 X 測試 | `tools/fax-sim/src/engine_loopback.rs`、`tools/fax-sim/tests/x_interop.rs`；`spandsp-sys`／`spandsp.rs` 加 `t30_set_iaf_mode`、`t30_terminate`、壓縮遮罩；`tiff_writer::render_bitmap` |
| 測試 | `crates/t30/tests/{r01_table,receiver,caps,properties,misc}.rs`、`crates/fax-in/tests/t38engine.rs` |
| fuzz | `crates/t30/fuzz`（`frame_parse`、`receiver_events`），已加入根 `Cargo.toml` exclude 與 `fuzz.yml` |
| 效能量測 | `crates/fax-in/examples/decoder_chunks.rs` |
| CI | `ci.yml` coverage job 與 `scripts/ci-local.sh` 加 `cargo llvm-cov -p t30 --fail-under-lines 85`；`ci-local.sh` 的涵蓋率步驟加鎖（見 §6） |

## 2. 指令
```sh
cargo test -p t30 -p fax-in                       # R01–R10 與引擎測試（主機，純虛擬時鐘，沒有 sleep）
cargo llvm-cov -p t30 --fail-under-lines 85       # 涵蓋率
scripts/e2e-docker.sh                             # X01–X05（tools workspace，Rocky 9 + SpanDSP 3.1.1）
bash <(docker 內)  cargo test -p fax-sim --test x_interop -- --nocapture     # 看每個案例的完成碼
cargo run --release -p fax-in --example decoder_chunks                      # 解碼器吞吐
cd crates/t30 && cargo +nightly fuzz run frame_parse -- -max_total_time=45   # 同樣 receiver_events
scripts/ci-local.sh                                                          # 全部（最後一次：all checks passed）
```
測試數：t30 84 個（`receiver` 58、`caps` 13、`misc` 6、`properties` 4 個 proptest、`r01_table` 3）、引擎 30 個、X 測試 12 個（其中 X01 矩陣一個測試跑 24 通呼叫）。

## 3. 案例結果
### 3.1 R 系列（t30，純狀態機，虛擬時鐘）
| ID | 結果 | 證據 |
|---|---|---|
| R01 | 通過 | `r01_table.rs`：15 個狀態 × 23 種事件 = 345 格，81 格有動作或轉移寫成字面表（`EXPECTED`），其餘 264 格必須「狀態不變、零動作（只允許 log）」，且重複送入第二次仍不變。每個狀態都由一段從新機器重放的腳本到達，並先斷言真的在該狀態。 |
| R02 | 通過 | 19 組設定逐 bit 斷言。`default`＝`00 77 1E`（bit 10,11,12,14,15,16,20,21,22,23）；`t38`＝`20 77 1E`（+bit 3）；V.27ter only＝`00 53 1E`；V.29 only＝`00 63 1E`；V.17 only 強制帶 11、12＝`00 77 1E`；無高速＝`00 43 1E`；標準解析度＝`00 75 1E`；MH only＝`00 74 1E`；B4 寬＝`00 77 9E`（bit 17）；A3 寬＝`00 77 5E`（bit 18）；長度 A4/B4＝`0E`/`2E`；掃描時間 20/5/10/40/0 ms＝`10/18/14/12/1E`；MMR＝`00 77 1F 02`（bit 24 延伸、bit 31）。完整框 `FF C8 01 00 77 1E`，CSI `FF C0 02 8C 4C CC 2C AC 6C EC 1C 4C 0C 04×10`。另外斷言「永不設定」的 bit（1,2,4–9,13,25–30,32 以及沒設 MMR 時的 31），延伸位元恰在後面還有 octet 時才設。**與 SpanDSP 對照**：從黃金向量取出 SpanDSP 的 4 個真實 DIS（`20 77 1F 01 01 99 01 01 01 18` 等，含 V.29-only、V.27ter-only、ECM），`Dis::parse` 讀出的調變／解析度／編碼／ECM 位元與我們的設定一致，前 3 個 octet 與我們的僅差延伸位元。 |
| R03 | 通過 | 10 種合法速率碼＋6 種保留碼（回 `ReservedModem`）、5 種解析度、inch/metric（204×98/196、200×100/200）、MH/MR/MMR 與優先順序、寬度 00/10/01/11（11 視為 10，T.30 表 2 註 6）、長度、掃描時間碼、ECM／uncompressed／高解析度的 `unsupported()`、短於 3 octet 的 DCS。**SpanDSP 真實 DCS** 8 個（V.17 14400／V.29 9600／V.27ter 4800 × 標準／fine，以及 ECM 的 `00 44 1F 22`）解析結果與當時的設定相同。 |
| R04 | 通過 | 長度邊界（14400：2159 octet→FTT、2160→CFR；4800：719→FTT、720→CFR）；零位元比例邊界（3000 octet 中 150 個全 1＝5.000 %→CFR、151→FTT）；FTT 後降速重試（14400 壞 TCF→FTT，9600 乾淨 TCF→CFR，結果 `bit_rate=9600`）；沒有 TCF 時 T2 在精確的 16000 ms（DCS 在 10000 ms）送 FTT；資料閒置 15000 ms 與 no-signal 都能結束 TCF。 |
| R05 | 通過（與規格有偏離，見 §7-1） | localTCF：DCS 後不收 TCF，也**不回 CFR**，直接進入等影像（`start T2=6000`）；設定 `local_tcf_sends_cfr` 時才回 CFR（同一毫秒）；影像沒來則 T2 逾時。 |
| R06 | 通過 | MPS→MCF→下一頁，每頁位圖與來源**逐像素相同**；EOP→MCF→等 DCN→`Ok`，DCN 沒來則 T2（6000 ms）後仍為 `Ok`；EOM→MCF→TxComplete（2400 ms）後 `start T1=35000`＋重送 `tx[02,01]`，T4 在 5400 ms，再收到不同解析度的 DCS 會重新訓練；壞行門檻（200 列）：0／10（5 %）→MCF，11→RTP，20（10 %）→RTP，21→RTN，RTN 的頁不交付；連續壞行：標準 10 行 MCF／11 行 RTN，fine 19 行 MCF／20 行 RTN；RTN 後重訓練並重送同一頁；RTN 後 DCN→`PageRejected`；RTP 接受該頁並等重訓練；MR（k=4）與 MMR 的 DCS 用對的編碼解碼；2048 寬＋fine 的頁面幾何；信號結束遺失時由後續命令推定；空頁→RTN；`max_rows` 生效（`RowLimit`）。 |
| R07 | 通過 | DIS（含 CED）在 t=0 送出；TxComplete 於 3400 ms→T4 6400 ms；第 1 次重送 6400 ms、第 2 次 10000 ms、第 3 次 13600 ms；最後一個 T4 在 **17200 ms** 結束呼叫（先送 DCN，TxComplete 17500 ms 後 `Done(NoResponseToDis)`，duration 17500），之後沒有任何計時器殘留。T1：沒有 TxComplete 時在 **35000 ms** 失敗（`T1Expired`），34999 ms 時仍在 `SendingDis`；第一個有效 DCS 取消 T1 與 T4。T2：等影像／等後續命令／等重訓練都在恰好 +6000 ms 失敗（5999 ms 時仍在原狀態）；自訂 T1/T2/T4/重送次數生效；已停止的計時器的過期事件被忽略。 |
| R08 | 通過 | DCN 在 6 個活動狀態都結束呼叫、取消所有計時器、**不回任何 frame**；DCN 帶 X 位元（SpanDSP 的 `0xDF`）也辨識；頁面接收中 DCN（有 1 頁完成）→`PartialDcn`，pages=1、`is_partial()`、`bad_lines_per_page=[0]`，已完成頁面保留；FTT 後 DCN→`TrainingFailed`；一開始 DCN→`RemoteDisconnect`；Abort 不送任何東西（`["cancel T2","done aborted"]`）；Done 之後所有事件零動作。 |
| R09 | 通過 | CRP（含 X 位元）在等待狀態重送上一個回應：DIS 群（`tx[02,01]`）、CFR、MCF；回應還在送出中或在收影像時忽略；對方重送 MPS（MCF 遺失）→重送 MCF，不重複計頁；新影像開始後同樣的 MPS 就是新的一頁。 |
| R10 | 通過 | FCS 壞的 frame：零動作、狀態與計時器不變（T4 照跑）；之後重送的好 frame 被採用；phase D 的壞命令被忽略，T2 仍有效；壞 TSI 不被記住；`crp_on_bad_fcs` 時回 `tx[58]`（CRP）且只在等待狀態；address/control 錯誤、過短的 frame 被忽略。 |
| 其他 | 通過 | 不預期 frame 容忍 3 次，第 4 次 DCN＋`UnexpectedFrames`；DCS 要求 ECM／uncompressed／superfine／300／400 dpi→DCN＋`IncompatibleDcs`；保留的調變碼→FTT；DCS 過短→忽略；TCF 中途重送 DCS 重新訓練；回應送出中的 DCS 被忽略。proptest：`receiver_is_total_and_consistent`（任意事件序列 2000 例：Done 後無動作、至多一個 Done、取消的計時器必在執行中、頁號連續、送出的 frame 必可解析）、parser 全函式、DIS 對任意設定 round-trip。 |

### 3.2 引擎（R11–R18，`crates/fax-in/tests/t38engine.rs`，30 個）
| ID | 內容與證據 |
|---|---|
| R11 | **精確毫秒**：`start(0)`→no-signal×3 @0、CED×3 @200、（CED 保持 3 s）no-signal×3 @3200、v21-preamble×3 @3275、DIS 6 個 octet @4275／4301／4328／4354／4381／4408、`fcs-OK-sig-end`×3 @4434、結尾 no-signal×3 @4658（內部 4 658 329 µs）。T4 後第一次重送的 preamble @7733；4 次 DIS＋DCN 的 preamble 於 3275／7733／12191／16649／21108，結果 `NoResponseToDis`。`Pacing::none()` 時整個 burst 都在 start 的同一毫秒，frame 整包一個 field，T4 仍在恰好 +3000 ms。 |
| R12 | 指示重複與 sig-end 重複可設定（1/2/3/5、1/3/3/2），0 視為 1；CSI 的 `fcs-OK` 只送一次。 |
| R13 | 每個 frame＝若干 `hdlc-data` ＋ `hdlc-fcs-OK`，burst 最後一個＝`hdlc-fcs-OK-sig-end`；CSI 23 個單 octet 封包＋fcs-OK＋DIS 6 個＋sig-end×3；CSI 的號碼為反序、每個 octet bit-reverse；`hdlc_chunk_ms` 控制每包 octet 數。 |
| R14 | 對方重複的指示與 sig-end 副本只算一次（`rx_counters` 看得到被丟的重複數），額外的 sig-end 無作用。 |
| R15 | `on_loss` 使進行中的 frame 變壞 FCS 並被忽略（log 有 `bad FCS`），不污染下一個 frame；gateway 的 `fcs-BAD` 不採用；沒有 preamble 指示的 HDLC 被接受、但不是以 `FF` 開頭的視為缺頭；fcs 欄位夾帶 octet（Cisco 式）與 `hdlc-sig-end` 容忍；超長 frame 標壞；V.21 型別上的影像欄位被忽略。 |
| R16 | 整通呼叫（pacing 開，虛擬時鐘）：3 頁 MH 14400，**每頁解出的 MMR 與來源位圖逐像素相同**，並可寫成 TIFF／PDF；4800／9600 的 TCF 長度；localTCF（無 CFR）；FTT 後降到 9600 再成功；MMR 頁（手工 DCS bit 31）；頁面中途 DCN→`PartialDcn` 且已完成頁面保留；abort 後不再輸出。v3 以 PER octet 往返（含尾端補零的封包）。 |
| R17 | `next_wakeup_ms`；時鐘一次跳 60 s，所有事件仍依序發生；Done 後輸入被忽略；log 有上限；V.8／ECM／V.34 輸入被計入 ignored。 |
| R18 | UDPTL 包裝：序號 0.. 與 redundancy 深度 3；掉 1 個資料包由下一個的 redundancy 補回（`recovered_redundancy=1`、無 gap）；無法補回則 reorder 等待 50 ms 後 flush，產生 1 個 Gap→壞 FCS；損毀的 IFP 計為遺失。 |

### 3.3 X 系列（SpanDSP `t38_terminal` 當 caller，我們的引擎當 answerer，Docker）
每通：UDPTL（redundancy 3，每個複本一個資料包）、20 ms 步進、單向延遲 20 ms。每一頁都用 `t4` MMR 解碼並與 `tiff_writer::render_bitmap` 的來源位圖**逐 octet 比較**；同時斷言 SpanDSP 完成碼與我方 `ResultCode`。
| ID | 結果 |
|---|---|
| X01 | **24 通全部成功**（頁數 1/3 × standard/fine × SpanDSP 強制 MH／MR × 4800（V.27ter）／9600（V.29）／14400（V.17）；`t30_set_supported_modems`／`t30_set_supported_compressions`）。每通：SpanDSP＝OK(0)、我方＝`ok`、`bit_rate`、`compression`、TSI＝`0000000001`、`bad_lines_per_page` 全 0、y_dpi＝98/196、逐像素相同。模擬時間（ms）：1 頁 MH 4800／9600／14400＝37620／26300／23920；MR（標準）＝30100／22540／21420，（fine）＝26320／20660／20160；3 頁 MH＝97220／58560／47200，MR（標準）＝70540／45220／38260，（fine）＝57080／38480／33800。另一個測試：標準／fine／標準三頁（SpanDSP 在解析度改變時用 **EOM**）成功，3 頁逐像素相同，確認 EOM→MCF→新 DIS 的路徑。 |
| X01-MMR | **無法做到的格子**：SpanDSP 在 non-ECM 下 `mutual_compressions` 會去掉 T.6（T.30 規定 T.6 只能搭配 ECM），且無論遮罩為何都保留 MH，所以「強制 MMR」的呼叫是用 MH 完成（我方 DIS 有沒有宣告 bit 31 都一樣），SpanDSP＝OK、我方＝ok、`compression=Mh`。MMR 解碼由 R06／R16 的手工 DCS 與 t4 的 F03 涵蓋；與 SpanDSP 的 MMR 互通待 M3 的 ECM。 |
| X02 | **5 個 seed（0x5eed0001..5）× 5 % 遺失 × redundancy 3 × 2 頁全部成功且逐像素相同**。遺失與補回（兩個方向合計）：seed1 送 580／掉 27（4.7 %）／我方補回 24＋對方補回 3；seed2 580／29（5.0 %）／22＋7；seed3 580／35（6.0 %）／26＋9；seed4 580／25（4.3 %）／23＋2；seed5 580／32（5.5 %）／24＋8；五個 seed 的 gap 都是 (0,0)。**對照組**（同樣 seed、無錯誤恢復）：seed1 `ok` 但壞行 [11,7]；seed2 [4,20]；seed3 SpanDSP＝20、我方＝`remote_disconnect`；seed4 [11,6]；seed5 [11,10]——沒有一個逐像素乾淨，證明案例確實需要 redundancy。另外 FEC（span 3、entries 3、3 % 遺失）5 個 seed 也成功且逐像素相同。 |
| X03 | transferredTCF 與 localTCF（SpanDSP 設 `T30_IAF_MODE_NO_TCF`，我方 `TcfMode::Local`）都成功、2 頁逐像素相同（29600 ms 與 25360 ms，省下 TCF 時間）。不對稱（SpanDSP 不送 TCF、我方等 TCF）：SpanDSP＝「Invalid response after sending a page」、我方＝`training_failed`，14840 ms 乾淨結束，沒有卡住。 |
| X04 | T.38 v0／v1／v2／v3 皆成功、2 頁（fine）逐像素相同（26820 ms）。 |
| X05 | **缺 tx 檔案**：SpanDSP＝41（`TIFF/F file cannot be opened`），我方＝`remote_disconnect`（pages=0），7680 ms 結束。**中途掛斷**（`t30_terminate`，3 頁來源，SpanDSP 不送 DCN）：@9000 ms（訓練中）SpanDSP＝49（call dropped）／我方＝`t2_expired_command`、0 頁；@18000 ms SpanDSP＝49／我方＝`t2_expired_image`、**保留 1 頁**；@30000 ms SpanDSP＝49／我方＝`t2_expired_command`、**保留 2 頁**，已交付的頁面逐像素相同。**對方完全沉默**：`no_response_to_dis`，25380 ms（4 次 DIS＋DCN）。全部在 `max_sim_ms` 內結束。 |

## 4. 涵蓋率（`cargo llvm-cov`，行）
| crate／檔案 | 行涵蓋率 |
|---|---|
| **t30（門檻 85）** | **97.62 %**（965 行、漏 23） |
| caps.rs | 99.46 % |
| config.rs | 100 % |
| event.rs | 100 % |
| frame.rs | 99.21 % |
| receiver.rs | 96.22 % |
| result.rs | 100 % |
| **fax-in（門檻 70）** | **94.07 %** |
| t38engine/mod.rs | 96.24 % |
| t38engine/rx.rs | 95.61 % |
| t38engine/tx.rs | 100 % |
| t38engine/udptl.rs | 92.55 % |
| sip-core（門檻 85） | 93.35 % |
| t38（門檻 85）／t4（門檻 85） | 98.50 % ／ 98.52 % |

fuzz：`frame_parse` 跑 45 s（1067 萬次）、`receiver_events` 跑 60 s（179 萬次）皆無 crash／斷言失敗；receiver_events 斷言「Done 後無動作、至多一個 Done、取消的計時器必在執行中、送出的 frame 可解析、頁位圖大小正確」。

## 5. 失敗與根因（原始輸出摘要）
1. **DIS 多設了位元**（R02 對照 SpanDSP 前發現）：延伸位元我原本對每個 octet 都設（`for o in 1..octets`），於是 3-octet DIS 的 bit 8、bit 16 被設起，bit 16 正是「2D 編碼」。測試的 `DcsSpec` 同樣錯誤，症狀是頁面全部 RTN（`left: ["cancel T2","tx[32]"]`，DCS 被讀成 MR）。根因：T.30 只有第 3 個 octet 起才有延伸位元（bit 24、32、…）。修正為 `for o in 3..octets`，並以 SpanDSP 的 DIS（`20 77 1F …`，bit 24 設起、bit 16 是 2D 能力）驗證。
2. **識別字串的 octet 是 bit-reversed ASCII**：X01 第一次執行 `remote_ident` 為 `Some("")`。SpanDSP 的 TSI 在線上是 `FF C0 C2 8C 0C 0C …`（`1`＝`0x8C`，空白＝`0x04`），我原本當成普通 ASCII。根因：T.30 欄位的 bit 順序；修正 `encode_ident`／`decode_ident` 逐 octet `reverse_bits`，R02 的 CSI 改以明確 hex 斷言，與黃金向量的 SpanDSP CSI（`4C 0C… 04…`）一致。影像資料與 HDLC 的其他欄位則不需要反轉（X01 逐像素通過可證）。
3. **localTCF 不能回 CFR**：X03 的 localTCF 失敗，`SpanDSP says Some(13) (Unexpected message received)`，我方 `T2ExpiredImage`。根因：SpanDSP 在 `NO_TCF` 下 DCS 後直接送影像，不等 CFR；我們的 CFR 送出要約 1.4 s，期間影像資料被忽略（`SendingCfr` 不收資料），且 CFR 在影像中途到達被 SpanDSP 當成非預期訊息。依規格 §3 字面「收到 DCS 後直接回 CFR」在這個狀況是錯的；改為預設不回 CFR（`local_tcf_sends_cfr` 可開）。見偏離 1。
4. **X01 MMR 格**：預期失敗（INCOMPATIBLE），實際 SpanDSP 用 MH 成功，見 §3.3；測試改為斷言真實行為，並把 MMR 缺口列為未解事項。
5. **引擎測試的自身錯誤**（非產品缺陷）：`start()` 後沒有釋放「現在就到期」的封包（`Pacing::none()` 下 `poll_output` 為空）→ `start`／`on_ifp` 結尾補 `advance`；測試腳本的 DCS 常數 `00 41 1E` 其實設了 bit 16（MR），改成 `00 44 1E`；caller 回應晚於 T4 導致 DIS 又被重送——測試改成 DIS 發完後 300 ms 內回應。
6. clippy：`field_reassign_with_default`、`type_complexity`、`bool_comparison`、`if_same_then_else`（測試碼）都已修正；`cargo fmt` 在 tools workspace 的格式差異導致 `scripts/ci-local.sh` 第一次在 Docker 步驟失敗（`cargo fmt --check`），已格式化。

## 6. 殘留事項調查
### 6.1 sip-core 涵蓋率一次掉到 83.5 %
- `cargo test -p sip-core` **連跑 20 次無任何失敗**；再在 8 個忙碌迴圈（CPU 全滿）下以 `--test-threads=64` 連跑 20 次，**無失敗**（log 裡沒有 `FAILED`／`panicked`）。`cargo llvm-cov -p sip-core --summary-only` 連跑 5 次：93.31 %、93.35 %、93.33 %、93.33 %、93.31 %。**無法重現，沒有找到不穩定的測試。**
- 另做了一個實驗：同時啟動兩個 `cargo llvm-cov -p sip-core`（共用 `target/llvm-cov-target`）。其中一個以 `error: test failed … No such file or directory (os error 2)` 失敗（另一個正常 93.33 %）——`cargo llvm-cov` 開始時會清掉共用目錄中的 profraw／可執行檔，兩個行程互相破壞對方的資料。M2a 當時有另一個 agent 在同一個 target 目錄編譯／量涵蓋率，**這是最可能的原因（推論，沒有當時的原始 log 可證）**；它也解釋了為何同一指令之後連續 7 次都正常。
- 緩解：`scripts/ci-local.sh` 的涵蓋率步驟以 `mkdir target/.llvm-cov.lock` 序列化（結束或失敗都會釋放）。CI 上每個 job 是獨立機器，不受影響。

### 6.2 t4 解碼器在小量餵入下的吞吐
`cargo run --release -p fax-in --example decoder_chunks`（一頁 A4 fine，2200 列，14400 bit/s 線路時間 = 資料量×8/14400；量測時背景有 sip-core 的壓力測試在跑，數字偏保守）：

| 資料 | 大小 | 線路時間 | 1 byte/包 | 4 | 16 | 54（30 ms 包） | 300 | 整包 |
|---|---|---|---|---|---|---|---|---|
| 文字樣式 MH | 203 KB | 113 s | 238 ms | 61 ms | 19 ms | 8.7 ms | 4.9 ms | 4.2 ms |
| 文字樣式 MR | 277 KB | 154 s | 396 ms | 123 ms | 35 ms | 18 ms | 8.3 ms | 6.6 ms |
| 文字樣式 MMR | 300 KB | 167 s | 463 ms | 118 ms | 37 ms | 17 ms | 9.1 ms | 7.7 ms |
| 純雜訊 MH（最壞，列長約 420 byte） | 922 KB | 512 s | **3.5 s** | 879 ms | 268 ms | 88 ms | 33 ms | 22 ms |
| 純雜訊 MR／MMR | 984 KB／1.0 MB | 547／556 s | 3.1 s／3.1 s | 0.78 s | 0.22 s | 94 ms | 45–47 ms | 31–34 ms |

最壞情況（雜訊、**每包 1 byte**，這已超過引擎實際的包大小——T.38 非 ECM 影像包至少 30 ms×14400/8000＝54 byte）也只用掉線路時間的 0.68 %（3.5 s／512 s）；引擎實際的 54 byte 包是 0.017 %。**二次方的重解成本存在（1 byte→整包約 150 倍）但絕對值可忽略，不需要把解碼器改成可續解。** 結論：關閉此待辦。

## 7. 與規格／計畫的偏離與取捨
1. **localTCF 不回 CFR**（規格 §3、計畫 R05 寫「收到 DCS 後直接回 CFR」）。理由：X03／SpanDSP 實測（§5-3）；T.38 的 localTCF 是由 gateway 在本地完成訓練，接收端若再回 CFR 會在對方送頁面時到達。仍保留 `local_tcf_sends_cfr`（預設關）給需要的 gateway。請 Opus 確認；需要改規格文字。
2. **`on_event(now_ms, event)` 多了時間參數**：規格寫 `on_event(Event)`。時間只用來算結果的 `duration_ms`，計時器仍是 `Action::StartTimer{ms}`／`Event::Timer`，所以機器仍是純函式。
3. **多了 `Event::TxComplete`**：T4／T2 要從「我方 burst 完全送出」起算（R07 的精確時間點依賴它）。引擎以排程好的 burst 結束時刻產生。
4. **MMR 預設不在 DIS 宣告**（`mmr=false`）：T.30 規定 T.6 只能搭配 ECM；規格 §3 的 DIS 項目有 MMR，保留為設定項，DCS 選 T.6 時仍會解碼（R06／R16）。與 SpanDSP 的 MMR 互通待 M3。
5. **DIS 只有 3（或 4）個 octet**，不像 SpanDSP 帶到 bit 80；沒有宣告 inch／metric 偏好、第三方向的選項。
6. **Abort 不送 DCN**，立即 `Done(Aborted)`；protocol 自己判定的失敗才先送 DCN 再結束。
7. **T2 逾時在 `WaitDcn`（EOP 的 MCF 之後）視為成功**：頁面都已確認，對方只是沒有送 DCN（SIP BYE 會先到）。
8. **頁面品質門檻**：RTN＝壞行 > 10 % 或連續壞行 > 2.5 mm（標準 10 行、fine 19 行，四捨五入）或 0 列或 MMR 中止；RTP＝壞行 > 5 %（SpanDSP 的 `<5 %` 與 `<15 %` 分級）；其餘 MCF。規格只寫了 RTN 條件，RTP 的 5 % 是我的選擇（可設定）。
9. **TCF 以「零 bit 比例」判斷**（≥95 %），長度下限 `rate × 1.2 s / 8`。
10. **引擎的 pacing 完全採 SpanDSP 3.1.1 `t38_terminal`**（表在 `t38engine/tx.rs`）：preamble 後等 1 s（規格寫「≥75 ms」，T.30 要求 V.21 前導 1 s ±15 %，75 ms 是 no-signal 後的靜默），V.21 資料每包 30 ms 目標（300 bit/s＝每包 1 octet），frame 結束等 stuffed bit＋35 bit 的線上時間，最後加 100 ms。這對真實 gateway 的相容性尚未驗證（只有 SpanDSP）。
11. **UDPTL 包裝**：reorder 等待 60 ms 後 `flush`（接收端沒有自己的時鐘）；無法解碼的 IFP 視為遺失。FEC 恢復的封包以 `decode_prefix` 容忍補零。
12. **新增依賴：無**（t30 只依賴 t4，fax-in 加 t38／t30／t4 的 path 依賴；fuzz 的 `libfuzzer-sys` 同其他 crate）。`tools/Cargo.lock` 與根 `Cargo.lock` 因新的 path 依賴自動更新。

## 8. 擬議 ADR（請 Opus 合併到 `docs/DECISIONS.md`，編號自行調整）
### ADR-019：T.30 線上 octet 慣例與識別字串（2026-10-21）
- `t30` 的 frame 一律用 T.38 `hdlc-data` 的 octet 慣例（T.30 表格的書寫順序，最先送出的 bit 為 MSB）：最終 DIS 是 `FF C8 01 …`、DCS 是 `FF C8 C1 …`、X 位元是 FCF 的最高位元；DIS／DCS 的「bit n」＝FIF 第 n 個 bit＝`fif[(n-1)/8] & (0x80 >> ((n-1)%8))`。SpanDSP 記憶體內的 bit 反轉表示（`FF 13 80`）永遠不出現在本 repo。
- CSI／TSI：字元反序、補空白到 20 個 octet，**每個 octet 再 bit-reverse**（`1`＝`0x8C`、空白＝`0x04`）。T.4 非 ECM 影像資料與 HDLC 無須反轉。
- 延伸位元只在第 3 個 octet 起（bit 24、32、…）。依據：SpanDSP 3.1.1 原始碼與其真實封包；ITU 原文離線無法取得，請 Opus 對照 T.30 表 2 複核（尤其是掃描時間碼 21–23 的 5／10／40 ms 對應與 V.33 的 bit 13，這兩項只有 SpanDSP 的對照或沒有）。
### ADR-020：T.30 接收端的事件／動作介面與 localTCF（2026-10-21）
- `Receiver::on_event(now_ms, Event) -> Vec<Action>`；`Event` 有 `Start`、`Frame{data,fcs_ok}`、`Signal`、`ImageData`、`DataEnd`、`TxComplete`、`Timer`、`Abort`；`Action` 有 `SendSignal(Ced)`、`SendFrames`（一個 V.21 burst）、`StartTimer`、`CancelTimer`、`Page`、`Log`、`Done`。計時器 T4／T2 從 `TxComplete` 起算。失敗先送 DCN（`SendingDcn`），`Done` 在 DCN 送出後；Abort 與收到 DCN 直接 `Done`。
- localTCF：收到 DCS 後不回 CFR，直接等影像（`local_tcf_sends_cfr` 預設關）。transferredTCF：零 bit ≥95 %、長度 ≥ `rate×1.2 s`。
- 頁面品質：RTN／RTP／MCF 門檻見報告 §7-8；RTN 的頁面不交付。
### ADR-021：T.38 接收引擎的 sans-IO 介面與 pacing（2026-10-21）
- `fax_in::t38engine::T38Engine`：`start/advance/on_ifp/on_loss/abort/poll_output(poll_timed)/next_wakeup_ms/result/take_pages`；時間為呼叫者給的毫秒，內部以 µs 排程（V.21 300 bit/s 無法整除毫秒）。`UdptlEngine` 為薄包裝。輸出頁面一律重新編成單一 MMR strip（`ReceivedPage`），檔案輸出在 session 層。
- pacing 與重複次數採 SpanDSP `t38_terminal` 的行為（指示 ×3、資料 ×1、sig-end ×3；CED 前 200 ms、CED 3 s、burst 前 75 ms、preamble 1 s、30 ms 一包、frame 尾 stuffed+35 bit、burst 尾 100 ms）。接收端以「情境」丟棄重複的指示與 sig-end，不依賴序號。
### ADR-022：SpanDSP 互通的限制與測試規範（2026-10-21）
- SpanDSP 3.1.1 在 non-ECM 下不會使用 T.6，且無論遮罩都保留 MH；non-ECM 的「MMR 互通」不可能，改在 M3 的 ECM 驗證。
- SpanDSP 的 `NO_TCF` 模式不等 CFR，是 localTCF 對端的行為參考。`t30_terminate` 不送 DCN，只會讓我方走 T2 逾時。
- 協定 crate 的涵蓋率量測不得同時在同一 target 目錄執行兩個 `cargo llvm-cov`（`scripts/ci-local.sh` 已加鎖）。

## 9. 未解事項（誠實清單）
- **ITU T.30 原文沒有核對**（離線）：DIS／DCS 的位元位置只與 SpanDSP 3.1.1（原始碼與它實際送出的封包）對照。已知只有 SpanDSP 對照或沒有對照的點：V.33 的 DCS 碼（bit 13）、最小掃描時間碼 21–23 除了 20 ms（000）與 0 ms（111）之外的數值、inch／metric（bit 44）。我們只宣告 0 ms／unlimited length／A4，R02 中的其他值是依 SpanDSP 的對應。
- **只對 SpanDSP 驗證**，沒有對真實 gateway（Sonus／AudioCodes）驗證 pacing 與 localTCF 的回應行為（M6 的擷取回歸）。
- **沒有 ECM**（M3）；與 SpanDSP 的 MMR 互通因此尚未驗證。
- 引擎尚未接上 socket、`T38FaxSession`、`udptl.log`、TIFF／PDF 檔案輸出與 SIP 端到端（M2d）；`UdptlEngine` 的預設參數尚未與 SDP 的 `T38FaxMaxDatagram`／`T38FaxUdpEC` 連動。
- `EngineConfig` 尚無 TOML 設定對應（pacing、門檻、timers），M2d 一起做。
- sip-core 的 83.5 % 只是「最可能的原因」，沒有當時的 log 可證實。
- `cargo-deny` 本機仍未安裝（`ci-local.sh` 跳過）。
- 引擎測試以自己寫的簡易 caller 驗證（R11–R18），與 SpanDSP 的互通在 X 系列；兩者都沒有覆蓋 SpanDSP 作為「被叫端」的方向（我方當 caller 屬 M5 之後）。

## 10. 異動的檔案
- 新增：`crates/t30/**`（src、tests、fuzz）、`crates/fax-in/src/t38engine/**`、`crates/fax-in/tests/t38engine.rs`、`crates/fax-in/examples/decoder_chunks.rs`、`tools/fax-sim/src/engine_loopback.rs`、`tools/fax-sim/tests/x_interop.rs`、`docs/test-reports/M2c.md`。
- 修改：`Cargo.toml`（exclude）、`Cargo.lock`、`tools/Cargo.lock`、`crates/fax-in/{Cargo.toml,src/lib.rs}`、`tools/fax-sim/{Cargo.toml,src/{lib,spandsp,t38_loopback,audio_loopback,tiff_writer}.rs}`、`tools/spandsp-sys/src/lib.rs`、`.github/workflows/{ci,fuzz}.yml`、`scripts/ci-local.sh`、`CLAUDE.md`（涵蓋率門檻一行）、`docs/TODO.md`、`docs/ARCHITECTURE.md`。

## Opus 審核結論（2026-10-21）
**結論：通過。**

審核時重新執行 `scripts/ci-local.sh`，全部通過，SpanDSP 的 X 系列測試、SIPp 和 t4 的對照測試都包含在內。

已檢查：
- **R01**：345 格表格驅動測試。
- **R02**：DIS 的 FIF 逐 bit 斷言，並和 SpanDSP 實際送出的 DIS/DCS 交叉比對。
- **X01**：24 格互通矩陣全部逐像素相同，雙方的完成碼都有斷言。
- **X02**：對照組（不做 redundancy 補回）每個 seed 都失敗，證明這個情境確實需要補回。
- 互通測試抓到 3 個真的 bug：DIS 的 extend bit、CSI 和 TSI 的位元反轉、localTCF 時不應該送 CFR。這證明有獨立對照的價值。

確認接受的偏離：
1. **localTCF 預設不送 CFR**：這和 SpanDSP 的行為一致。SpanDSP 是 Asterisk 和 FreeSWITCH 的傳真引擎，長期和真實 gateway 互通，所以預設跟它走，保留 `local_tcf_sends_cfr` 旗標可以切換。**M6 用 CS210 的實際擷取驗證**，屆時依結果調整 `cht-cs210` 預設組。
2. **DIS 預設不宣告 MMR**：T.30 規定 T.6 只能在 ECM 下使用，這是正確做法。X01 的 MMR 格因此無法測試，**列為 M3（ECM）必測**。
3. `now_ms` 參數、`TxComplete` 事件、RTP 門檻 5%：接受。

需要追蹤：
- 這次沒有取得 ITU T.30 原文，bit 位置只對照了 SpanDSP 原始碼和實際封包。V.33（bit 13）、20ms 以外的掃描時間碼是弱點。M3 要取得 T.30 原文（ITU 網站可以免費下載）重新核對，並補上逐 bit 測試。
- sip-core 涵蓋率掉到 83.5%：推論是兩個 llvm-cov 同時執行時共用 target 目錄造成的。`ci-local.sh` 已經加鎖序列化。CI 每個 job 都有各自的 runner，不會受影響，所以結案。
