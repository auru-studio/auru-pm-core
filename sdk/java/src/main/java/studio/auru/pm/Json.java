package studio.auru.pm;

import java.util.ArrayList;
import java.util.Collections;
import java.util.LinkedHashMap;
import java.util.List;
import java.util.Map;
import java.util.Optional;
import java.util.TreeMap;

/**
 * A JSON value, and the canonical encoding a commit id is derived from.
 *
 * <p>Written here rather than taken from a dependency for two reasons. A client library that pulls
 * in a JSON stack causes version conflicts in somebody else's application. More importantly,
 * canonical encoding needs exact control over the output: a third-party serializer's formatting
 * choices would become this library's bug, and it would surface as a provider rejecting a commit
 * rather than as anything that names the cause.
 *
 * <p>Integers and non-integers are separate cases ({@link Int} and {@link Dec}) because RFC 8785
 * treats them differently and only one of them can be canonicalized here — see
 * {@link #toCanonicalJson()}.
 */
public sealed interface Json {

    /** An object. Member order is not significant; canonical output sorts them. */
    record Obj(Map<String, Json> members) implements Json {
        public Obj {
            members = Collections.unmodifiableMap(new LinkedHashMap<>(members));
        }
    }

    /** An array. Order is significant. */
    record Arr(List<Json> elements) implements Json {
        public Arr {
            elements = List.copyOf(elements);
        }
    }

    record Str(String value) implements Json {}

    /** A number with no fractional part, within ±(2^53 − 1). */
    record Int(long value) implements Json {}

    /**
     * A number with a fractional part or an exponent.
     *
     * <p>Parsed and readable, but not canonicalizable — see {@link #toCanonicalJson()}.
     */
    record Dec(double value) implements Json {}

    record Bool(boolean value) implements Json {}

    /** JSON {@code null}. */
    record Null() implements Json {}

    Null NULL = new Null();
    Bool TRUE = new Bool(true);
    Bool FALSE = new Bool(false);

    /**
     * The largest integer RFC 8785 can represent exactly.
     *
     * <p>Its numbers are IEEE-754 binary64, so beyond this two conformant implementations derive
     * different ids from the same commit.
     */
    long MAX_SAFE_INTEGER = 9_007_199_254_740_991L;

    // ── Building ─────────────────────────────────────────────────────────────

    static Json of(String value) {
        return value == null ? NULL : new Str(value);
    }

    static Json of(long value) {
        return new Int(value);
    }

    static Json of(boolean value) {
        return value ? TRUE : FALSE;
    }

    /** An object builder that keeps insertion order for readability. */
    static ObjectBuilder object() {
        return new ObjectBuilder();
    }

    /** Fluent construction; canonical output sorts members regardless of the order used here. */
    final class ObjectBuilder {
        private final Map<String, Json> members = new LinkedHashMap<>();

        private ObjectBuilder() {}

        /** Add a member. A {@code null} value is omitted rather than written as JSON null. */
        public ObjectBuilder putIfPresent(String name, Json value) {
            if (value != null && !(value instanceof Null)) {
                members.put(name, value);
            }
            return this;
        }

        public ObjectBuilder put(String name, Json value) {
            members.put(name, value == null ? NULL : value);
            return this;
        }

        public ObjectBuilder put(String name, String value) {
            return put(name, Json.of(value));
        }

        public ObjectBuilder put(String name, long value) {
            return put(name, Json.of(value));
        }

        public Obj build() {
            return new Obj(members);
        }
    }

    static Arr array(List<Json> elements) {
        return new Arr(elements);
    }

    // ── Reading ──────────────────────────────────────────────────────────────

    /** This value's members, or empty when it is not an object. */
    default Map<String, Json> members() {
        return this instanceof Obj object ? object.members() : Map.of();
    }

    /** A member by name, absent when missing, JSON null, or when this is not an object. */
    default Optional<Json> get(String name) {
        Json value = members().get(name);
        return value == null || value instanceof Null ? Optional.empty() : Optional.of(value);
    }

    /** A required member. */
    default Json require(String name) {
        return get(name)
                .orElseThrow(
                        () -> new IllegalArgumentException("missing required member \"" + name + "\""));
    }

    default Optional<String> optString(String name) {
        return get(name).map(Json::asString);
    }

    default String string(String name) {
        return asString(require(name));
    }

    default long integer(String name) {
        Json value = require(name);
        if (value instanceof Int number) {
            return number.value();
        }
        if (value instanceof Dec number) {
            throw new IllegalArgumentException(
                    "\"" + name + "\" must be an integer, got " + number.value());
        }
        throw new IllegalArgumentException("\"" + name + "\" is not a number");
    }

    default boolean bool(String name, boolean fallback) {
        return get(name).orElse(NULL) instanceof Bool flag ? flag.value() : fallback;
    }

    /**
     * This value as text, empty when it is not a string.
     *
     * <p>Records carry an implicit {@code toString}, so printing a {@link Str} directly yields
     * {@code Str[value=...]}. This is the accessor to reach for instead.
     */
    default Optional<String> asText() {
        return this instanceof Str text ? Optional.of(text.value()) : Optional.empty();
    }

