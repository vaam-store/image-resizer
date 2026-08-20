//! `If-None-Match` matching against a single strong `ETag` (#44).
//!
//! Only strong comparison against one server-computed ETag is needed here -
//! `src/modules/router/middlewares.rs` computes exactly one ETag per
//! download response - so this only implements RFC 7232 §3.2's rules for
//! that shape: `*` matches unconditionally, and an entity-tag is compared
//! ignoring any leading weak (`W/`) marker on the *client's* value (a
//! client is allowed to send a weak validator and still get a cache hit
//! against our strong one for a safe/idempotent `GET`, per §2.3.2).

/// Returns `true` if `if_none_match` (the raw, possibly comma-separated
/// `If-None-Match` header value) is satisfied by `etag` (the server's own,
/// already-quoted strong ETag for the resource) - i.e. the request should
/// be answered with `304 Not Modified` rather than the full response.
pub fn if_none_match_satisfied(if_none_match: &str, etag: &str) -> bool {
    if_none_match
        .split(',')
        .map(str::trim)
        .any(|candidate| candidate == "*" || strip_weak_prefix(candidate) == etag)
}

fn strip_weak_prefix(value: &str) -> &str {
    value.strip_prefix("W/").unwrap_or(value)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wildcard_always_matches() {
        assert!(if_none_match_satisfied("*", "\"abc\""));
    }

    #[test]
    fn exact_strong_match() {
        assert!(if_none_match_satisfied("\"abc\"", "\"abc\""));
    }

    #[test]
    fn weak_client_value_matches_strong_server_etag() {
        assert!(if_none_match_satisfied("W/\"abc\"", "\"abc\""));
    }

    #[test]
    fn no_match_when_different() {
        assert!(!if_none_match_satisfied("\"xyz\"", "\"abc\""));
    }

    #[test]
    fn matches_one_of_several_comma_separated_values() {
        assert!(if_none_match_satisfied(
            "\"xyz\", \"abc\", \"def\"",
            "\"abc\""
        ));
    }

    #[test]
    fn empty_header_does_not_match() {
        assert!(!if_none_match_satisfied("", "\"abc\""));
    }

    #[test]
    fn whitespace_around_candidates_is_ignored() {
        assert!(if_none_match_satisfied(
            "  \"abc\"  ,  \"def\"  ",
            "\"abc\""
        ));
    }
}
