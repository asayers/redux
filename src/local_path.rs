use crate::REPO;
use std::fmt;
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::sync::LazyLock;
use tracing::debug;

pub fn project_base() -> &'static Path {
    // REPO.work_dir().unwrap()
    static PROJECT_BASE: LazyLock<PathBuf> = LazyLock::new(|| {
        let path = REPO.to_thread_local().worktree().unwrap().base().to_owned();
        let path = path.canonicalize().unwrap();
        debug!("project_base = {}", path.display());
        path
    });
    &PROJECT_BASE
}

/// A path relative to project_base()
#[derive(Debug, Hash, PartialEq, Eq, PartialOrd, Ord, Clone)]
pub struct LocalPath(PathBuf);

impl LocalPath {
    pub fn to_abs(&self) -> PathBuf {
        project_base().join(&self.0)
    }

    pub fn file_name(&self) -> &str {
        self.0.file_name().unwrap().to_str().unwrap()
    }

    pub fn parent(&self) -> LocalPath {
        LocalPath(self.0.parent().unwrap().to_owned())
    }

    pub fn relative_to(&self, other: &LocalPath) -> PathBuf {
        pathdiff::diff_paths(&self.0, &other.0).unwrap()
    }

    pub fn as_path(&self) -> &Path {
        &self.0
    }

    pub fn depth(&self) -> usize {
        self.0.components().count()
    }

    pub fn join(&self, component: &str) -> LocalPath {
        LocalPath(self.0.join(component))
    }

    pub fn exists(&self) -> bool {
        self.to_abs().exists()
    }
}

impl fmt::Display for LocalPath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0.display())
    }
}
impl FromStr for LocalPath {
    type Err = std::convert::Infallible;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        s.parse().map(LocalPath)
    }
}

impl From<&Path> for LocalPath {
    fn from(path: &Path) -> Self {
        let abs = std::env::current_dir().unwrap().join(path);
        let canonical = match abs.canonicalize() {
            Ok(x) => x,
            Err(_) => abs,
        };
        let local = pathdiff::diff_paths(canonical, project_base()).unwrap();
        LocalPath(local)
    }
}
impl From<PathBuf> for LocalPath {
    fn from(path: PathBuf) -> Self {
        Self::from(path.as_path())
    }
}
