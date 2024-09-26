use crate::{
    redux_dir,
    trace::{JobSpec, Trace, TraceFile},
    FileStamp, RuleSet,
};
use std::{
    collections::{BTreeMap, BTreeSet, HashSet},
    fmt,
    path::PathBuf,
    sync::LazyLock,
    time::SystemTime,
};
use tracing::debug;
use yansi::Paint;

pub static TRACES_DIR: LazyLock<PathBuf> = LazyLock::new(|| {
    let path = redux_dir().join("traces");
    std::fs::create_dir_all(&path).unwrap();
    path
});

// TODO: It's really a DAG
// TODO: BTreeMap<JobSpec, Trace>?
#[derive(Clone)]
pub struct BuildTree {
    pub job: JobSpec,
    pub sources: Vec<FileStamp>,
    pub intermediates: Vec<(FileStamp, BuildTree)>,
    pub outputs: Vec<FileStamp>,
    pub valid_until: Option<SystemTime>,
}

impl fmt::Display for BuildTree {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut printed_jobs = BTreeSet::<JobSpec>::default();
        fn to_tt(tree: &BuildTree, printed_jobs: &mut BTreeSet<JobSpec>) -> termtree::Tree<String> {
            let relevant_output = &tree.outputs[0]; // FIXME
            let mut tt = termtree::Tree::new(format!(
                "{}@{} <= {}{}",
                relevant_output.path,
                relevant_output.hash.to_hex()[..8].yellow(),
                tree.job.fancy(),
                if let Some(t) = tree.valid_until {
                    let remaining = t.duration_since(SystemTime::now()).unwrap();
                    let remaining = humantime::Duration::from(remaining).to_string();
                    let remaining = remaining.split(" ").next().unwrap();
                    format!(" (cached for another {})", remaining)
                } else {
                    "".to_owned()
                }
            ));
            let first_time = printed_jobs.insert(tree.job.clone());
            if first_time {
                for x in &tree.sources {
                    tt.push(format!("{:#.8}", x));
                }
                for (_, job) in &tree.intermediates {
                    tt.push(to_tt(job, printed_jobs));
                }
            } else {
                tt.root.push_str(" (see above)");
            }
            tt
        }
        to_tt(self, &mut printed_jobs).fmt(f)
    }
}

#[derive(Debug, Default)]
pub struct DepGraph {
    pub traces: BTreeMap<JobSpec, HashSet<Trace>>,
}

impl DepGraph {
    pub fn load_all() -> anyhow::Result<Self> {
        let mut traces: BTreeMap<JobSpec, HashSet<Trace>> = BTreeMap::default();
        for dent in std::fs::read_dir(&*TRACES_DIR)? {
            let (job, trace) = TraceFile::read(&dent?.path())?;
            traces.entry(job).or_default().insert(trace);
        }
        let graph = DepGraph { traces };
        debug!(
            "Loaded {} traces from {}",
            graph.len(),
            TRACES_DIR.display()
        );
        Ok(graph)
    }

    pub fn load(ruleset: &RuleSet) -> anyhow::Result<Self> {
        let mut graph = Self::load_all()?;
        graph.drop_superseded(ruleset);
        Ok(graph)
    }

    /// Drop if the rule has been overridden by a new, higher-priority rule
    pub fn drop_superseded(&mut self, ruleset: &RuleSet) {
        let n = self.len();
        self.traces.retain(|j, _| ruleset.is_job_valid(j));
        debug!(
            "Dropped {} trace(s) produced by superseded rules",
            n - self.len()
        );
    }

    pub fn drop_out_of_date(&mut self) {
        let n = self.len();
        self.traces.retain(|_, ts| {
            ts.retain(|t| {
                // Drop if any of the sources are out-of-date
                t.sources.iter().all(|s| s.is_valid().unwrap())
            });
            // Drop if all traces are gone
            !ts.is_empty()
        });
        // TODO: Remove all out-of-date traces, recursively
        // loop {
        //     for (_, ts) in &mut x.traces {
        //         ts.retain(|t| t.intermediates.all(|x| todo!()))
        //     }
        // }
        debug!("Dropped {} out-of-date trace(s)", n - self.len());
    }

    /// The number of traces
    fn len(&self) -> usize {
        self.traces.values().map(|x| x.len()).sum()
    }

    pub fn some_tree_for(&self, target: &FileStamp) -> Option<BuildTree> {
        let (job, trace) = self.runs_producing(target).next()?;
        let mut tree = BuildTree {
            job: job.clone(),
            sources: trace.sources.clone(),
            intermediates: Vec::with_capacity(trace.intermediates.len()), // We'll fill this in next
            outputs: trace.outputs.clone(),
            valid_until: trace.valid_until,
        };
        for x in &trace.intermediates {
            if let Some(witness) = self.some_tree_for(x) {
                tree.intermediates.push((x.clone(), witness));
            }
        }
        Some(tree)
    }

    // TODO: Avoid checking the same trace multiple times
    // TODO: Protect against stack overflows
    fn is_trace_valid(&self, job: &JobSpec, trace: &Trace) -> Option<BuildTree> {
        if trace.valid_until.is_some_and(|t| t < SystemTime::now()) {
            return None;
        }
        if let Some(id) = trace.valid_for {
            if !id.is_current() {
                return None;
            }
        }
        if !trace.sources.iter().all(|x| x.is_valid().unwrap_or(false)) {
            return None;
        }
        let mut tree = BuildTree {
            job: job.clone(),
            sources: trace.sources.clone(),
            intermediates: Vec::with_capacity(trace.intermediates.len()), // We'll fill this in next
            outputs: trace.outputs.clone(),
            valid_until: trace.valid_until,
        };
        for x in &trace.intermediates {
            let witness = self
                .runs_producing(x)
                .find_map(|(job, trace)| self.is_trace_valid(job, trace))?;
            tree.intermediates.push((x.clone(), witness));
        }
        Some(tree)
    }

    pub fn valid_trace_for(&self, job: &JobSpec) -> Option<BuildTree> {
        self.traces
            .get(job)
            .into_iter()
            .flatten()
            .find_map(|t| self.is_trace_valid(job, t))
    }

    // TODO: We could just use the ruleset and jump to the relevant job
    fn runs_producing<'a>(
        &'a self,
        file: &'a FileStamp,
    ) -> impl Iterator<Item = (&'a JobSpec, &'a Trace)> + 'a {
        self.all_traces()
            .filter(move |(_, t)| t.outputs.iter().any(|x| x == file))
    }

    fn all_traces(&self) -> impl Iterator<Item = (&JobSpec, &Trace)> {
        self.traces
            .iter()
            .flat_map(|(job, ts)| ts.iter().map(move |t| (job, t)))
    }

    /// May contain duplicates
    pub fn sources(&self) -> impl Iterator<Item = &FileStamp> {
        self.all_traces().flat_map(|x| &x.1.sources)
    }

    /// May contain duplicates
    pub fn outputs(&self) -> impl Iterator<Item = &FileStamp> {
        self.all_traces().flat_map(|x| &x.1.outputs)
    }
}
