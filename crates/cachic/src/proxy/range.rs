//! Range parsing and slice-plan arithmetic.
//!
//! Pure functions with no IO, because this is the code most likely to be subtly wrong at object
//! boundaries and it is the surface TASK-21 will fuzz.
//!
//! Promoted from the M0 spike (TASK-03) unchanged apart from this note: it was already
//! property-tested, and the tiling property below - that each slice window concatenates back to
//! exactly the requested range, with no gaps and no overlaps - is the invariant the whole
//! orchestrator rests on.

/// A client's `Range` request, before it is resolved against a known object length.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RangeSpec {
    /// `bytes=a-b`
    FromTo(u64, u64),
    /// `bytes=a-`
    From(u64),
    /// `bytes=-n`, the last `n` bytes.
    Suffix(u64),
}

/// An inclusive byte range resolved against a known object length.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ByteRange {
    pub start: u64,
    /// Inclusive.
    pub end: u64,
}

impl ByteRange {
    pub fn len(&self) -> u64 {
        self.end - self.start + 1
    }

    pub fn is_empty(&self) -> bool {
        false
    }
}

/// Why a `Range` header did not yield a single satisfiable range.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RangeError {
    /// Not a `bytes=` range, or otherwise not valid. Per RFC 9110 a Range header that cannot be parsed
    /// is ignored and the full object is returned.
    Malformed,
    /// More than one range. RFC 9110 permits answering with the full object, which is what
    /// monolithic effectively does and what we do (FR-11).
    Multiple,
    /// Syntactically valid but outside the object: this is a 416.
    Unsatisfiable,
}

/// Parse a `Range` header value into a single range spec.
pub fn parse_range(value: &str) -> Result<RangeSpec, RangeError> {
    let spec = value
        .trim()
        .strip_prefix("bytes=")
        .ok_or(RangeError::Malformed)?;
    if spec.contains(',') {
        return Err(RangeError::Multiple);
    }
    let (start, end) = spec.split_once('-').ok_or(RangeError::Malformed)?;
    match (start.trim(), end.trim()) {
        ("", "") => Err(RangeError::Malformed),
        ("", suffix) => {
            let n: u64 = suffix.parse().map_err(|_| RangeError::Malformed)?;
            Ok(RangeSpec::Suffix(n))
        }
        (s, "") => {
            let start: u64 = s.parse().map_err(|_| RangeError::Malformed)?;
            Ok(RangeSpec::From(start))
        }
        (s, e) => {
            let start: u64 = s.parse().map_err(|_| RangeError::Malformed)?;
            let end: u64 = e.parse().map_err(|_| RangeError::Malformed)?;
            if start > end {
                return Err(RangeError::Malformed);
            }
            Ok(RangeSpec::FromTo(start, end))
        }
    }
}

/// Resolve a spec against a known object length.
///
/// A zero-length object satisfies no range (FR-15).
pub fn resolve(spec: RangeSpec, total: u64) -> Result<ByteRange, RangeError> {
    if total == 0 {
        return Err(RangeError::Unsatisfiable);
    }
    let last = total - 1;
    match spec {
        RangeSpec::FromTo(start, end) => {
            if start > last {
                Err(RangeError::Unsatisfiable)
            } else {
                // An end beyond the object is clamped, not rejected.
                Ok(ByteRange {
                    start,
                    end: end.min(last),
                })
            }
        }
        RangeSpec::From(start) => {
            if start > last {
                Err(RangeError::Unsatisfiable)
            } else {
                Ok(ByteRange { start, end: last })
            }
        }
        RangeSpec::Suffix(n) => {
            if n == 0 {
                Err(RangeError::Unsatisfiable)
            } else {
                let n = n.min(total);
                Ok(ByteRange {
                    start: total - n,
                    end: last,
                })
            }
        }
    }
}

/// The whole object as a range. Used when there is no `Range` header, or when one is ignored.
pub fn whole(total: u64) -> Option<ByteRange> {
    (total > 0).then(|| ByteRange {
        start: 0,
        end: total - 1,
    })
}

/// The inclusive span of slice indices covering a byte range.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SlicePlan {
    pub first: u32,
    /// Inclusive.
    pub last: u32,
    pub slice_size: u32,
}

impl SlicePlan {
    pub fn count(&self) -> u32 {
        self.last - self.first + 1
    }

    pub fn indices(&self) -> impl Iterator<Item = u32> {
        self.first..=self.last
    }
}

/// Map a byte range onto slice indices.
pub fn plan(range: ByteRange, slice_size: u32) -> SlicePlan {
    debug_assert!(slice_size > 0, "slice size must be non-zero");
    let s = slice_size as u64;
    SlicePlan {
        first: (range.start / s) as u32,
        last: (range.end / s) as u32,
        slice_size,
    }
}

/// The absolute byte range a slice covers within an object of `total` bytes.
///
/// The final slice of an object is short unless the object is an exact multiple of the slice size.
pub fn slice_extent(index: u32, slice_size: u32, total: u64) -> ByteRange {
    let s = slice_size as u64;
    let start = index as u64 * s;
    let end = (start + s - 1).min(total.saturating_sub(1));
    ByteRange { start, end }
}

