#include "auru/pm/json.hpp"

#include <algorithm>
#include <cmath>
#include <cstdio>
#include <cstdlib>

namespace auru::pm {
namespace {

// ── UTF-16 ordering ──────────────────────────────────────────────────────────

/// Decode the UTF-8 code point starting at `index`, advancing it.
///
/// Malformed input cannot reach here: everything compared has already been
/// through the parser or was produced by this library.
std::uint32_t next_code_point(const std::string& text, std::size_t& index) {
    const auto lead = static_cast<unsigned char>(text[index]);
    if (lead < 0x80) {
        index += 1;
        return lead;
    }
    const auto at = [&text](std::size_t offset) {
        return static_cast<std::uint32_t>(static_cast<unsigned char>(text[offset]) & 0x3Fu);
    };
    if ((lead & 0xE0u) == 0xC0u) {
        const std::uint32_t value = ((lead & 0x1Fu) << 6) | at(index + 1);
        index += 2;
        return value;
    }
    if ((lead & 0xF0u) == 0xE0u) {
        const std::uint32_t value = ((lead & 0x0Fu) << 12) | (at(index + 1) << 6) | at(index + 2);
        index += 3;
        return value;
    }
    const std::uint32_t value =
        ((lead & 0x07u) << 18) | (at(index + 1) << 12) | (at(index + 2) << 6) | at(index + 3);
    index += 4;
    return value;
}

/// Compare two UTF-8 strings as RFC 8785 requires: by UTF-16 code unit.
///
/// This is not the same as comparing UTF-8 bytes. Byte order equals code point
/// order, but UTF-16 puts the surrogate range (U+D800–U+DFFF) below U+E000, so
/// a key containing an astral character sorts differently under the two rules.
/// Every key this protocol uses is ASCII, where the rules agree — but a
/// canonicalizer that is only correct for its own inputs is a trap for whoever
/// reuses it next.
bool utf16_less(const std::string& left, const std::string& right) {
    std::size_t left_index = 0;
    std::size_t right_index = 0;

    // A code point above the BMP becomes a surrogate pair; comparing the lead
    // surrogate is enough, because a shared lead means comparing the trail
    // resolves the same way as comparing the code points.
    const auto lead_unit = [](std::uint32_t code_point) -> std::uint32_t {
        return code_point >= 0x10000u ? 0xD800u + ((code_point - 0x10000u) >> 10) : code_point;
    };

    while (left_index < left.size() && right_index < right.size()) {
        const std::size_t left_start = left_index;
        const std::size_t right_start = right_index;
        const std::uint32_t left_point = next_code_point(left, left_index);
        const std::uint32_t right_point = next_code_point(right, right_index);

        const std::uint32_t left_unit = lead_unit(left_point);
        const std::uint32_t right_unit = lead_unit(right_point);
        if (left_unit != right_unit) {
            return left_unit < right_unit;
        }
        if (left_point != right_point) {
            // Same lead surrogate, different code point: the trail decides, and
            // it orders the same way the code points do.
            return left_point < right_point;
        }
        static_cast<void>(left_start);
        static_cast<void>(right_start);
    }
    return (left.size() - left_index) < (right.size() - right_index);
}

// ── Writing ──────────────────────────────────────────────────────────────────

void write_string(const std::string& value, std::string& out) {
    out.push_back('"');
    for (char raw : value) {
        const auto character = static_cast<unsigned char>(raw);
        switch (character) {
            case '"':
                out.append("\\\"");
                break;
            case '\\':
                out.append("\\\\");
                break;
            case '\b':
                out.append("\\b");
                break;
            case '\f':
                out.append("\\f");
                break;
            case '\n':
                out.append("\\n");
                break;
            case '\r':
                out.append("\\r");
                break;
            case '\t':
                out.append("\\t");
                break;
            default:
                if (character < 0x20u) {
                    char escape[7];
                    std::snprintf(escape, sizeof(escape), "\\u%04x", character);
                    out.append(escape);
                } else {
                    // No escaping of `/`, and none of non-ASCII, which travels
                    // as UTF-8. Escaping more would still be valid JSON and
                    // still be the wrong bytes.
                    out.push_back(raw);
                }
                break;
        }
    }
    out.push_back('"');
}

std::string format_double(double value) {
    char buffer[32];
    std::snprintf(buffer, sizeof(buffer), "%.17g", value);
    return buffer;
}

}  // namespace

// ── Construction ─────────────────────────────────────────────────────────────

Json Json::null() { return Json(); }

Json Json::boolean(bool value) {
    Json json;
    json.kind_ = Kind::Bool;
    json.bool_ = value;
    return json;
}

Json Json::integer(std::int64_t value) {
    Json json;
    json.kind_ = Kind::Int;
    json.int_ = value;
    return json;
}

Json Json::number(double value) {
    Json json;
    json.kind_ = Kind::Dec;
    json.dec_ = value;
    return json;
}

Json Json::string(std::string value) {
    Json json;
    json.kind_ = Kind::Str;
    json.string_ = std::move(value);
    return json;
}

Json Json::array(std::vector<Json> elements) {
    Json json;
    json.kind_ = Kind::Arr;
    json.elements_ = std::move(elements);
    return json;
}

Json Json::object(Members members) {
    Json json;
    json.kind_ = Kind::Obj;
    json.members_ = std::move(members);
    return json;
}

// ── Reading ──────────────────────────────────────────────────────────────────

std::optional<std::string> Json::as_text() const {
    return kind_ == Kind::Str ? std::optional<std::string>(string_) : std::nullopt;
}

std::optional<std::int64_t> Json::as_integer() const {
    return kind_ == Kind::Int ? std::optional<std::int64_t>(int_) : std::nullopt;
}

std::optional<double> Json::as_number() const {
    if (kind_ == Kind::Int) {
        return static_cast<double>(int_);
    }
    return kind_ == Kind::Dec ? std::optional<double>(dec_) : std::nullopt;
}

std::optional<bool> Json::as_bool() const {
    return kind_ == Kind::Bool ? std::optional<bool>(bool_) : std::nullopt;
}

const Json* Json::find(const std::string& name) const {
    if (kind_ != Kind::Obj) {
        return nullptr;
    }
    for (const auto& member : members_) {
        if (member.first == name) {
            return member.second.is_null() ? nullptr : &member.second;
        }
    }
    return nullptr;
}

Result<std::string> Json::string_at(const std::string& name) const {
    const Json* member = find(name);
    if (member == nullptr) {
        return make_error<std::string>(ErrorCode::BadRequest,
                                       "missing required member \"" + name + "\"");
    }
    auto text = member->as_text();
    if (!text) {
        return make_error<std::string>(ErrorCode::BadRequest, "\"" + name + "\" is not a string");
    }
    return Result<std::string>::ok(std::move(*text));
}

std::optional<std::string> Json::optional_string(const std::string& name) const {
    const Json* member = find(name);
    return member == nullptr ? std::nullopt : member->as_text();
}

Result<std::int64_t> Json::integer_at(const std::string& name) const {
    const Json* member = find(name);
    if (member == nullptr) {
        return make_error<std::int64_t>(ErrorCode::BadRequest,
                                        "missing required member \"" + name + "\"");
    }
    auto value = member->as_integer();
    if (!value) {
        return make_error<std::int64_t>(ErrorCode::BadRequest,
                                        "\"" + name + "\" is not a whole number");
    }
    return Result<std::int64_t>::ok(*value);
}

bool Json::bool_at(const std::string& name, bool fallback) const {
    const Json* member = find(name);
    if (member == nullptr) {
        return fallback;
    }
    return member->as_bool().value_or(fallback);
}

void Json::set(std::string name, Json value) {
    kind_ = Kind::Obj;
    for (auto& member : members_) {
        if (member.first == name) {
            member.second = std::move(value);
            return;
        }
    }
    members_.emplace_back(std::move(name), std::move(value));
}

void Json::set_if(std::string name, std::optional<Json> value) {
    if (value && !value->is_null()) {
        set(std::move(name), std::move(*value));
    } else {
        kind_ = Kind::Obj;
    }
}

// ── Encoding ─────────────────────────────────────────────────────────────────

namespace {

/// Shared writer. `canonical` sorts members and refuses what RFC 8785 cannot
/// represent; otherwise this is an ordinary serializer.
std::optional<Error> write(const Json& value, std::string& out, bool canonical) {
    switch (value.kind()) {
        case Json::Kind::Null:
            out.append("null");
            return std::nullopt;
        case Json::Kind::Bool:
            out.append(value.as_bool().value() ? "true" : "false");
            return std::nullopt;
        case Json::Kind::Str:
            write_string(value.as_text().value(), out);
            return std::nullopt;
        case Json::Kind::Int: {
            const std::int64_t number = value.as_integer().value();
            if (canonical && (number > Json::kMaxSafeInteger || number < -Json::kMaxSafeInteger)) {
                Error error;
                error.code = ErrorCode::BadRequest;
                error.message =
                    "integer " + std::to_string(number) +
                    " is outside +/-(2^53 - 1); RFC 8785 numbers are IEEE-754 binary64, so no two"
                    " implementations would agree on it";
                return error;
            }
            out.append(std::to_string(number));
            return std::nullopt;
        }
        case Json::Kind::Dec: {
            if (canonical) {
                Error error;
                error.code = ErrorCode::BadRequest;
                error.message =
                    "cannot canonicalize the non-integral number " +
                    format_double(value.as_number().value()) +
                    "; RFC 8785 requires ECMAScript number formatting, which this library"
                    " implements only for integers because nothing in a commit is fractional";
                return error;
            }
            out.append(format_double(value.as_number().value()));
            return std::nullopt;
        }
        case Json::Kind::Arr: {
            out.push_back('[');
            bool first = true;
            for (const Json& element : value.elements()) {
                if (!first) {
                    out.push_back(',');
                }
                first = false;
                if (auto error = write(element, out, canonical)) {
                    return error;
                }
            }
            out.push_back(']');
            return std::nullopt;
        }
        case Json::Kind::Obj: {
            out.push_back('{');
            std::vector<const std::pair<std::string, Json>*> members;
            members.reserve(value.members().size());
            for (const auto& member : value.members()) {
                members.push_back(&member);
            }
            if (canonical) {
                std::stable_sort(members.begin(), members.end(),
                                 [](const auto* left, const auto* right) {
                                     return utf16_less(left->first, right->first);
                                 });
            }
            bool first = true;
            for (const auto* member : members) {
                if (!first) {
                    out.push_back(',');
                }
                first = false;
                write_string(member->first, out);
                out.push_back(':');
                if (auto error = write(member->second, out, canonical)) {
                    return error;
                }
            }
            out.push_back('}');
            return std::nullopt;
        }
    }
    return std::nullopt;
}

}  // namespace

Result<std::string> Json::to_canonical_json() const {
    std::string out;
    if (auto error = write(*this, out, true)) {
        return Result<std::string>::fail(*error);
    }
    return Result<std::string>::ok(std::move(out));
}

std::string Json::to_json_text() const {
    std::string out;
    static_cast<void>(write(*this, out, false));
    return out;
}

// ── Parsing ──────────────────────────────────────────────────────────────────

namespace {

/// A recursive-descent parser. Small because JSON is small.
class Parser {
public:
    explicit Parser(const std::string& text) : text_(text) {}

