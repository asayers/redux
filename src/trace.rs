use crate::{BuildId, FileStamp, LocalPath, RuleSet, ENV_VAR_TRACEFILE};
use anyhow::{anyhow, bail};
use std::{
    fmt,
    fs::File,
    io::Write,
    path::{Path, PathBuf},
    str::FromStr,
    time::SystemTime,
};
use tracing::{info, warn};

#[derive(Debug, Hash, PartialEq, Eq, Clone, PartialOrd, Ord)]
pub struct JobSpec {
    pub rule: LocalPath,
    pub target: LocalPath,
    pub env: Vec<(String, String)>,
}

impl fmt::Display for JobSpec {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.rule)?;
        write!(f, "({}", self.target)?;
        for (k, v) in &self.env {
            write!(f, ", {k}={v}")?;
        }
        write!(f, ")")?;
        Ok(())
    }
}

impl FromStr for JobSpec {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let (rule, args) = s.split_once('(').ok_or_else(|| anyhow!("No ( char"))?;
        let rule: LocalPath = rule.parse()?;
        let mut args = args.trim_end_matches(')').split(',');
        let target = args.next().unwrap().parse()?;
        let env = args
            .map(|x| {
                let (k, v) = x.split_once('=').unwrap();
                (k.to_owned(), v.to_owned())
            })
            .collect();
        Ok(JobSpec { rule, target, env })
    }
}

impl JobSpec {
    pub fn fancy(&self) -> String {
        use yansi::Paint;
        let rules = RuleSet::scan_for_do_files().unwrap();
        let is_valid = rules.is_job_valid(self);
        let txt = format!("{:.8}", self);
        format!("{}", if is_valid { txt.magenta() } else { txt.red() })
    }

    pub fn abs_target(&self) -> PathBuf {
        self.target.to_abs()
    }

    pub fn target_relative_to_rule(&self) -> PathBuf {
        self.target.relative_to(&self.rule.parent())
    }

    fn rule_extension(&self) -> &str {
        let dofile = self.rule.file_name();
        match dofile.strip_prefix("default") {
            None => "",
            Some(x) => x.strip_suffix(".do").unwrap(),
        }
    }

    /// > In a file called chicken.a.b.c.do that builds a file called
    /// > chicken.a.b.c, $1 and $2 are chicken.a.b.c, and $3 is a temporary name
    /// > like chicken.a.b.c.tmp. You might have expected $2 to be just chicken,
    /// > but that's not possible, because redo doesn't know which portion of the
    /// > filename is the "extension." Is it .c, .b.c, or .a.b.c?
    /// >
    /// > .do files starting with default. are special; they can build any target
    /// > ending with the given extension. So let's say we have a file named
    /// > default.c.do building a file called chicken.a.b.c. $1 is chicken.a.b.c,
    /// > $2 is chicken.a.b, and $3 is a temporary name like chicken.a.b.c.tmp.
    ///
    /// https://redo.readthedocs.io/en/latest/FAQSemantics/#what-are-the-parameters-1-2-3-to-a-do-file
    pub fn target_minus_extension(&self) -> PathBuf {
        let extension = self.rule_extension();
        let target = self.target_relative_to_rule();
        PathBuf::from(target.to_string_lossy().strip_suffix(extension).unwrap())
    }
}

#[derive(Debug, Hash, PartialEq, Eq, Clone, Default)]
pub struct Trace {
    pub env_vars: Vec<EnvVar>,
    pub data: Vec<blake3::Hash>,
    pub sources: Vec<FileStamp>,
    pub intermediates: Vec<FileStamp>,
    pub outputs: Vec<FileStamp>,
    pub valid_for: Option<BuildId>,
    pub valid_until: Option<SystemTime>,
}

impl fmt::Display for Trace {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        use yansi::Paint;
        for x in &self.sources {
            write!(f, "{x:#.8} ")?;
        }
        for x in &self.intermediates {
            write!(f, "{}@{} ", x.path, x.hash.to_hex()[..8].yellow())?;
        }
        write!(f, "=>")?;
        for x in &self.outputs {
            write!(f, " {}@{}", x.path, x.hash.to_hex()[..8].yellow())?;
        }
        for x in &self.env_vars {
            write!(f, "{x} ")?;
        }
        if self.valid_for.is_some() {
            write!(f, " (volatile)")?;
        }
        if let Some(t) = self.valid_until {
            let remaining = SystemTime::now().duration_since(t).unwrap();
            write!(
                f,
                " (cached for another {})",
                humantime::Duration::from(remaining),
            )?;
        }
        Ok(())
    }
}

impl Trace {
    fn merge(&mut self, line: TraceFileLine) {
        match line {
            TraceFileLine::Job(_) => (),
            TraceFileLine::Source(x) => self.sources.push(x),
            TraceFileLine::Generated(x) => self.intermediates.push(x),
            TraceFileLine::Produced(x) => self.outputs.push(x),
            TraceFileLine::EnvVar(x) => self.env_vars.push(x),
            TraceFileLine::Data(x) => self.data.push(x),
            TraceFileLine::ValidFor(x) => self.valid_for = Some(x),
            TraceFileLine::ValidUntil(t) => {
                self.valid_until = match self.valid_until {
                    Some(x) => Some(x.min(t)),
                    None => Some(t),
                }
            }
        }
    }

