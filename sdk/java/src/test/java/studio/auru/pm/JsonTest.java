package studio.auru.pm;

import static org.junit.jupiter.api.Assertions.assertEquals;
import static org.junit.jupiter.api.Assertions.assertThrows;
import static org.junit.jupiter.api.Assertions.assertTrue;

import java.util.List;
import org.junit.jupiter.api.Test;

/**
 * The hand-written JSON layer.
 *
 * <p>Everything else in this library rests on it, and the interesting cases are the ones a
 * canonical encoder has to get exactly right rather than merely parse: escapes, control characters,
 * astral-plane text, and the boundary between an integer and a number that is not one.
 */
class JsonTest {

    @Test
    void parsesTheShapesAWireBodyUses() {
        Json value =
                Json.parse(
                        "{\"a\":1,\"b\":\"two\",\"c\":[1,2,3],\"d\":{\"e\":true},\"f\":null,\"g\":false}");
        assertEquals(1, value.integer("a"));
        assertEquals("two", value.string("b"));
        assertEquals(3, value.require("c").elements().size());
        assertTrue(value.require("d").bool("e", false));
        // JSON null reads as absent, so a caller never distinguishes "missing"
        // from "explicitly null" when neither carries information.
        assertTrue(value.get("f").isEmpty());
        assertEquals(false, value.bool("g", true));
    }

    @Test
    void ignoresInsignificantWhitespace() {
        assertEquals(
                Json.parse("{\"a\":1}").toCanonicalJson(),
                Json.parse("  {\n  \"a\" :\t1\r\n}  ").toCanonicalJson());
    }

    @Test
    void roundTripsEveryEscape() {
        String text = "quote \" backslash \\ slash / tab \t newline \n return \r";
        Json value = Json.object().put("s", text).build();
        assertEquals(text, Json.parse(value.toCanonicalJson()).string("s"));
    }

    @Test
    void escapesOnlyWhatMustBeEscaped() {
        // Escaping more would still be valid JSON and still be the wrong bytes.
        assertEquals("{\"s\":\"a/b\"}", Json.object().put("s", "a/b").build().toCanonicalJson());
        assertEquals("{\"s\":\"café\"}", Json.object().put("s", "café").build().toCanonicalJson());
    }

    @Test
    void escapesControlCharactersAsLowercaseHex() {
        String controls = new String(new char[] {0x00, 0x1f});
        assertEquals(
                "{\"s\":\"\\u0000\\u001f\"}",
                Json.object().put("s", controls).build().toCanonicalJson());
    }

    @Test
    void roundTripsAstralPlaneTextThroughSurrogateEscapes() {
        String astral = "𝕁𝕒";
        assertEquals(astral, Json.parse("{\"s\":\"\\ud835\\udd41\\ud835\\udd52\"}").string("s"));
        assertEquals(
                astral, Json.parse(Json.object().put("s", astral).build().toJsonText()).string("s"));
    }

    @Test
    void sortsObjectMembersByUtf16CodeUnit() {
        Json value = Json.object().put("b", 1).put("a", 2).put("C", 3).build();
        // Uppercase sorts before lowercase, which is what comparing code units
        // gives and what RFC 8785 requires.
        assertEquals("{\"C\":3,\"a\":2,\"b\":1}", value.toCanonicalJson());
    }

    @Test
    void keepsInsertionOrderForOrdinaryJsonText() {
        Json value = Json.object().put("b", 1).put("a", 2).build();
        assertEquals("{\"b\":1,\"a\":2}", value.toJsonText());
    }

    @Test
    void distinguishesAnIntegerFromANumberThatIsNot() {
        assertTrue(Json.parse("1") instanceof Json.Int);
        assertTrue(Json.parse("-1") instanceof Json.Int);
        assertTrue(Json.parse("1.0") instanceof Json.Dec);
        assertTrue(Json.parse("1e3") instanceof Json.Dec);
        // The distinction is the whole reason canonical encoding can refuse the
        // second kind instead of approximating it.
        assertThrows(IllegalStateException.class, () -> Json.parse("1.5").toCanonicalJson());
    }

    @Test
    void writesFractionalNumbersInOrdinaryJsonText() {
        assertEquals("{\"tempo\":128.5}", Json.parse("{\"tempo\":128.5}").toJsonText());
    }

    @Test
    void rejectsMalformedInput() {
        for (String malformed :
                List.of("", "{", "[1,", "{\"a\"}", "{\"a\":}", "tru", "\"unterminated", "{} extra")) {
            assertThrows(
                    IllegalArgumentException.class, () -> Json.parse(malformed), "accepted " + malformed);
        }
    }

    @Test
    void rejectsAnUnescapedControlCharacterInAString() {
        assertThrows(IllegalArgumentException.class, () -> Json.parse("{\"s\":\"a\nb\"}"));
    }

    @Test
    void reportsWhereItGaveUp() {
        IllegalArgumentException error =
                assertThrows(IllegalArgumentException.class, () -> Json.parse("{\"a\":1,}"));
        assertTrue(error.getMessage().contains("offset"), error.getMessage());
    }

    @Test
    void namesAMissingRequiredMember() {
        IllegalArgumentException error =
                assertThrows(IllegalArgumentException.class, () -> Json.parse("{}").string("message"));
        assertTrue(error.getMessage().contains("message"), error.getMessage());
    }

    @Test
    void omitsAbsentOptionalMembersRatherThanWritingNull() {
        // A profile with no genre must not carry `"genre":null`; the wire shape
        // distinguishes omitted from present-and-empty.
        Json value = Json.object().put("a", 1).putIfPresent("b", null).build();
        assertEquals("{\"a\":1}", value.toCanonicalJson());
    }
}
