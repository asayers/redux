mod artifacts;
mod depgraph;
mod filestamp;
mod local_path;
mod ruleset;
mod trace;

pub use crate::{
    artifacts::Artifacts,
    depgraph::{DepGraph, TRACES_DIR},
    filestamp::FileStamp,
    local_path::LocalPath,
    ruleset::RuleSet,
    trace::{EnvVar, TraceFile, TraceFileLine},
};

use crate::trace::{JobSpec, Trace};
use anyhow::{anyhow, bail, ensure, Context};
use std::path::Path;
use std::path::PathBuf;
use std::sync::LazyLock;
use tracing::{debug, error, info, warn};
use uuid::Uuid;

// TODO: Thread-local storage?
pub static REPO: LazyLock<gix::ThreadSafeRepository> =
    LazyLock::new(|| gix::discover(".").unwrap().into_sync());

pub fn redux_dir() -> &'static Path {
    static REDUX_DIR: LazyLock<PathBuf> = LazyLock::new(|| {
        let redux_dir = REPO.git_dir().join("redux");
        std::fs::create_dir_all(&redux_dir).unwrap();
        debug!("redux dir = {}", redux_dir.display());
        redux_dir.canonicalize().unwrap()
    });
    &REDUX_DIR
}

/// A tracefile which was created by this process, and which should be moved or
/// deleted before this process exits.
struct JobTmpFiles {
    trace: TraceFile,
    out: PathBuf,
    committed: bool,
}
impl JobTmpFiles {
    /// None means the tracefile already existed
    fn create(job: &JobSpec) -> anyhow::Result<Option<JobTmpFiles>> {
        match TraceFile::create(job.clone())? {
            Some(trace) => {
                let outfile = {
                    let filename = job.target.file_name();
                    let target = job.abs_target();
                    target.with_file_name(format!(".redux_{}.tmp", filename))
                };
                debug!(path = %trace.path.display(), "Prepared tracefile");
                debug!(path = %outfile.display(), "Prepared outfile");
                Ok(Some(JobTmpFiles {
                    trace,
                    out: outfile,
                    committed: false,
                }))
            }
            None => Ok(None),
        }
    }

    fn commit(mut self) -> anyhow::Result<Trace> {
        ensure!(self.out.exists(), "Job produced no output");

        // Move the outfile _before_ moving the tracefile
        let job = &self.trace.job;
        std::fs::rename(&self.out, job.abs_target())?;
        let stamp = FileStamp::new(job.target.clone())?;
        Artifacts::new()?.insert(&stamp)?;

        // Append one more line to the tracefile
        self.trace.finish(stamp)?;

        // Store the trace
        let tracefile_hash = FileStamp::new(self.trace.path.as_path().into())?.hash;
        let new_tracefile = TRACES_DIR.join(format!("{tracefile_hash}.trace"));
        std::fs::rename(&self.trace.path, &new_tracefile)?;
        info!("Tracefile moved to {}", new_tracefile.display());
        let (_, trace) = TraceFile::read(&new_tracefile)?;

        self.committed = true;
        Ok(trace)
    }
}

impl Drop for JobTmpFiles {
    fn drop(&mut self) {
        if !self.committed {
            info!(
                out = %self.out.display(),
                trace = %self.trace.path.display(),
                "Cleaning up",
            );
            // Remove the outfile _before_ removing the tracefile
            let _ = std::fs::remove_file(&self.out); // Might be missing
            if let Err(e) = std::fs::remove_file(&self.trace.path) {
                error!("{}: Failed to clean up: {e}", self.trace.path.display());
            }
        }
    }
}

pub fn build(target: &LocalPath, clean: bool) -> anyhow::Result<()> {
    let rules = RuleSet::scan_for_do_files()?;
    let job = rules
        .job_for(target.clone())
        .ok_or_else(|| anyhow!("{}: No rule matching this path", target))?;
    debug!("Found rule {}", job.rule);
    let tmp_files = loop {
        if !clean {
            // Try to re-use a prior build, if there is one
            let restored = try_restore(&rules, &job)?;
            if restored {
                // The target file has been restored from the artifact store,
                // and we're done!
                return Ok(());
            }
        }
        match JobTmpFiles::create(&job)? {
            Some(x) => break x,
            None => {
                // TODO: inotify
                std::thread::sleep(std::time::Duration::from_secs(1));
                info!("Retrying...");
                // loop
            }
        }
    };
    actually_run(job, tmp_files)?;
    Ok(())
}