    Result<Json> parse_document() {
        auto value = parse_value();
        if (!value) {
            return value;
        }
        skip_whitespace();
        if (position_ != text_.size()) {
            return error("trailing content after the JSON value");
        }
        return value;
    }

private:
    Result<Json> error(const std::string& message) {
        return make_error<Json>(ErrorCode::BadRequest,
                                "JSON at offset " + std::to_string(position_) + ": " + message);
    }

    void skip_whitespace() {
        while (position_ < text_.size()) {
            const char character = text_[position_];
            if (character == ' ' || character == '\t' || character == '\n' || character == '\r') {
                ++position_;
            } else {
                return;
            }
        }
    }

    Result<Json> parse_value() {
        skip_whitespace();
        if (position_ >= text_.size()) {
            return error("unexpected end of input");
        }
        switch (text_[position_]) {
            case '{':
                return parse_object();
            case '[':
                return parse_array();
            case '"': {
                auto text = parse_string();
                if (!text) {
                    return Result<Json>::fail(text.error());
                }
                return Result<Json>::ok(Json::string(std::move(text).value()));
            }
            case 't':
                return parse_literal("true", Json::boolean(true));
            case 'f':
                return parse_literal("false", Json::boolean(false));
            case 'n':
                return parse_literal("null", Json::null());
            default:
                return parse_number();
        }
    }

