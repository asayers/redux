use crate::{redux_dir, FileStamp};
use anyhow::Context;
use blake3::Hash;
use std::{collections::HashSet, path::PathBuf, sync::LazyLock};
use tracing::debug;

pub static ARTIFACTS_DIR: LazyLock<PathBuf> = LazyLock::new(|| {
    let path = redux_dir().join("artifacts");
    std::fs::create_dir_all(&path).unwrap();
    path
});

/// A cache of the contents of redux_dir/artifacts
pub struct Artifacts(HashSet<Hash>);

impl Artifacts {
    pub fn new() -> anyhow::Result<Artifacts> {
        std::fs::create_dir_all(&*ARTIFACTS_DIR)?;
        let mut xs = HashSet::default();
        for ent in std::fs::read_dir(&*ARTIFACTS_DIR)? {
            let path = ent?.path();
            let fname = path.file_name().unwrap();
            let fname = fname.to_str().unwrap();
            xs.insert(fname.parse().unwrap());
        }
        Ok(Artifacts(xs))
    }

    pub fn store_path(hash: Hash) -> PathBuf {
        ARTIFACTS_DIR.join(hash.to_string())
    }

    pub fn insert(&mut self, file: &FileStamp) -> anyhow::Result<()> {
        if self.0.contains(&file.hash) {
            debug!("{}: contents already in the store", file.path);
        } else {
            let to = Self::store_path(file.hash);
            std::fs::copy(file.path.to_abs(), to)?;
            debug!("{}: contents added to the store", file.path);
            self.0.insert(file.hash);
        }
        Ok(())
    }

    pub fn restore(&self, file: &FileStamp) -> anyhow::Result<()> {
        assert!(self.0.contains(&file.hash));
        let from = Self::store_path(file.hash);
        std::fs::copy(from, file.path.to_abs()).context("Copy artifact")?;
        debug!(
            "{}: Restored contents @{}",
            file.path,
            &file.hash.to_hex()[..8],
        );
        Ok(())
    }
}
