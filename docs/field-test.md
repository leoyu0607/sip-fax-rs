# 現場試接指南（v0.2.0-alpha）

這一版的目的有兩個：**實際對接中華電信 CS210 收傳真**，以及**取得 CS210 的真實擷取資料**。

0.2.0 新增的功能（M3）：
- **資料庫紀錄**：預設 SQLite（`data/sip-faxd.db`，內建，不需要另外安裝）。每通電話一筆：進線號碼（ANI）、撥打號碼（DNIS）、掛線方向與原因、狀態（接收中／完整／部分／失敗／非傳真）、頁數、檔案大小。設定在 `[database]`、`[retention]`。
- **號碼來源**：取自哪個 header 由每個 trunk 的 `[trunks.numbers]` 決定（預設 `ani_sources = ["pai","rpid","from"]`、`dnis_sources = ["ruri","to"]`，這是 AudioCodes 的實測）。**CS210 在哪個 header 帶號碼還不知道**，請回傳 `sip.log`，我們會據此調整 `cht-cs210` 預設。
- **投遞**（選用）：在 `[[routes]]` 設定資料夾、Email 或 Webhook，範例見 `conf/sip-faxd.toml.example`。沒有設定路由時，傳真只存檔與寫入資料庫。
- **清整**：預設檔案保留 90 天、紀錄保留 365 天，每天 03:17 執行。可以先用 `./sip-faxd purge --dry-run` 看會清掉多少。
- 資料庫連不上不影響收傳真，之後可用 `./sip-faxd db backfill` 補回。
- `./sip-faxd config check` 會顯示上述所有設定。
- ECM 已支援但預設關閉（`[trunks.fax] ecm = false`），真機驗證後才會重新評估。這次試接**請維持關閉**。

這一版還沒有 Web 管理介面（M4）。

## 1. 安裝（RHEL 9/10，不需要安裝任何套件）
```sh
tar xzf sip-fax-rs-0.2.0-linux-x86_64.tar.gz
cd sip-fax-rs-0.2.0-linux-x86_64
cp conf/sip-faxd.toml.example conf/sip-faxd.toml
chown -R <執行服務的帳號>: .     # 解壓後的擁有者是 root，改成實際要執行服務的帳號
```

## 2. 設定 `conf/sip-faxd.toml`（最少要改的項目）
```toml
[sip]
bind = "0.0.0.0:5060"
advertised = "<本機對 CS210 的 IP>:5060"   # 會寫進 Via、Contact 和 SDP，一定要填對

[media]
port_range = "30000-30999"                  # T.38（UDPTL）使用的埠範圍

[[trunks]]
name = "cht"
preset = "cht-cs210"
peer = "<CS210 的 IP>:5060"
# allowed_sources = ["<CS210 的 IP>"]       # 預設就是 peer 的 IP；CS210 若有多個來源 IP，請列在這裡
```
- 如果 CS210 要求註冊，請開啟 `[trunks.register]`，並填入帳號和密碼。
- **第一次試接時建議**把 `[sip] unknown_source` 改成 `"log"`。這樣即使 CS210 實際發話的 IP 和預期不同，電話也會被接起來，並在 log 裡記錄對方的實際 IP。確認 IP 之後再改回 `"drop"`。

檢查設定：
```sh
./sip-faxd config check
```

## 3. 防火牆
- SIP：UDP 5060，只允許 CS210 的 IP 連入。
- T.38：UDP 30000–30999，也就是 `port_range` 設定的範圍。

```sh
firewall-cmd --permanent --add-rich-rule='rule family=ipv4 source address=<CS210 IP> port port=5060 protocol=udp accept'
firewall-cmd --permanent --add-rich-rule='rule family=ipv4 source address=<CS210 IP> port port=30000-30999 protocol=udp accept'
firewall-cmd --reload
```
5060 是特權埠。非 root 執行時需要先給執行檔授權：
```sh
setcap 'cap_net_bind_service=+ep' ./sip-faxd
```

## 4. 啟動
```sh
./sip-faxd start        # 在背景啟動，等服務就緒後顯示 log；Ctrl+C 只停止顯示，服務繼續執行
./sip-faxd status       # 是否在執行、pid、監聽位址、進行中的通話數（沒有在執行時回傳 3）
./sip-faxd logs -f      # 之後要再看 log：logs/sip-faxd.log（自動輪替，舊檔以 gzip 壓縮）
./sip-faxd stop         # 等進行中的傳真收完再停止（最多 300 秒，期間新的來電回 503）
./sip-faxd stop --force # 要馬上停止時：立刻結束通話，已收到的頁面照樣寫出（partial）
```
- 長時間測試直接用 `start` 即可，不需要 `nohup` 或 `tmux`。`restart` 等於 `stop` 再 `start`。
- 要看更詳細的 SIP log：`RUST_LOG=info,sip_core=debug ./sip-faxd start`（環境變數會傳給背景程序）。
- **用 systemd 管理時**：unit 的 `ExecStart` 使用前景的 `serve`（`ExecStart=/opt/sip-faxd/sip-faxd serve`），並設定 `TimeoutStopSec=330`（要大於 `[service] stop_timeout_secs`，預設 300 秒，否則 systemd 會在傳真收完前強制終止）。停止請用 `systemctl stop sip-faxd`（等同 `stop`：新來電回 503、等通話結束）。這時 `sip-faxd start`／`stop`／`restart` 會提示改用 `systemctl`；`sip-faxd status` 與 `logs` 照常可用。
- 要在前景除錯：`./sip-faxd serve`，log 同時輸出到 stderr 與 `logs/sip-faxd.log`，Ctrl+C 與 `stop` 相同（再按一次 Ctrl+C 就是強制結束）。

## 5. 結果在哪裡
| 路徑 | 內容 |
|---|---|
| `data/faxes/YYYY/MM/DD/<call-id>.tif`、`.pdf` | 收到的傳真 |
| `logs/calls/<call-id>/result.json` | 結果：頁數、速率、對方 TSI、成功或失敗原因 |
| `logs/calls/<call-id>/sip.log` | 這通電話的所有 SIP 訊息原文 |
| `logs/calls/<call-id>/udptl.log` | T.38 封包摘要，含完整的 T.30 控制 frame，但不含影像內容 |
| `data/sip-faxd.db` | 每通電話一筆紀錄；不需要回傳，`logs/calls/` 的內容已足夠 |

## 6. 試接後請回傳（不論成功或失敗）
1. 整個 `logs/calls/` 目錄（壓縮後傳回）。裡面不含傳真影像，但含有雙方的電話號碼和 IP。
2. 終端機的 log 輸出。
3. 傳真是否成功、頁數是否正確、影像是否完整。可以的話，附上發送端的原稿供比對。
4. 如果完全沒有接到電話：附上 `tcpdump -i any -w cs210.pcap udp port 5060` 的擷取。

## 7. 已知限制（alpha）
- 只支援 T.38。如果 CS210 拒絕 T.38、要求 G.711，這一版會掛斷，並在 `result.json` 記錄失敗原因。G.711 passthrough 在 M5 才會做。
- ECM 預設關閉。發送端要求 ECM 時，傳送會以 non-ECM 完成。ECM 只和 SpanDSP 互通驗證過，還沒有和真實 gateway 測過。
- 沒有 Web 管理介面；紀錄可以直接用 SQLite 工具查詢 `data/sip-faxd.db`（欄位說明見壓縮檔內的 `docs/database.md`）。
- localTCF 時不會送 CFR。這是跟 SpanDSP 一致的行為，但還沒有用 CS210 驗證過，這正是這次試接要確認的項目之一。