    /** This value as a whole number, empty when it is not one. */
    default Optional<Long> asInteger() {
        return this instanceof Int number ? Optional.of(number.value()) : Optional.empty();
    }

    /** This value as a number, empty when it is not one. Integers widen. */
    default Optional<Double> asNumber() {
        if (this instanceof Int number) {
            return Optional.of((double) number.value());
        }
        return this instanceof Dec number ? Optional.of(number.value()) : Optional.empty();
    }

    /** Elements, or empty when this is not an array. */
    default List<Json> elements() {
        return this instanceof Arr array ? array.elements() : List.of();
    }

    private static String asString(Json value) {
        if (value instanceof Str text) {
            return text.value();
        }
        throw new IllegalArgumentException("expected a string, got " + value.getClass().getSimpleName());
    }

    // ── Canonical encoding ───────────────────────────────────────────────────

    /**
     * RFC 8785 canonical JSON — the bytes a commit id is the BLAKE3 of.
     *
     * <p>Object members are sorted by UTF-16 code unit, which is exactly what Java's natural string
     * ordering already does, so a {@link TreeMap} is the rule rather than an approximation of it.
     *
     * @throws IllegalStateException if the value contains a non-integral number, or an integer
     *     outside ±(2^53 − 1). RFC 8785 defines numbers by ECMAScript's {@code Number::toString},
     *     which is IEEE-754 binary64; producing approximate bytes would mean producing a commit id
     *     no other implementation agrees with, so this fails loudly instead. Nothing in a commit is
     *     fractional, and every integer in one is far inside the bound.
     */
    default String toCanonicalJson() {
        StringBuilder out = new StringBuilder();
        write(this, out, true);
        return out.toString();
    }

    /**
     * Ordinary JSON text, for a request body.
     *
     * <p>Distinct from {@link #toCanonicalJson()}: this keeps insertion order, which reads better in
     * a log, and it can write a fractional number, which canonical encoding deliberately refuses.
     * Nothing derives a hash from these bytes.
     *
     * <p>Not {@code toString()}, because these are records and their implicit {@code toString} would
     * shadow any override — producing {@code Obj[members={...}]} on the wire.
     */
    default String toJsonText() {
        StringBuilder out = new StringBuilder();
        write(this, out, false);
        return out.toString();
    }

    private static void write(Json value, StringBuilder out, boolean canonical) {
        if (value instanceof Null) {
            out.append("null");
        } else if (value instanceof Bool flag) {
            out.append(flag.value() ? "true" : "false");
        } else if (value instanceof Str text) {
            writeString(text.value(), out);
        } else if (value instanceof Int number) {
            if (canonical && Math.abs(number.value()) > MAX_SAFE_INTEGER) {
                throw new IllegalStateException(
                        "integer "
                                + number.value()
                                + " is outside +/-(2^53 - 1); RFC 8785 numbers are IEEE-754"
                                + " binary64, so no two implementations would agree on it");
            }
            out.append(number.value());
        } else if (value instanceof Dec number) {
            if (canonical) {
                throw new IllegalStateException(
                        "cannot canonicalize the non-integral number "
                                + number.value()
                                + "; RFC 8785 requires ECMAScript number formatting, which this"
                                + " library implements only for integers because nothing in a commit"
                                + " is fractional");
            }
            out.append(number.value());
        } else if (value instanceof Arr array) {
            out.append('[');
            boolean first = true;
            for (Json element : array.elements()) {
                if (!first) {
                    out.append(',');
                }
                first = false;
                write(element, out, canonical);
            }
            out.append(']');
        } else if (value instanceof Obj object) {
            out.append('{');
            boolean first = true;
            // Canonical output sorts members. Natural String ordering compares
            // UTF-16 code units, which is precisely the rule RFC 8785 states,
            // so a TreeMap is that rule rather than an approximation of it.
            Map<String, Json> members =
                    canonical ? new TreeMap<>(object.members()) : object.members();
            for (Map.Entry<String, Json> member : members.entrySet()) {
                if (!first) {
                    out.append(',');
                }
                first = false;
                writeString(member.getKey(), out);
                out.append(':');
                write(member.getValue(), out, canonical);
            }
            out.append('}');
        } else {
            throw new IllegalStateException("unreachable: " + value.getClass());
        }
    }

    /**
     * Escape a string the way ECMAScript's {@code JSON.stringify} does.
     *
     * <p>Only the characters that must be escaped are: no escaping of {@code /}, and none of
     * non-ASCII, which travels as UTF-8. Escaping more would still be valid JSON and still be the
     * wrong bytes.
     */
    private static void writeString(String value, StringBuilder out) {
        out.append('"');
        for (int index = 0; index < value.length(); index++) {
            char character = value.charAt(index);
            switch (character) {
                case '"' -> out.append("\\\"");
                case '\\' -> out.append("\\\\");
                case '\b' -> out.append("\\b");
                case '\f' -> out.append("\\f");
                case '\n' -> out.append("\\n");
                case '\r' -> out.append("\\r");
                case '\t' -> out.append("\\t");
                default -> {
                    if (character < 0x20) {
                        out.append(String.format("\\u%04x", (int) character));
                    } else {
                        out.append(character);
                    }
                }
            }
        }
        out.append('"');
    }