    Result<Json> parse_literal(const char* literal, Json value) {
        const std::size_t length = std::char_traits<char>::length(literal);
        if (text_.compare(position_, length, literal) != 0) {
            return error(std::string("expected ") + literal);
        }
        position_ += length;
        return Result<Json>::ok(std::move(value));
    }

    Result<Json> parse_object() {
        ++position_;  // '{'
        Json::Members members;
        skip_whitespace();
        if (position_ < text_.size() && text_[position_] == '}') {
            ++position_;
            return Result<Json>::ok(Json::object(std::move(members)));
        }
        while (true) {
            skip_whitespace();
            if (position_ >= text_.size() || text_[position_] != '"') {
                return error("expected a member name");
            }
            auto name = parse_string();
            if (!name) {
                return Result<Json>::fail(name.error());
            }
            skip_whitespace();
            if (position_ >= text_.size() || text_[position_] != ':') {
                return error("expected ':' after a member name");
            }
            ++position_;
            auto value = parse_value();
            if (!value) {
                return value;
            }
            members.emplace_back(std::move(name).value(), std::move(value).value());

            skip_whitespace();
            if (position_ >= text_.size()) {
                return error("unterminated object");
            }
            const char next = text_[position_++];
            if (next == '}') {
                return Result<Json>::ok(Json::object(std::move(members)));
            }
            if (next != ',') {
                return error("expected ',' or '}' in object");
            }
        }
    }

