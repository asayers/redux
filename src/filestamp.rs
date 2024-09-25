use crate::local_path::LocalPath;
use anyhow::{anyhow, Context};
use blake3::Hash;
use std::fmt;
use std::path::PathBuf;
use std::str::FromStr;

#[derive(Debug, Hash, PartialEq, Eq, Clone)]
pub struct FileStamp {
    pub path: LocalPath,
    pub hash: Hash,
}

impl fmt::Display for FileStamp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        use yansi::Paint;
        let hash = self.hash.to_hex();
        let hash = f.precision().map(|x| &hash[..x]).unwrap_or(&hash);
        if f.alternate() {
            match self.is_valid() {
                Ok(true) => write!(f, "{}@{}", self.path, hash.green()),
                Ok(false) => write!(f, "{}@{}", self.path, hash.red()),
                Err(_) => write!(f, "{}@{}", self.path.red(), hash),
            }
        } else {
            write!(f, "{}@{}", self.path, hash)
        }
    }
}

impl FromStr for FileStamp {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let (path, hash) = s.split_once('@').ok_or_else(|| anyhow!("No @ sign"))?;
        Ok(FileStamp {
            path: path.parse()?,
            hash: hash.parse()?,
        })
    }
}

impl PartialOrd for FileStamp {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for FileStamp {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        (&self.path, self.hash.as_bytes()).cmp(&(&other.path, other.hash.as_bytes()))
    }
}

impl FileStamp {
    pub fn new(path: LocalPath) -> anyhow::Result<Self> {
        let mut hasher = blake3::Hasher::new();
        hasher
            .update_mmap_rayon(path.to_abs())
            .context(path.to_string())?;
        let hash = hasher.finalize();
        Ok(FileStamp { path, hash })
    }

    pub fn abs_path(&self) -> PathBuf {
        self.path.to_abs()
    }

    pub fn is_valid(&self) -> anyhow::Result<bool> {
        let mut hasher = blake3::Hasher::new();
        hasher.update_mmap_rayon(self.abs_path())?;
        let hash = hasher.finalize();
        Ok(hash == self.hash)
    }
}