    // ── Parsing ──────────────────────────────────────────────────────────────

    /**
     * Parse JSON text.
     *
     * @throws IllegalArgumentException on anything that is not one well-formed JSON value.
     */
    static Json parse(String text) {
        Parser parser = new Parser(text);
        Json value = parser.readValue();
        parser.skipWhitespace();
        if (!parser.atEnd()) {
            throw parser.error("trailing content after the JSON value");
        }
        return value;
    }

    /** A recursive-descent parser. Small because JSON is small. */
    final class Parser {
        private final String text;
        private int position;

        private Parser(String text) {
            this.text = text;
        }

        private IllegalArgumentException error(String message) {
            return new IllegalArgumentException("JSON at offset " + position + ": " + message);
        }

        private boolean atEnd() {
            return position >= text.length();
        }

        private void skipWhitespace() {
            while (!atEnd()) {
                char character = text.charAt(position);
                if (character == ' ' || character == '\t' || character == '\n' || character == '\r') {
                    position++;
                } else {
                    return;
                }
            }
        }

        private char peek() {
            if (atEnd()) {
                throw error("unexpected end of input");
            }
            return text.charAt(position);
        }

        private void expect(char expected) {
            if (peek() != expected) {
                throw error("expected '" + expected + "' but found '" + peek() + "'");
            }
            position++;
        }

        private Json readValue() {
            skipWhitespace();
            switch (peek()) {
                case '{':
                    return readObject();
                case '[':
                    return readArray();
                case '"':
                    return new Str(readString());
                case 't':
                    return readLiteral("true", TRUE);
                case 'f':
                    return readLiteral("false", FALSE);
                case 'n':
                    return readLiteral("null", NULL);
                default:
                    return readNumber();
            }
        }

        private Json readLiteral(String literal, Json value) {
            if (!text.startsWith(literal, position)) {
                throw error("expected " + literal);
            }
            position += literal.length();
            return value;
        }

        private Json readObject() {
            expect('{');
            Map<String, Json> members = new LinkedHashMap<>();
            skipWhitespace();
            if (peek() == '}') {
                position++;
                return new Obj(members);
            }
            while (true) {
                skipWhitespace();
                String name = readString();
                skipWhitespace();
                expect(':');
                members.put(name, readValue());
                skipWhitespace();
                char next = peek();
                position++;
                if (next == '}') {
                    return new Obj(members);
                }
                if (next != ',') {
                    throw error("expected ',' or '}' in object");
                }
            }
        }

        private Json readArray() {
            expect('[');
            List<Json> elements = new ArrayList<>();
            skipWhitespace();
            if (peek() == ']') {
                position++;
                return new Arr(elements);
            }
            while (true) {
                elements.add(readValue());
                skipWhitespace();
                char next = peek();
                position++;
                if (next == ']') {
                    return new Arr(elements);
                }
                if (next != ',') {
                    throw error("expected ',' or ']' in array");
                }
            }
        }

        private String readString() {
            expect('"');
            StringBuilder out = new StringBuilder();
            while (true) {
                char character = peek();
                position++;
                if (character == '"') {
                    return out.toString();
                }
                if (character != '\\') {
                    if (character < 0x20) {
                        throw error("unescaped control character in string");
                    }
                    out.append(character);
                    continue;
                }
                char escape = peek();
                position++;
                switch (escape) {
                    case '"' -> out.append('"');
                    case '\\' -> out.append('\\');
                    case '/' -> out.append('/');
                    case 'b' -> out.append('\b');
                    case 'f' -> out.append('\f');
                    case 'n' -> out.append('\n');
                    case 'r' -> out.append('\r');
                    case 't' -> out.append('\t');
                    case 'u' -> {
                        if (position + 4 > text.length()) {
                            throw error("truncated \\u escape");
                        }
                        out.append((char) Integer.parseInt(text.substring(position, position + 4), 16));
                        position += 4;
                    }
                    default -> throw error("unknown escape \\" + escape);
                }
            }
        }

        private Json readNumber() {
            int start = position;
            if (!atEnd() && text.charAt(position) == '-') {
                position++;
            }
            boolean fractional = false;
            while (!atEnd()) {
                char character = text.charAt(position);
                if (character >= '0' && character <= '9') {
                    position++;
                } else if (character == '.' || character == 'e' || character == 'E'
                        || character == '+' || character == '-') {
                    fractional = true;
                    position++;
                } else {
                    break;
                }
            }
            String literal = text.substring(start, position);
            if (literal.isEmpty() || literal.equals("-")) {
                throw error("expected a value");
            }
            if (!fractional) {
                try {
                    return new Int(Long.parseLong(literal));
                } catch (NumberFormatException beyondLong) {
                    // Falls through to a double, which loses precision — but a
                    // value that large is already outside what this protocol
                    // permits, and canonicalizing it will say so.
                    return new Dec(Double.parseDouble(literal));
                }
            }
            return new Dec(Double.parseDouble(literal));
        }
    }
}