    fn parse(txt: &str) -> anyhow::Result<Trace> {
        let mut trace = Trace::default();
        for line in txt.lines() {
            match line.parse() {
                Ok(line) => trace.merge(line),
                Err(e) => warn!("{e}"),
            }
        }
        Ok(trace)
    }
}

pub enum TraceFileLine {
    Job(JobSpec),
    /// Needed, but not generated
    Source(FileStamp),
    /// Needed, and generated
    Generated(FileStamp),
    /// The output of the job
    Produced(FileStamp),
    EnvVar(EnvVar),
    Data(blake3::Hash),
    // Data(),
    /// Job was non-deterministic and must be re-run, even if the sources/
    /// intermediates are up-to-date
    ValidFor(BuildId),
    /// Job was non-deterministic and must be re-run, even if the sources/
    /// intermediates are up-to-date
    ValidUntil(SystemTime),
}

impl fmt::Display for TraceFileLine {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            TraceFileLine::Job(x) => write!(f, "job {x}"),
            TraceFileLine::Source(x) => write!(f, "source {x}"),
            TraceFileLine::Generated(x) => write!(f, "generated {x}"),
            TraceFileLine::Produced(x) => write!(f, "produced {x}"),
            TraceFileLine::EnvVar(x) => write!(f, "env_var {x}"),
            TraceFileLine::Data(x) => write!(f, "data {x}"),
            TraceFileLine::ValidFor(x) => write!(f, "valid_for {}", x.0),
            TraceFileLine::ValidUntil(x) => {
                write!(f, "valid_until {}", humantime::Timestamp::from(*x))
            }
        }
    }
}

impl FromStr for TraceFileLine {
    type Err = anyhow::Error;

    fn from_str(line: &str) -> Result<Self, Self::Err> {
        let (x, y) = line.split_once(' ').unwrap_or((line, ""));
        Ok(match x {
            "source" => TraceFileLine::Source(y.parse()?),
            "generated" => TraceFileLine::Generated(y.parse()?),
            "produced" => TraceFileLine::Produced(y.parse()?),
            "env_var" => TraceFileLine::EnvVar(y.parse()?),
            "data" => TraceFileLine::Data(y.parse()?),
            "valid_for" => TraceFileLine::ValidFor(BuildId(y.parse()?)),
            "valid_until" => TraceFileLine::ValidUntil(y.parse::<humantime::Timestamp>()?.into()),
            _ => bail!("Unknown line in tracefile: {}", x),
        })
    }
}

#[derive(Debug, Hash, PartialEq, Eq, Clone, Default, PartialOrd, Ord)]
pub struct EnvVar {
    pub key: String,
    pub val: String,
}

impl fmt::Display for EnvVar {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}={} ", self.key, self.val)
    }
}
impl FromStr for EnvVar {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        s.split_once('=')
            .map(|(key, val)| EnvVar {
                key: key.to_owned(),
                val: val.to_owned(),
            })
            .ok_or_else(|| anyhow!("No '=' sign"))
    }
}

pub struct TraceFile {
    pub path: PathBuf,
    pub job: JobSpec,
}

impl TraceFile {
    /// `None` means the tracefile already exists
    pub fn create(job: JobSpec) -> anyhow::Result<Option<Self>> {
        let path = {
            let filename = job.target.file_name();
            let target = job.abs_target();
            target.with_file_name(format!(".redux_{}.trace", filename))
        };
        std::fs::create_dir_all(path.parent().unwrap())?;
        match File::create_new(&path) {
            Ok(mut f) => {
                // TODO: Lock the file
                writeln!(f, "{}", TraceFileLine::Job(job.clone()))?;
                writeln!(
                    f,
                    "{}",
                    TraceFileLine::Source(FileStamp::new(job.rule.clone())?)
                )?;
                Ok(Some(TraceFile { path, job }))
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                // TODO: Check if the file is locked
                info!("{}: A build job is already in progress", path.display());
                Ok(None)
            }
            Err(e) => bail!("{}: {e}", path.display()),
        }
    }

    pub fn finish(&self, output: FileStamp) -> anyhow::Result<()> {
        TraceFile::append(Some(self), TraceFileLine::Produced(output))
    }

    pub fn read(path: &Path) -> anyhow::Result<(JobSpec, Trace)> {
        let txt = std::fs::read_to_string(path)?;
        let (job, trace) = txt.split_once('\n').unwrap();
        let job = job.trim_start_matches("job ").parse()?;
        let trace = Trace::parse(trace)?;
        Ok((job, trace))
    }

    pub fn current() -> anyhow::Result<Option<TraceFile>> {
        match std::env::var(ENV_VAR_TRACEFILE) {
            Err(std::env::VarError::NotPresent) => Ok(None),
            Ok(path) => Ok(Some(TraceFile::open(path.into())?)),
            Err(e) => Err(e.into()),
        }
    }

    pub fn open(path: PathBuf) -> anyhow::Result<TraceFile> {
        let (job, _) = TraceFile::read(&path)?;
        Ok(TraceFile { path, job })
    }

    pub fn append(tracefile: Option<&Self>, line: TraceFileLine) -> anyhow::Result<()> {
        let txt = line.to_string();
        if let Some(TraceFile { path, job }) = tracefile {
            // Other processes should not be trying to access the tracefile
            // concurrently, but you never know...
            // TODO: Take a lock on the tracefile before writing?
            let mut file = File::options().append(true).open(path)?;
            writeln!(file, "{}", txt)?;
            println!("{}: {}", job.target, txt);
        } else {
            println!("{}", txt);
        }
        Ok(())
    }
}
