package phx;

import android.app.Activity;
import android.database.Cursor;
import android.database.sqlite.SQLiteDatabase;
import android.util.Base64;
import java.io.File;
import java.nio.ByteBuffer;
import java.nio.charset.CharacterCodingException;
import java.nio.charset.CodingErrorAction;
import java.nio.charset.StandardCharsets;
import java.util.HashMap;

/**
 * Read-only access to Phoenix's own SQLite databases (its persisted stores). Results are JSON
 * strings: rows as arrays; TEXT and UTF-8 BLOBs as strings, other BLOBs as {"$b64": "..."}.
 */
public final class Db {
    private Db() {}

    private static final HashMap<String, SQLiteDatabase> open = new HashMap<>();

    /** The app's data directory (to find the database files). */
    public static String dataDir(Object owner) throws IllegalAccessException {
        Activity a = Picker.findActivity(owner);
        return a == null ? null : a.getApplicationInfo().dataDir;
    }

    /** Files under `dir` (depth <= 4) as JSON [[path, size], ...], max 400. */
    public static String files(String dir) {
        StringBuilder sb = new StringBuilder("[");
        int[] n = {0};
        walk(new File(dir), 0, sb, n);
        return sb.append(']').toString();
    }

    private static void walk(File f, int depth, StringBuilder sb, int[] n) {
        File[] kids = f.listFiles();
        if (kids == null) return;
        for (File k : kids) {
            if (n[0] >= 400) return;
            if (k.isDirectory()) { if (depth < 4) walk(k, depth + 1, sb, n); continue; }
            if (n[0]++ > 0) sb.append(',');
            sb.append('[').append(quote(k.getPath())).append(',').append(k.length()).append(']');
        }
    }

    private static synchronized SQLiteDatabase db(String path) {
        SQLiteDatabase d = open.get(path);
        if (d == null || !d.isOpen()) {
            d = SQLiteDatabase.openDatabase(path, null, SQLiteDatabase.OPEN_READONLY | SQLiteDatabase.NO_LOCALIZED_COLLATORS);
            open.put(path, d);
        }
        return d;
    }

    /** Runs a read-only query; returns {"cols":[...],"rows":[[...],...]} or {"error": "..."}. */
    public static String query(String path, String sql, String[] args, int limit) {
        StringBuilder sb = new StringBuilder();
        Cursor c = null;
        try {
            c = db(path).rawQuery(sql, args);
            int nc = c.getColumnCount();
            sb.append("{\"cols\":[");
            for (int i = 0; i < nc; i++) { if (i > 0) sb.append(','); sb.append(quote(c.getColumnName(i))); }
            sb.append("],\"rows\":[");
            int rows = 0;
            while (c.moveToNext() && rows < limit) {
                if (rows++ > 0) sb.append(',');
                sb.append('[');
                for (int i = 0; i < nc; i++) {
                    if (i > 0) sb.append(',');
                    switch (c.getType(i)) {
                        case Cursor.FIELD_TYPE_NULL: sb.append("null"); break;
                        case Cursor.FIELD_TYPE_INTEGER: sb.append(c.getLong(i)); break;
                        case Cursor.FIELD_TYPE_FLOAT: sb.append(c.getDouble(i)); break;
                        case Cursor.FIELD_TYPE_STRING: sb.append(quote(c.getString(i))); break;
                        default: blob(sb, c.getBlob(i));
                    }
                }
                sb.append(']');
            }
            sb.append("],\"more\":").append(!c.isAfterLast() && rows >= limit).append('}');
            return sb.toString();
        } catch (Throwable t) {
            return "{\"error\":" + quote(String.valueOf(t)) + "}";
        } finally {
            if (c != null) c.close();
        }
    }

    private static void blob(StringBuilder sb, byte[] b) {
        try {
            String s = StandardCharsets.UTF_8.newDecoder().onMalformedInput(CodingErrorAction.REPORT)
                .onUnmappableCharacter(CodingErrorAction.REPORT).decode(ByteBuffer.wrap(b)).toString();
            sb.append(quote(s));
        } catch (CharacterCodingException e) {
            sb.append("{\"$b64\":\"").append(Base64.encodeToString(b, Base64.NO_WRAP)).append("\"}");
        }
    }

    static String quote(String s) {
        StringBuilder sb = new StringBuilder(s.length() + 2).append('"');
        for (int i = 0; i < s.length(); i++) {
            char ch = s.charAt(i);
            switch (ch) {
                case '"': sb.append("\\\""); break;
                case '\\': sb.append("\\\\"); break;
                case '\n': sb.append("\\n"); break;
                case '\r': sb.append("\\r"); break;
                case '\t': sb.append("\\t"); break;
                default:
                    if (ch < 0x20) sb.append(String.format("\\u%04x", (int) ch)); else sb.append(ch);
            }
        }
        return sb.append('"').toString();
    }
}
