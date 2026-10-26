# 串接範例

從其他系統讀取 sip-faxd 收到的傳真。做法與注意事項見 [`docs/integration.md`](../docs/integration.md)。

| 目錄 | 內容 | 需要 |
|---|---|---|
| [`java/`](java/) | `FaxClient.java`（API：增量查詢、下載 PDF 與縮圖）、`FaxDbReader.java`（讀 view `fax_records`） | JDK 17 以上；讀 DB 時加資料庫的 JDBC driver |
| [`node/`](node/) | `fax-client.mjs`（API，與 Java 版相同） | Node.js 18 以上 |

兩個 API 範例的行為相同：

1. 呼叫 `GET /api/v1/faxes/changes`，帶上次存下的 `next`；
2. 印出每一份新進或有變動的傳真；接收完成、有 PDF 而且還沒下載過的，下載 `<id>.pdf` 與第 1 頁縮圖 `<id>-p1.png` 到 `FAX_OUT_DIR`；
3. 處理完才把 `next` 存到 `FAX_CURSOR_FILE`（當機重來只會重複，不會漏）；
4. 追上之後每 10 秒再問一次；加 `--once` 則追上就結束（適合排程執行）。

需要的環境變數：

| 變數 | 說明 |
|---|---|
| `FAX_BASE_URL` | 例如 `https://fax.example.com:8443/api/v1` |
| `FAX_API_KEY` | 管理員建立的 API 金鑰（`sfx_…`），唯讀即可 |
| `FAX_OUT_DIR` | 下載目錄，預設 `./faxes` |
| `FAX_CURSOR_FILE` | 保存位置的檔案，預設 `./fax-cursor.txt` |

sip-faxd 使用自簽憑證時，要先讓用戶端信任它（見串接指南第 3 節「HTTPS 憑證」）：

```sh
# Java
java -Djavax.net.ssl.trustStore=fax-truststore.p12 -Djavax.net.ssl.trustStorePassword=changeit \
     java/FaxClient.java --once
# Node.js
NODE_EXTRA_CA_CERTS=sip-faxd.crt node node/fax-client.mjs --once
```

讀資料庫的範例：

```sh
FAX_DB_URL=jdbc:postgresql://db.example.com/sipfax FAX_DB_USER=fax_reader FAX_DB_PASSWORD=... \
java -cp postgresql-42.7.4.jar java/FaxDbReader.java
```

這些範例刻意不依賴任何套件，方便直接執行與閱讀；正式的程式可以改用自己慣用的 HTTP 與 JSON 函式庫，或以 `docs/openapi.yaml` 產生用戶端。