pub fn try_restore(rules: &RuleSet, job: &JobSpec) -> anyhow::Result<bool> {
    // Need to reload the dep graph each time
    let dep_graph = DepGraph::load(rules)?;
    let Some(tree) = dep_graph.valid_trace_for(job) else {
        return Ok(false);
    };
    info!(
        "{}: Found an existing trace whose sources are up-to-date",
        job.target
    );
    info!("{tree}");
    let x = tree.outputs.iter().find(|x| x.path == job.target).unwrap();
    Artifacts::new()?.restore(x)?;
    Ok(true)
}

#[derive(Debug, Hash, PartialEq, Eq, Clone, Copy, Default, PartialOrd, Ord)]
pub struct BuildId(pub Uuid);

impl BuildId {
    pub fn new() -> Self {
        BuildId(Uuid::new_v4())
    }

    pub fn current() -> anyhow::Result<BuildId> {
        Ok(Self::current2()?.unwrap_or_else(Self::new))
    }

    pub fn is_current(self) -> bool {
        match Self::current2() {
            Ok(Some(x)) => x == self,
            _ => false,
        }
    }

    fn current2() -> anyhow::Result<Option<BuildId>> {
        match std::env::var(ENV_VAR_BUILD_ID) {
            Ok(x) => Ok(Some(BuildId(x.parse()?))),
            Err(std::env::VarError::NotPresent) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }
}

pub const ENV_VAR_TRACEFILE: &str = "REDUX_TRACEFILE";
pub const ENV_VAR_BUILD_ID: &str = "REDUX_BUILD_ID";
pub const ENV_VAR_FORCE: &str = "REDUX_FORCE";

fn actually_run(job: JobSpec, tmp_files: JobTmpFiles) -> anyhow::Result<Trace> {
    info!("Running rule to build file");
    let cmd = job.rule.to_abs();
    let job_dir = cmd.parent().unwrap();
    let build_id = BuildId::current()?;
    let mut child = std::process::Command::new(&cmd)
        .current_dir(job_dir)
        // the name of the target file
        .arg(job.target_relative_to_rule())
        // the basename of the target, minus the extension, if any
        .arg(job.target_minus_extension())
        // the name of a temporary file that will be renamed to the
        // target filename atomically if your .do file returns a
        // zero (success) exit code
        .arg(&tmp_files.out)
        .env(ENV_VAR_TRACEFILE, &tmp_files.trace.path)
        .env(ENV_VAR_BUILD_ID, build_id.0.to_string())
        .spawn()
        .context(format!(
            "Spawn cmd {} in {}",
            cmd.to_str().unwrap(),
            job_dir.display(),
        ))?;
    let exit_status = child.wait().context("Wait for child")?;
    debug!("Child finished: {exit_status}");
    if exit_status.success() {
        let trace = tmp_files.commit()?;
        info!("Finished build");
        Ok(trace)
    } else if exit_status.code() == Some(102) {
        info!("Looks like the job bailed out early");
        assert!(job.target.exists());
        let (_, partial_trace) = TraceFile::read(&tmp_files.trace.path)?;
        Ok(partial_trace)
    } else {
        bail!("{}: Job failed", job.target);
    }
}

pub fn is_source(path: &LocalPath) -> anyhow::Result<bool> {
    let index = REPO.to_thread_local().index_or_load_from_head().unwrap();
    let path2 = gix::bstr::BStr::new(path.as_path().as_os_str().to_str().unwrap().as_bytes());
    if index.entry_index_by_path(path2).is_ok() {
        debug!("{path}: Checked-in => source");
        return Ok(true);
    }
    let Ok(stamp) = FileStamp::new(path.clone()) else {
        debug!("{path}: Doesn't exist => generated");
        return Ok(false);
    };
    if DepGraph::load_all()?.outputs().any(|x| x == &stamp) {
        debug!("{path}: We generated it => generated");
        Ok(false)
    } else {
        warn!("{path}: Looks like we didn't generate this file; assuming it's a source");
        info!("{path}: Check the file in to silence this warning");
        Ok(true)
    }
}