    Result<Json> parse_array() {
        ++position_;  // '['
        std::vector<Json> elements;
        skip_whitespace();
        if (position_ < text_.size() && text_[position_] == ']') {
            ++position_;
            return Result<Json>::ok(Json::array(std::move(elements)));
        }
        while (true) {
            auto value = parse_value();
            if (!value) {
                return value;
            }
            elements.push_back(std::move(value).value());

            skip_whitespace();
            if (position_ >= text_.size()) {
                return error("unterminated array");
            }
            const char next = text_[position_++];
            if (next == ']') {
                return Result<Json>::ok(Json::array(std::move(elements)));
            }
            if (next != ',') {
                return error("expected ',' or ']' in array");
            }
        }
    }

    void append_utf8(std::uint32_t code_point, std::string& out) {
        if (code_point < 0x80u) {
            out.push_back(static_cast<char>(code_point));
        } else if (code_point < 0x800u) {
            out.push_back(static_cast<char>(0xC0u | (code_point >> 6)));
            out.push_back(static_cast<char>(0x80u | (code_point & 0x3Fu)));
        } else if (code_point < 0x10000u) {
            out.push_back(static_cast<char>(0xE0u | (code_point >> 12)));
            out.push_back(static_cast<char>(0x80u | ((code_point >> 6) & 0x3Fu)));
            out.push_back(static_cast<char>(0x80u | (code_point & 0x3Fu)));
        } else {
            out.push_back(static_cast<char>(0xF0u | (code_point >> 18)));
            out.push_back(static_cast<char>(0x80u | ((code_point >> 12) & 0x3Fu)));
            out.push_back(static_cast<char>(0x80u | ((code_point >> 6) & 0x3Fu)));
            out.push_back(static_cast<char>(0x80u | (code_point & 0x3Fu)));
        }
    }

