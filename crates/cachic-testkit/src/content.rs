//! Deterministic object content.
//!
//! Every byte of every mock object is a pure function of `(seed, offset)`. That means a test can
//! verify any byte range without keeping a reference copy of a multi-gigabyte object, which is
//! what makes the differential tester in TASK-14 affordable.

/// Bytes produced per hash invocation.
const BLOCK: u64 = 8;

const MIX: u64 = 0x9E37_79B9_7F4A_7C15;

#[inline]
fn splitmix64(mut z: u64) -> u64 {
    z = z.wrapping_add(MIX);
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

#[inline]
fn block_at(seed: u64, block_index: u64) -> [u8; BLOCK as usize] {
    splitmix64(seed ^ block_index.wrapping_mul(MIX)).to_le_bytes()
}

/// Fill `buf` with the object's content starting at absolute byte `offset`.
pub fn fill(seed: u64, offset: u64, buf: &mut [u8]) {
    let mut written = 0usize;
    let mut pos = offset;
    while written < buf.len() {
        let block_index = pos / BLOCK;
        let within = (pos % BLOCK) as usize;
        let block = block_at(seed, block_index);
        let n = std::cmp::min(BLOCK as usize - within, buf.len() - written);
        buf[written..written + n].copy_from_slice(&block[within..within + n]);
        written += n;
        pos += n as u64;
    }
}

/// Allocate and return `len` bytes of the object starting at `offset`.
pub fn range(seed: u64, offset: u64, len: usize) -> Vec<u8> {
    let mut v = vec![0u8; len];
    fill(seed, offset, &mut v);
    v
}

/// Derive an object's seed from its name, so a URL alone identifies its content.
pub fn seed_for(name: &str) -> u64 {
    let mut h = 0xcbf2_9ce4_8422_2325u64;
    for b in name.as_bytes() {
        h ^= *b as u64;
        h = h.wrapping_mul(0x0000_0100_0000_01B3);
    }
    h
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn content_is_position_independent() {
        // The whole design rests on this: reading a range directly must equal reading the same
        // span out of a larger buffer. If it does not, every differential test is meaningless.
        let seed = seed_for("obj-a");
        let whole = range(seed, 0, 4096);
        for (offset, len) in [(0u64, 1usize), (1, 7), (7, 9), (63, 65), (1000, 3096)] {
            let direct = range(seed, offset, len);
            let expected = &whole[offset as usize..offset as usize + len];
            assert_eq!(direct, expected, "mismatch at offset {offset} len {len}");
        }
    }

    #[test]
    fn distinct_seeds_produce_distinct_content() {
        let a = range(seed_for("obj-a"), 0, 256);
        let b = range(seed_for("obj-b"), 0, 256);
        assert_ne!(a, b);
    }

    #[test]
    fn fill_matches_range_across_block_boundaries() {
        let seed = seed_for("boundary");
        let mut buf = vec![0u8; 100];
        fill(seed, 3, &mut buf);
        assert_eq!(buf, range(seed, 3, 100));
    }
}
