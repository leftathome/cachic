//! Free-space guard.
//!
//! FR-46: reduce the effective disk cap when the filesystem runs low.
//!
//! The cache is usually not alone on its volume, and `CACHE_DISK_SIZE` is what the operator
//! *intended*, not what is actually available. If something else fills the disk, continuing to
//! write until the filesystem returns ENOSPC turns a tuning problem into an outage - and on a
//! copy-on-write filesystem it can turn into an unrecoverable one.
//!
//! So the effective cap is the smaller of what was configured and what the filesystem can still
//! give us while keeping `MIN_FREE_DISK` in reserve.

use std::path::Path;

/// What the filesystem reports.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DiskSpace {
    pub total: u64,
    /// Bytes available to an unprivileged process, which is what matters: reserved blocks are
    /// not ours to use.
    pub available: u64,
}

#[derive(Debug, thiserror::Error)]
#[error("cannot read free space for {path}: {source}")]
pub struct SpaceError {
    pub path: std::path::PathBuf,
    #[source]
    pub source: std::io::Error,
}

/// Read filesystem space for the volume holding `path`.
#[cfg(unix)]
pub fn read(path: &Path) -> Result<DiskSpace, SpaceError> {
    use std::os::unix::ffi::OsStrExt;

    let c_path = std::ffi::CString::new(path.as_os_str().as_bytes()).map_err(|_| SpaceError {
        path: path.to_owned(),
        source: std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "path contains an interior NUL",
        ),
    })?;

    // SAFETY: `c_path` is a valid NUL-terminated string for the duration of the call, and
    // `stat` is a valid, properly aligned output buffer that statvfs fully initialises on
    // success. The return value is checked before the buffer is read.
    let stat = unsafe {
        let mut stat: libc::statvfs = std::mem::zeroed();
        if libc::statvfs(c_path.as_ptr(), &mut stat) != 0 {
            return Err(SpaceError {
                path: path.to_owned(),
                source: std::io::Error::last_os_error(),
            });
        }
        stat
    };

    // `f_frsize` is the fragment size, which is the unit the block counts are expressed in.
    // Using `f_bsize` here is a classic overestimate on filesystems where they differ.
    let unit = if stat.f_frsize > 0 {
        stat.f_frsize as u64
    } else {
        stat.f_bsize as u64
    };
    Ok(DiskSpace {
        total: stat.f_blocks as u64 * unit,
        available: stat.f_bavail as u64 * unit,
    })
}

/// The cap the store should actually enforce.
///
/// `used` is what the cache currently occupies, so the calculation answers "how large may the
/// cache grow" rather than "how much free space is there", which are different once the cache is
/// already large.
pub fn effective_cap(configured: u64, min_free: u64, used: u64, space: DiskSpace) -> u64 {
    // Headroom the cache may still claim without eating into the reserve.
    let claimable = space.available.saturating_sub(min_free);
    // The cache may keep what it already has, plus whatever it can still claim.
    let ceiling = used.saturating_add(claimable);
    configured.min(ceiling)
}

/// Whether the guard is currently reducing the cap.
pub fn is_engaged(configured: u64, effective: u64) -> bool {
    effective < configured
}

#[cfg(test)]
mod tests {
    use super::*;

    const GIB: u64 = 1 << 30;

    fn space(total: u64, available: u64) -> DiskSpace {
        DiskSpace { total, available }
    }

    #[test]
    fn a_roomy_disk_leaves_the_configured_cap_alone() {
        let cap = effective_cap(100 * GIB, 10 * GIB, 20 * GIB, space(500 * GIB, 400 * GIB));
        assert_eq!(cap, 100 * GIB);
        assert!(!is_engaged(100 * GIB, cap));
    }

    #[test]
    fn a_full_disk_reduces_the_cap_to_what_is_already_used() {
        // Something else filled the volume. The cache keeps what it has and grows no further,
        // rather than writing until the filesystem returns ENOSPC.
        let cap = effective_cap(100 * GIB, 10 * GIB, 20 * GIB, space(500 * GIB, 2 * GIB));
        assert_eq!(cap, 20 * GIB);
        assert!(is_engaged(100 * GIB, cap));
    }

    #[test]
    fn the_reserve_is_honoured_rather_than_consumed() {
        // 30 GiB available, 10 GiB reserved, so 20 GiB is claimable on top of the 5 GiB in use.
        let cap = effective_cap(100 * GIB, 10 * GIB, 5 * GIB, space(500 * GIB, 30 * GIB));
        assert_eq!(cap, 25 * GIB);
    }

    #[test]
    fn less_free_space_than_the_reserve_freezes_the_cache_at_its_current_size() {
        let cap = effective_cap(100 * GIB, 10 * GIB, 40 * GIB, space(500 * GIB, 4 * GIB));
        assert_eq!(
            cap,
            40 * GIB,
            "the cache must not be asked to shrink below what it holds"
        );
    }

    #[test]
    fn a_completely_full_disk_does_not_underflow() {
        let cap = effective_cap(100 * GIB, 10 * GIB, 0, space(500 * GIB, 0));
        assert_eq!(cap, 0);
    }

    #[test]
    fn the_guard_never_raises_the_configured_cap() {
        // An enormous disk must not licence exceeding what the operator asked for.
        let cap = effective_cap(10 * GIB, GIB, 0, space(100_000 * GIB, 90_000 * GIB));
        assert_eq!(cap, 10 * GIB);
    }

    #[test]
    fn reads_real_filesystem_space() {
        // The arithmetic above is only useful if the numbers going into it are real.
        let space = read(std::path::Path::new(".")).unwrap();
        assert!(space.total > 0, "filesystem reports no capacity");
        assert!(
            space.available <= space.total,
            "available {} exceeds total {}",
            space.available,
            space.total
        );
    }

    #[test]
    fn a_missing_path_is_an_error_rather_than_a_zero() {
        // Silently reporting zero free space would engage the guard permanently.
        assert!(read(std::path::Path::new("/definitely/not/a/real/path")).is_err());
    }
}
