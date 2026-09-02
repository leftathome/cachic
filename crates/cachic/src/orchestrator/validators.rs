//! Validator comparison and `If-Range`.
//!
//! Content that is "effectively immutable" is not actually immutable. When an object changes
//! under us mid-stream, the one unacceptable outcome is serving a response whose first half came
//! from a version that no longer exists (FR-14).
//!
//! Generation is part of the slice key, so invalidation is atomic by construction: bumping it
//! makes every old slice unreachable at once, with no sweep and no window in which a response
//! could mix versions.

/// An object's validators, as the origin reported them.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Validators {
    pub etag: Option<String>,
    pub last_modified: Option<String>,
}

impl Validators {
    pub fn new(etag: Option<String>, last_modified: Option<String>) -> Self {
        Self {
            etag,
            last_modified,
        }
    }

    /// Whether `other` describes the same entity.
    ///
    /// An `ETag` is authoritative when both sides have one; `Last-Modified` is the fallback. If
    /// neither side offers any validator we cannot tell, and we say they match: refusing to
    /// serve an object because its origin is silent about versioning would break a large part of
    /// the CDN population for no benefit.
    pub fn matches(&self, other: &Validators) -> bool {
        match (&self.etag, &other.etag) {
            (Some(a), Some(b)) => return strong_eq(a, b),
            // One side has an ETag and the other does not: the entity changed, or the origin is
            // inconsistent. Either way, do not assume they are the same bytes.
            (Some(_), None) | (None, Some(_)) => return false,
            (None, None) => {}
        }
        match (&self.last_modified, &other.last_modified) {
            (Some(a), Some(b)) => a == b,
            (Some(_), None) | (None, Some(_)) => false,
            (None, None) => true,
        }
    }

    pub fn is_empty(&self) -> bool {
        self.etag.is_none() && self.last_modified.is_none()
    }
}

/// Strong comparison of two ETags.
///
/// A weak ETag (`W/"x"`) means the entities are semantically equivalent but not byte-identical.
/// For a byte-range cache that is not good enough: we are assembling one response out of slices
/// fetched at different times, and semantic equivalence permits those bytes to differ.
fn strong_eq(a: &str, b: &str) -> bool {
    if a.starts_with("W/") || b.starts_with("W/") {
        return false;
    }
    a == b
}

/// Whether an `If-Range` header permits a partial response (FR-17).
///
/// `If-Range` means "send me the range if the entity is unchanged, otherwise send me the whole
/// thing". A mismatch is answered with `200` and the full object, not an error.
pub fn if_range_matches(header: &str, current: &Validators) -> bool {
    let value = header.trim();
    if value.is_empty() {
        return false;
    }
    // An ETag form starts with a quote or the weak marker; anything else is an HTTP-date.
    if value.starts_with('"') || value.starts_with("W/") {
        match &current.etag {
            Some(etag) => strong_eq(value, etag),
            None => false,
        }
    } else {
        match &current.last_modified {
            Some(modified) => modified == value,
            None => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(etag: Option<&str>, modified: Option<&str>) -> Validators {
        Validators::new(etag.map(str::to_owned), modified.map(str::to_owned))
    }

    #[test]
    fn identical_etags_match() {
        assert!(v(Some("\"abc\""), None).matches(&v(Some("\"abc\""), None)));
    }

    #[test]
    fn different_etags_do_not_match() {
        assert!(!v(Some("\"abc\""), None).matches(&v(Some("\"def\""), None)));
    }

    #[test]
    fn a_weak_etag_never_matches() {
        // Weak means semantically equivalent, not byte-identical. We assemble one response from
        // slices fetched at different times, so semantic equivalence is not sufficient.
        assert!(!v(Some("W/\"abc\""), None).matches(&v(Some("W/\"abc\""), None)));
        assert!(!v(Some("W/\"abc\""), None).matches(&v(Some("\"abc\""), None)));
    }

    #[test]
    fn an_etag_appearing_or_disappearing_is_a_mismatch() {
        // Either the entity changed or the origin is inconsistent. Both mean "do not assume
        // these are the same bytes".
        assert!(!v(Some("\"abc\""), None).matches(&v(None, None)));
        assert!(!v(None, None).matches(&v(Some("\"abc\""), None)));
    }

    #[test]
    fn last_modified_is_the_fallback_when_neither_side_has_an_etag() {
        let a = v(None, Some("Wed, 21 Oct 2015 07:28:00 GMT"));
        assert!(a.matches(&a.clone()));
        assert!(!a.matches(&v(None, Some("Thu, 22 Oct 2015 07:28:00 GMT"))));
    }

    #[test]
    fn an_etag_wins_over_last_modified() {
        // A CDN that rewrites Last-Modified per edge but keeps a stable ETag must not be treated
        // as changing on every request.
        let a = v(Some("\"abc\""), Some("Wed, 21 Oct 2015 07:28:00 GMT"));
        let b = v(Some("\"abc\""), Some("Thu, 22 Oct 2015 07:28:00 GMT"));
        assert!(a.matches(&b));
    }

    #[test]
    fn no_validators_at_all_is_treated_as_a_match() {
        // Refusing to serve because the origin is silent about versioning would break a large
        // part of the CDN population for no benefit.
        assert!(v(None, None).matches(&v(None, None)));
        assert!(v(None, None).is_empty());
    }

    #[test]
    fn if_range_accepts_a_matching_etag() {
        let current = v(Some("\"abc\""), None);
        assert!(if_range_matches("\"abc\"", &current));
        assert!(!if_range_matches("\"def\"", &current));
    }

    #[test]
    fn if_range_rejects_a_weak_etag() {
        let current = v(Some("W/\"abc\""), None);
        assert!(!if_range_matches("W/\"abc\"", &current));
    }

    #[test]
    fn if_range_accepts_a_matching_date() {
        let current = v(None, Some("Wed, 21 Oct 2015 07:28:00 GMT"));
        assert!(if_range_matches("Wed, 21 Oct 2015 07:28:00 GMT", &current));
        assert!(!if_range_matches("Thu, 22 Oct 2015 07:28:00 GMT", &current));
    }

    #[test]
    fn if_range_against_an_object_with_no_validators_is_a_mismatch() {
        // We cannot prove the entity is unchanged, so the safe answer is the whole object.
        assert!(!if_range_matches("\"abc\"", &v(None, None)));
        assert!(!if_range_matches(
            "Wed, 21 Oct 2015 07:28:00 GMT",
            &v(None, None)
        ));
    }

    #[test]
    fn an_empty_if_range_is_a_mismatch() {
        assert!(!if_range_matches("", &v(Some("\"abc\""), None)));
        assert!(!if_range_matches("   ", &v(Some("\"abc\""), None)));
    }
}