    Result<std::string> string_error(const std::string& message) {
        return make_error<std::string>(
            ErrorCode::BadRequest,
            "JSON at offset " + std::to_string(position_) + ": " + message);
    }

    Result<std::string> parse_string() {
        ++position_;  // '"'
        std::string out;
        while (true) {
            if (position_ >= text_.size()) {
                return string_error("unterminated string");
            }
            const char raw = text_[position_++];
            if (raw == '"') {
                return Result<std::string>::ok(std::move(out));
            }
            if (raw != '\\') {
                if (static_cast<unsigned char>(raw) < 0x20u) {
                    return string_error("unescaped control character in string");
                }
                out.push_back(raw);
                continue;
            }
            if (position_ >= text_.size()) {
                return string_error("truncated escape");
            }
            const char escape = text_[position_++];
            switch (escape) {
                case '"': out.push_back('"'); break;
                case '\\': out.push_back('\\'); break;
                case '/': out.push_back('/'); break;
                case 'b': out.push_back('\b'); break;
                case 'f': out.push_back('\f'); break;
                case 'n': out.push_back('\n'); break;
                case 'r': out.push_back('\r'); break;
                case 't': out.push_back('\t'); break;
                case 'u': {
                    if (position_ + 4 > text_.size()) {
                        return string_error("truncated \\u escape");
                    }
                    auto unit = std::strtoul(text_.substr(position_, 4).c_str(), nullptr, 16);
                    position_ += 4;
                    std::uint32_t code_point = static_cast<std::uint32_t>(unit);
                    // A lead surrogate is only half a character; the trail
                    // follows as a second escape.
                    if (code_point >= 0xD800u && code_point <= 0xDBFFu
                        && position_ + 6 <= text_.size() && text_[position_] == '\\'
                        && text_[position_ + 1] == 'u') {
                        const auto trail = static_cast<std::uint32_t>(
                            std::strtoul(text_.substr(position_ + 2, 4).c_str(), nullptr, 16));
                        if (trail >= 0xDC00u && trail <= 0xDFFFu) {
                            position_ += 6;
                            code_point = 0x10000u + ((code_point - 0xD800u) << 10)
                                         + (trail - 0xDC00u);
                        }
                    }
                    append_utf8(code_point, out);
                    break;
                }
                default:
                    return string_error(std::string("unknown escape \\") + escape);
            }
        }
    }

    Result<Json> parse_number() {
        const std::size_t start = position_;
        if (position_ < text_.size() && text_[position_] == '-') {
            ++position_;
        }
        bool fractional = false;
        while (position_ < text_.size()) {
            const char character = text_[position_];
            if (character >= '0' && character <= '9') {
                ++position_;
            } else if (character == '.' || character == 'e' || character == 'E'
                       || character == '+' || character == '-') {
                fractional = true;
                ++position_;
            } else {
                break;
            }
        }

        const std::string literal = text_.substr(start, position_ - start);
        if (literal.empty() || literal == "-") {
            return error("expected a value");
        }
        if (!fractional) {
            errno = 0;
            char* end = nullptr;
            const long long parsed = std::strtoll(literal.c_str(), &end, 10);
            if (errno == 0 && end != nullptr && *end == '\0') {
                return Result<Json>::ok(Json::integer(parsed));
            }
            // Falls through to a double, which loses precision — but a value
            // that large is already outside what this protocol permits, and
            // canonicalizing it will say so.
        }
        return Result<Json>::ok(Json::number(std::strtod(literal.c_str(), nullptr)));
    }

    const std::string& text_;
    std::size_t position_ = 0;
};

}  // namespace

Result<Json> Json::parse(const std::string& text) {
    Parser parser(text);
    return parser.parse_document();
}

}  // namespace auru::pm
