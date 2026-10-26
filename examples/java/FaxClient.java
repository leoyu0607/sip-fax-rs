// sip-faxd API example (JDK 17+, no libraries): follow the change feed, download the PDF and the
// thumbnail of the first page of every fax that finished, remember where it got to.
//
//   FAX_BASE_URL=https://fax.example.com:8443/api/v1 FAX_API_KEY=sfx_... \
//   java -Djavax.net.ssl.trustStore=fax-truststore.p12 -Djavax.net.ssl.trustStorePassword=changeit \
//        FaxClient.java [--once]
//
// FAX_OUT_DIR (default ./faxes) receives <id>.pdf and <id>-p1.png; FAX_CURSOR_FILE (default
// ./fax-cursor.txt) keeps the position, so a restart continues where it stopped. With --once it
// stops when it has caught up, otherwise it asks again every 10 seconds.
// See docs/integration.md.

import java.io.IOException;
import java.net.URI;
import java.net.URLEncoder;
import java.net.http.HttpClient;
import java.net.http.HttpRequest;
import java.net.http.HttpResponse;
import java.nio.charset.StandardCharsets;
import java.nio.file.Files;
import java.nio.file.Path;
import java.nio.file.StandardCopyOption;
import java.time.Duration;
import java.util.ArrayList;
import java.util.Arrays;
import java.util.LinkedHashMap;
import java.util.List;
import java.util.Map;

public class FaxClient {
    static final int LIMIT = 100;

    final HttpClient http = HttpClient.newBuilder().connectTimeout(Duration.ofSeconds(10)).build();
    final String base;
    final String key;
    final Path outDir;

    FaxClient(String base, String key, Path outDir) {
        this.base = base.replaceAll("/+$", "");
        this.key = key;
        this.outDir = outDir;
    }

    public static void main(String[] args) throws Exception {
        String base = required("FAX_BASE_URL");
        String key = required("FAX_API_KEY");
        Path outDir = Path.of(System.getenv().getOrDefault("FAX_OUT_DIR", "faxes"));
        Path cursorFile = Path.of(System.getenv().getOrDefault("FAX_CURSOR_FILE", "fax-cursor.txt"));
        boolean once = Arrays.asList(args).contains("--once");
        Files.createDirectories(outDir);
        FaxClient c = new FaxClient(base, key, outDir);

        String cursor = Files.exists(cursorFile) ? Files.readString(cursorFile).trim() : "";
        while (true) {
            String url = c.base + "/faxes/changes?limit=" + LIMIT
                    + (cursor.isEmpty() ? "" : "&after=" + URLEncoder.encode(cursor, StandardCharsets.UTF_8));
            Map<String, Object> page = Json.object(c.getText(url));
            List<Object> items = Json.array(page.get("items"));
            for (Object o : items) {
                c.handle(Json.object(o));
            }
            // saved after the faxes are handled: a crash repeats some, it never skips one
            cursor = (String) page.get("next");
            Files.writeString(cursorFile, cursor);
            if (items.size() < LIMIT) {
                if (once) {
                    break;
                }
                Thread.sleep(10_000);
            }
        }
    }

    /** One new or changed fax: print it; once it is finished, fetch its files (once). */
    void handle(Map<String, Object> fax) throws IOException, InterruptedException {
        long id = ((Number) fax.get("id")).longValue();
        String status = (String) fax.get("fax_status");
        System.out.printf("fax %d: %s, %s pages, from %s to %s, started %s%n", id, status, fax.get("pages"),
                fax.get("ani"), fax.get("dnis"), fax.get("started_at"));
        if (status.equals("receiving")) {
            return; // it comes again when the call ends
        }
        Path pdf = outDir.resolve(id + ".pdf");
        if (Boolean.TRUE.equals(fax.get("has_pdf")) && !Files.exists(pdf)) {
            download(base + "/faxes/" + id + "/file.pdf", pdf);
            if (((Number) fax.get("pages")).longValue() > 0) {
                download(base + "/faxes/" + id + "/pages/1.png?kind=thumb", outDir.resolve(id + "-p1.png"));
            }
            System.out.printf("  saved %s%n", pdf);
        }
    }

    HttpRequest.Builder request(String url) {
        return HttpRequest.newBuilder(URI.create(url))
                .timeout(Duration.ofSeconds(60))
                .header("Authorization", "Bearer " + key);
    }

    String getText(String url) throws IOException, InterruptedException {
        HttpResponse<String> r = http.send(request(url).GET().build(), HttpResponse.BodyHandlers.ofString());
        check(r.statusCode(), r.body());
        return r.body();
    }