/// The sub-slice of slice `index`'s payload that satisfies `wanted`, as offsets into the payload.
pub fn payload_window(index: u32, slice_size: u32, wanted: ByteRange) -> (usize, usize) {
    let s = slice_size as u64;
    let slice_start = index as u64 * s;
    let from = wanted.start.saturating_sub(slice_start);
    let to = (wanted.end - slice_start + 1).min(s);
    (from as usize, to as usize)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_the_three_range_forms() {
        assert_eq!(parse_range("bytes=0-99"), Ok(RangeSpec::FromTo(0, 99)));
        assert_eq!(parse_range("bytes=500-"), Ok(RangeSpec::From(500)));
        assert_eq!(parse_range("bytes=-100"), Ok(RangeSpec::Suffix(100)));
        assert_eq!(parse_range(" bytes=0-0 "), Ok(RangeSpec::FromTo(0, 0)));
    }

    #[test]
    fn rejects_malformed_and_multiple_ranges() {
        assert_eq!(parse_range("items=0-99"), Err(RangeError::Malformed));
        assert_eq!(parse_range("bytes=99-0"), Err(RangeError::Malformed));
        assert_eq!(parse_range("bytes=-"), Err(RangeError::Malformed));
        assert_eq!(parse_range("bytes=abc-def"), Err(RangeError::Malformed));
        assert_eq!(parse_range("bytes=0-10,20-30"), Err(RangeError::Multiple));
    }

    #[test]
    fn resolves_against_object_length() {
        assert_eq!(
            resolve(RangeSpec::FromTo(0, 99), 1000),
            Ok(ByteRange { start: 0, end: 99 })
        );
        // Over-long end is clamped.
        assert_eq!(
            resolve(RangeSpec::FromTo(900, 5000), 1000),
            Ok(ByteRange {
                start: 900,
                end: 999
            })
        );
        assert_eq!(
            resolve(RangeSpec::From(500), 1000),
            Ok(ByteRange {
                start: 500,
                end: 999
            })
        );
        assert_eq!(
            resolve(RangeSpec::Suffix(100), 1000),
            Ok(ByteRange {
                start: 900,
                end: 999
            })
        );
        // A suffix longer than the object is the whole object.
        assert_eq!(
            resolve(RangeSpec::Suffix(5000), 1000),
            Ok(ByteRange { start: 0, end: 999 })
        );
    }

    #[test]
    fn detects_unsatisfiable_ranges() {
        assert_eq!(
            resolve(RangeSpec::FromTo(1000, 2000), 1000),
            Err(RangeError::Unsatisfiable)
        );
        assert_eq!(
            resolve(RangeSpec::From(1000), 1000),
            Err(RangeError::Unsatisfiable)
        );
        assert_eq!(
            resolve(RangeSpec::Suffix(0), 1000),
            Err(RangeError::Unsatisfiable)
        );
        // Zero-length objects satisfy nothing (FR-15).
        assert_eq!(
            resolve(RangeSpec::FromTo(0, 0), 0),
            Err(RangeError::Unsatisfiable)
        );
        assert_eq!(whole(0), None);
    }

    #[test]
    fn plans_slice_spans() {
        let p = plan(ByteRange { start: 0, end: 999 }, 1024);
        assert_eq!((p.first, p.last, p.count()), (0, 0, 1));

        let p = plan(
            ByteRange {
                start: 0,
                end: 1024,
            },
            1024,
        );
        assert_eq!((p.first, p.last, p.count()), (0, 1, 2));

        // A range starting exactly on a boundary must not pull the preceding slice.
        let p = plan(
            ByteRange {
                start: 1024,
                end: 2047,
            },
            1024,
        );
        assert_eq!((p.first, p.last, p.count()), (1, 1, 1));

        let p = plan(
            ByteRange {
                start: 1023,
                end: 1024,
            },
            1024,
        );
        assert_eq!((p.first, p.last, p.count()), (0, 1, 2));
    }

    #[test]
    fn computes_slice_extents_including_the_short_final_slice() {
        assert_eq!(
            slice_extent(0, 1024, 2500),
            ByteRange {
                start: 0,
                end: 1023
            }
        );
        assert_eq!(
            slice_extent(1, 1024, 2500),
            ByteRange {
                start: 1024,
                end: 2047
            }
        );
        // Final slice is short.
        assert_eq!(
            slice_extent(2, 1024, 2500),
            ByteRange {
                start: 2048,
                end: 2499
            }
        );
        // Exact multiple: final slice is full width.
        assert_eq!(
            slice_extent(1, 1024, 2048),
            ByteRange {
                start: 1024,
                end: 2047
            }
        );
    }

    #[test]
    fn computes_payload_windows() {
        // Entire first slice.
        assert_eq!(
            payload_window(
                0,
                1024,
                ByteRange {
                    start: 0,
                    end: 2047
                }
            ),
            (0, 1024)
        );
        // Tail of the first slice.
        assert_eq!(
            payload_window(
                0,
                1024,
                ByteRange {
                    start: 100,
                    end: 2047
                }
            ),
            (100, 1024)
        );
        // Head of the second slice.
        assert_eq!(
            payload_window(
                1,
                1024,
                ByteRange {
                    start: 0,
                    end: 1100
                }
            ),
            (0, 77)
        );
        // A range wholly inside one slice.
        assert_eq!(
            payload_window(
                1,
                1024,
                ByteRange {
                    start: 1100,
                    end: 1200
                }
            ),
            (76, 177)
        );
    }

    #[test]
    fn payload_windows_tile_the_requested_range_exactly() {
        // The property that matters: concatenating each slice's window reproduces the request
        // byte-for-byte, with no gaps and no overlaps.
        let slice_size = 64u32;
        let total = 1000u64;
        for start in [0u64, 1, 63, 64, 65, 500, 999] {
            for end in [start, start + 1, start + 63, start + 200, 999] {
                if end >= total {
                    continue;
                }
                let wanted = ByteRange { start, end };
                let p = plan(wanted, slice_size);
                let mut covered = 0u64;
                for i in p.indices() {
                    let (from, to) = payload_window(i, slice_size, wanted);
                    assert!(to > from, "empty window for slice {i}");
                    covered += (to - from) as u64;
                }
                assert_eq!(covered, wanted.len(), "range {start}-{end}");
            }
        }
    }
}
