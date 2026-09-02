//! Small helpers shared by unit tests.
//!
//! Deliberately not behind `#[cfg(test)]` on the module itself, so integration tests can use it
//! too; it is excluded from release builds by the `cfg` on its declaration in `lib.rs`.

use std::path::{Path, PathBuf};

/// A scratch directory removed on drop.
///
/// Honours `CACHIC_TEST_TMP` so tests can be pointed at native storage; running them against a
/// Windows drive under WSL2 is slow enough to change what the tests measure.
pub struct Scratch(PathBuf);

impl Scratch {
    pub fn new(tag: &str) -> Self {
        let base = std::env::var("CACHIC_TEST_TMP").unwrap_or_else(|_| "/tmp".into());
        let path = Path::new(&base).join(format!(
            "cachic-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&path).expect("create scratch directory");
        Self(path)
    }

    pub fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