    /** Into a temporary file first: a broken download never leaves a half file behind. */
    void download(String url, Path to) throws IOException, InterruptedException {
        Path tmp = to.resolveSibling(to.getFileName() + ".part");
        HttpResponse<Path> r = http.send(request(url).GET().build(), HttpResponse.BodyHandlers.ofFile(tmp));
        if (r.statusCode() != 200) {
            String body = Files.readString(tmp);
            Files.deleteIfExists(tmp);
            check(r.statusCode(), body);
        }
        Files.move(tmp, to, StandardCopyOption.REPLACE_EXISTING, StandardCopyOption.ATOMIC_MOVE);
    }

    /** API errors are {"error": {"code", "message"}}; decide by the code. */
    static void check(int status, String body) throws IOException {
        if (status == 200) {
            return;
        }
        String code = "?";
        try {
            code = (String) Json.object(Json.object(body).get("error")).get("code");
        } catch (RuntimeException ignored) {
            // not the API's JSON (a proxy in between, for example)
        }
        if (code.equals("invalid_api_key")) {
            System.err.println("the API key is not valid (revoked or expired?): ask the administrator");
            System.exit(2);
        }
        throw new IOException("HTTP " + status + " " + code + ": " + body);
    }

    static String required(String name) {
        String v = System.getenv(name);
        if (v == null || v.isBlank()) {
            System.err.println("set " + name);
            System.exit(2);
        }
        return v;
    }

    /** A small JSON reader (objects, arrays, strings, numbers, true, false, null): enough here. */
    static final class Json {
        private final String s;
        private int i;

        private Json(String s) {
            this.s = s;
        }

        static Object parse(String text) {
            Json j = new Json(text);
            Object v = j.value();
            j.space();
            if (j.i != j.s.length()) {
                throw j.error("trailing characters");
            }
            return v;
        }

        @SuppressWarnings("unchecked")
        static Map<String, Object> object(Object o) {
            return (Map<String, Object>) (o instanceof String t ? parse(t) : o);
        }

        @SuppressWarnings("unchecked")
        static List<Object> array(Object o) {
            return (List<Object>) o;
        }

        private RuntimeException error(String what) {
            return new IllegalArgumentException("JSON: " + what + " at " + i);
        }

        private void space() {
            while (i < s.length() && Character.isWhitespace(s.charAt(i))) {
                i++;
            }
        }

        private Object value() {
            space();
            if (i >= s.length()) {
                throw error("unexpected end");
            }
            char c = s.charAt(i);
            switch (c) {
                case '{': {
                    i++;
                    Map<String, Object> m = new LinkedHashMap<>();
                    space();
                    if (s.charAt(i) == '}') {
                        i++;
                        return m;
                    }
                    while (true) {
                        space();
                        String k = string();
                        space();
                        expect(':');
                        m.put(k, value());
                        space();
                        if (s.charAt(i) == ',') {
                            i++;
                        } else {
                            expect('}');
                            return m;
                        }
                    }
                }
                case '[': {
                    i++;
                    List<Object> a = new ArrayList<>();
                    space();
                    if (s.charAt(i) == ']') {
                        i++;
                        return a;
                    }
                    while (true) {
                        a.add(value());
                        space();
                        if (s.charAt(i) == ',') {
                            i++;
                        } else {
                            expect(']');
                            return a;
                        }
                    }
                }
                case '"':
                    return string();
                case 't':
                    return literal("true", Boolean.TRUE);
                case 'f':
                    return literal("false", Boolean.FALSE);
                case 'n':
                    return literal("null", null);
                default:
                    return number();
            }
        }

        private void expect(char c) {
            if (i >= s.length() || s.charAt(i) != c) {
                throw error("expected " + c);
            }
            i++;
        }

        private Object literal(String word, Object v) {
            if (!s.startsWith(word, i)) {
                throw error("bad literal");
            }
            i += word.length();
            return v;
        }

        private Number number() {
            int start = i;
            while (i < s.length() && "+-0123456789.eE".indexOf(s.charAt(i)) >= 0) {
                i++;
            }
            String n = s.substring(start, i);
            if (n.isEmpty()) {
                throw error("unexpected character");
            }
            return n.matches("-?\\d+") ? (Number) Long.valueOf(n) : (Number) Double.valueOf(n);
        }

        private String string() {
            expect('"');
            StringBuilder b = new StringBuilder();
            while (true) {
                if (i >= s.length()) {
                    throw error("unterminated string");
                }
                char c = s.charAt(i++);
                if (c == '"') {
                    return b.toString();
                }
                if (c != '\\') {
                    b.append(c);
                    continue;
                }
                char e = s.charAt(i++);
                switch (e) {
                    case 'n' -> b.append('\n');
                    case 't' -> b.append('\t');
                    case 'r' -> b.append('\r');
                    case 'b' -> b.append('\b');
                    case 'f' -> b.append('\f');
                    case 'u' -> {
                        b.append((char) Integer.parseInt(s.substring(i, i + 4), 16));
                        i += 4;
                    }
                    default -> b.append(e); // \" \\ \/
                }
            }
        }
    }
}
