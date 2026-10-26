// sip-faxd database example (JDK 17+ and the JDBC driver of your database): read the new and
// changed faxes from the view fax_records with a read-only account, remember the position.
//
//   FAX_DB_URL=jdbc:postgresql://db.example.com/sipfax FAX_DB_USER=fax_reader FAX_DB_PASSWORD=... \
//   java -cp postgresql-42.7.4.jar FaxDbReader.java
//
// FAX_DB_URL picks the dialect: jdbc:postgresql:, jdbc:mysql: / jdbc:mariadb:, jdbc:sqlserver:.
// For MySQL / MariaDB add connectionTimeZone=UTC to the URL: the times are UTC (DATETIME has no
// zone). FAX_DB_CURSOR_FILE (default ./fax-db-cursor.txt) keeps the position. Files and
// thumbnails are not in the database: get them over the API (FaxClient.java).
// See docs/integration.md section 5.

import java.nio.file.Files;
import java.nio.file.Path;
import java.sql.Connection;
import java.sql.DriverManager;
import java.sql.PreparedStatement;
import java.sql.ResultSet;
import java.sql.Timestamp;

public class FaxDbReader {
    public static void main(String[] args) throws Exception {
        String url = required("FAX_DB_URL");
        Path cursorFile = Path.of(System.getenv().getOrDefault("FAX_DB_CURSOR_FILE", "fax-db-cursor.txt"));
        // the position: updated_at and id of the last row handled
        Timestamp lastUpdated = Timestamp.valueOf("1970-01-01 00:00:00");
        long lastId = 0;
        if (Files.exists(cursorFile)) {
            String[] p = Files.readString(cursorFile).trim().split("\\|");
            lastUpdated = Timestamp.valueOf(p[0]);
            lastId = Long.parseLong(p[1]);
        }
        String sql = sql(url);
        try (Connection db = DriverManager.getConnection(url, required("FAX_DB_USER"), required("FAX_DB_PASSWORD"));
             PreparedStatement q = db.prepareStatement(sql)) {
            while (true) {
                q.setTimestamp(1, lastUpdated);
                q.setTimestamp(2, lastUpdated);
                q.setLong(3, lastId);
                int n = 0;
                try (ResultSet r = q.executeQuery()) {
                    while (r.next()) {
                        n++;
                        System.out.printf("fax %d: %s, %d pages, from %s to %s, read %s%n", r.getLong("id"),
                                r.getString("fax_status"), r.getInt("pages"), r.getString("ani"),
                                r.getString("dnis"), r.getBoolean("is_read"));
                        lastUpdated = r.getTimestamp("updated_at");
                        lastId = r.getLong("id");
                    }
                }
                Files.writeString(cursorFile, lastUpdated + "|" + lastId);
                if (n < 100) {
                    break;
                }
            }
        }
    }

    /** The change query of the dialect of `url` (5 seconds of settling time, see the guide). */
    static String sql(String url) {
        String after = "(updated_at > ? OR (updated_at = ? AND id > ?))";
        if (url.startsWith("jdbc:postgresql:")) {
            return "SELECT * FROM fax_records WHERE " + after
                    + " AND updated_at <= now() - interval '5 seconds' ORDER BY updated_at, id LIMIT 100";
        }
        if (url.startsWith("jdbc:mysql:") || url.startsWith("jdbc:mariadb:")) {
            return "SELECT * FROM fax_records WHERE " + after
                    + " AND updated_at <= UTC_TIMESTAMP(3) - INTERVAL 5 SECOND ORDER BY updated_at, id LIMIT 100";
        }
        if (url.startsWith("jdbc:sqlserver:")) {
            return "SELECT TOP (100) * FROM fax_records WHERE " + after
                    + " AND updated_at <= DATEADD(second, -5, SYSUTCDATETIME()) ORDER BY updated_at, id";
        }
        throw new IllegalArgumentException("FAX_DB_URL: PostgreSQL, MySQL / MariaDB or SQL Server");
    }

    static String required(String name) {
        String v = System.getenv(name);
        if (v == null || v.isBlank()) {
            System.err.println("set " + name);
            System.exit(2);
        }
        return v;
    }
}
