#include <string>

#include "auru/pm/json.hpp"
#include "harness.hpp"

using auru::pm::Json;

/// The hand-written JSON layer.
///
/// Everything else in this library rests on it, and the interesting cases are
/// the ones a canonical encoder has to get exactly right rather than merely
/// parse: escapes, control characters, astral-plane text, and the boundary
/// between an integer and a number that is not one.

TEST("parses the shapes a wire body uses") {
    auto value = Json::parse(
        "{\"a\":1,\"b\":\"two\",\"c\":[1,2,3],\"d\":{\"e\":true},\"f\":null,\"g\":false}");
    CHECK(value.has_value());
    const Json& json = value.value();
    CHECK_EQ(json.integer_at("a").value(), 1);
    CHECK_EQ(json.string_at("b").value(), std::string("two"));
    CHECK_EQ(json.find("c")->elements().size(), std::size_t{3});
    CHECK(json.find("d")->bool_at("e", false));
    // JSON null reads as absent: a caller never distinguishes "missing" from
    // "explicitly null" when neither carries information.
    CHECK(json.find("f") == nullptr);
    CHECK(!json.bool_at("g", true));
}

TEST("ignores insignificant whitespace") {
    CHECK_EQ(Json::parse("{\"a\":1}").value().to_canonical_json().value(),
             Json::parse("  {\n  \"a\" :\t1\r\n}  ").value().to_canonical_json().value());
}

TEST("round trips every escape") {
    const std::string text = "quote \" backslash \\ slash / tab \t newline \n return \r";
    Json json = Json::object({});
    json.set("s", Json::string(text));
    auto reparsed = Json::parse(json.to_canonical_json().value());
    CHECK_EQ(reparsed.value().string_at("s").value(), text);
}

TEST("escapes only what must be escaped") {
    // Escaping more would still be valid JSON and still be the wrong bytes.
    Json slash = Json::object({});
    slash.set("s", Json::string("a/b"));
    CHECK_EQ(slash.to_canonical_json().value(), std::string("{\"s\":\"a/b\"}"));

    Json accented = Json::object({});
    accented.set("s", Json::string("caf\xc3\xa9"));
    CHECK_EQ(accented.to_canonical_json().value(), std::string("{\"s\":\"caf\xc3\xa9\"}"));
}

TEST("escapes control characters as lowercase hex") {
    std::string controls;
    controls.push_back('\x00');
    controls.push_back('\x1f');
    Json json = Json::object({});
    json.set("s", Json::string(controls));
    CHECK_EQ(json.to_canonical_json().value(), std::string("{\"s\":\"\\u0000\\u001f\"}"));
}

TEST("round trips astral-plane text through surrogate escapes") {
    // U+1D541 U+1D552, written as a surrogate pair on the wire.
    const std::string astral = "\xf0\x9d\x95\x81\xf0\x9d\x95\x92";
    auto parsed = Json::parse("{\"s\":\"\\ud835\\udd41\\ud835\\udd52\"}");
    CHECK(parsed.has_value());
    CHECK_EQ(parsed.value().string_at("s").value(), astral);

    Json json = Json::object({});
    json.set("s", Json::string(astral));
    auto reparsed = Json::parse(json.to_json_text());
    CHECK_EQ(reparsed.value().string_at("s").value(), astral);
}

TEST("sorts object members by UTF-16 code unit") {
    Json json = Json::object({});
    json.set("b", Json::integer(1));
    json.set("a", Json::integer(2));
    json.set("C", Json::integer(3));
    // Uppercase sorts before lowercase, which is what comparing code units
    // gives and what RFC 8785 requires.
    CHECK_EQ(json.to_canonical_json().value(), std::string("{\"C\":3,\"a\":2,\"b\":1}"));
}

TEST("sorts an astral key the way UTF-16 does, not the way UTF-8 bytes do") {
    // U+FFFD sorts *after* U+10000 in UTF-8 byte order but *before* it in
    // UTF-16, because an astral character leads with a surrogate in the D800
    // range. Comparing bytes would put these the other way round.
    Json json = Json::object({});
    json.set("\xf0\x90\x80\x80", Json::integer(1));  // U+10000
    json.set("\xef\xbf\xbd", Json::integer(2));      // U+FFFD
    const std::string canonical = json.to_canonical_json().value();
    CHECK(canonical.find("\xf0\x90\x80\x80") < canonical.find("\xef\xbf\xbd"));
}

TEST("keeps insertion order for ordinary JSON text") {
    Json json = Json::object({});
    json.set("b", Json::integer(1));
    json.set("a", Json::integer(2));
    CHECK_EQ(json.to_json_text(), std::string("{\"b\":1,\"a\":2}"));
}

TEST("distinguishes an integer from a number that is not") {
    CHECK(Json::parse("1").value().kind() == Json::Kind::Int);
    CHECK(Json::parse("-1").value().kind() == Json::Kind::Int);
    CHECK(Json::parse("1.0").value().kind() == Json::Kind::Dec);
    CHECK(Json::parse("1e3").value().kind() == Json::Kind::Dec);
    // The distinction is the whole reason canonical encoding can refuse the
    // second kind instead of approximating it.
    CHECK(!Json::parse("1.5").value().to_canonical_json().has_value());
}

TEST("rejects malformed input") {
    for (const char* malformed :
         {"", "{", "[1,", "{\"a\"}", "{\"a\":}", "tru", "\"unterminated", "{} extra"}) {
        if (Json::parse(malformed).has_value()) {
            harness::fail(__FILE__, __LINE__, std::string("accepted ") + malformed);
        }
    }
}

TEST("rejects an unescaped control character in a string") {
    CHECK(!Json::parse("{\"s\":\"a\nb\"}").has_value());
}

TEST("reports where it gave up") {
    auto parsed = Json::parse("{\"a\":1,}");
    CHECK(!parsed.has_value());
    CHECK(parsed.error().message.find("offset") != std::string::npos);
}

TEST("names a missing required member") {
    auto missing = Json::parse("{}").value().string_at("message");
    CHECK(!missing.has_value());
    CHECK(missing.error().message.find("message") != std::string::npos);
}

TEST("omits absent optional members rather than writing null") {
    // A profile with no genre must not carry `"genre":null`; the wire shape
    // distinguishes omitted from present-and-empty.
    Json json = Json::object({});
    json.set("a", Json::integer(1));
    json.set_if("b", std::nullopt);
    CHECK_EQ(json.to_canonical_json().value(), std::string("{\"a\":1}"));
}

TEST_MAIN("json")
