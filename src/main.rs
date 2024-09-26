use anyhow::{anyhow, bail};
use bpaf::{Bpaf, Parser};
use redux::{
    build, is_source, try_restore, Artifacts, BuildId, DepGraph, EnvVar, FileStamp, LocalPath,
    RuleSet, TraceFile, TraceFileLine, ENV_VAR_FORCE,
};
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::time::SystemTime;
use tracing::{error, info, info_span};
use tracing_subscriber::{prelude::*, EnvFilter};

// Make sure the given files are up-to-date
//
// If possible, redux will restore pre-built copies of the requested files.
// If not, the files will be built based on their dofiles.
#[derive(Bpaf)]
struct Opts {
    #[bpaf(external)]
    command: Command,
}

#[derive(Bpaf, Clone)]
enum Command {
    /// Make sure the given files are up-to-date
    ///
    /// If this is a job within a larger build, the files are marked as "needed"
    // NOTE: No #[bpaf(command)] on this one - it's the default
    Build {
        /// Don't re-use any files from the build cache (recursive)
        #[bpaf(short, long)]
        force: bool,
        /// Limit parallelism to this many jobs (uses all cores by default)
        #[bpaf(
            short,
            long,
            argument("NUM"),
            fallback(jobs_fallback()),
            display_fallback
        )]
        jobs: usize,
        #[bpaf(positional("PATH"), some("Need at least one target"))]
        targets: Vec<PathBuf>,
    },
    #[bpaf(command)]
    Gc,
    #[bpaf(command)]
    Watch { target: PathBuf },
    /// Mark the given env var as contributing to the behaviour of the current
    // job
    #[bpaf(command)]
    EnvVar {
        #[bpaf(positional("VAR"), some("Need at least one env var"))]
        vars: Vec<String>,
    },
    /// Mark some data as a dependency of the current job (reads from stdin)
    #[bpaf(command)]
    Stamp,
    /// Mark the currently-running job as volatile
    #[bpaf(command)]
    Volatile {
        cache_for: Option<humantime::Duration>,
    },
    /// Show the dofile which builds a given target (or list all dofiles)
    #[bpaf(command)]
    Whichdo {
        #[bpaf(positional("PATH"))]
        target: Option<PathBuf>,
    },
    /// Show the build tree which resulted in the given file
    #[bpaf(command)]
    Howdid {
        #[bpaf(positional("PATH"))]
        target: PathBuf,
    },
    #[bpaf(command)]
    Depgraph {
        all: bool,
        #[bpaf(positional("PATH"))]
        target: Option<PathBuf>,
    },
    /// List all files in the current tree which have been used as a source
    #[bpaf(command)]
    Sources { all: bool },
    /// List all files in the current tree which were generated by redux
    #[bpaf(command)]
    Outputs { all: bool },
    #[bpaf(command)]
    Clean,
}

fn jobs_fallback() -> usize {
    std::thread::available_parallelism()
        .map(|x| x.into())
        .unwrap_or(1)
}

fn main() -> anyhow::Result<()> {
    let opts = opts().run();
    tracing_subscriber::registry()
        .with(EnvFilter::from_default_env())
        .with(tracing_subscriber::fmt::layer().with_writer(std::io::stderr))
        .init();
    match opts.command {
        Command::Gc => {
            todo!()
        }
        Command::Watch { target } => {
            let fname = target.file_name().unwrap().to_str().unwrap();
            let tracefile = target.with_file_name(format!(".redux_{fname}.trace"));
            loop {
                // TODO: Clear the screen
                // TODO: Recurse
                let (job, trace) = TraceFile::read(&tracefile)?;
                println!("{job} {trace}");
                std::thread::sleep(std::time::Duration::from_secs(1));
            }
        }
        Command::Whichdo { target } => which_do(target.as_deref())?,
        Command::Howdid { target } => how_did(&target)?,
        Command::Depgraph { target, all } => dep_graph(target.as_deref(), all)?,
        Command::Sources { all } => sources(all)?,
        Command::Outputs { all } => outputs(all)?,
        Command::Clean => {
            let dep_graph = DepGraph::load_all()?;
            let outputs: BTreeSet<&LocalPath> = dep_graph.outputs().map(|x| &x.path).collect();
            let mut artifacts = Artifacts::new()?;
            for s in outputs {
                if let Ok(stamp) = FileStamp::new(s.clone()) {
                    artifacts.insert(&stamp)?;
                    std::fs::remove_file(s.to_abs())?;
                    println!(
                        "{}: Removed (available at {})",
                        s,
                        Artifacts::store_path(stamp.hash).display(),
                    );
                }
            }
        }
        Command::Build {
            targets,
            jobs,
            force,
        } => {
            let force = force || std::env::var(ENV_VAR_FORCE).is_ok();
            // TODO: Include the number of logged messages in the tracefile
            // TODO: Warn if sources have been updated since the top-level build
            // was started (possibly restart the whole build?)
            // TODO: systemd-run
            let jobserver = get_jobserver(jobs)?;
            let mut threads = vec![];
            for target in targets {
                let token = jobserver.acquire()?;
                threads.push(std::thread::spawn(move || {
                    let target: LocalPath = target.into();
                    let _g = info_span!("build", %target).entered();
                    let is_source = is_source(&target)?;
                    if !is_source {
                        build(&target, force)?;
                    }
                    let stamp = FileStamp::new(target)?;
                    Artifacts::new()?.insert(&stamp)?;
                    let line = if is_source {
                        TraceFileLine::Source(stamp)
                    } else {
                        TraceFileLine::Generated(stamp)
                    };
                    anyhow::Ok(line)
                }));
                std::mem::drop(token);
            }
            let tracefile = TraceFile::current()?;
            let mut errored = false;
            for th in threads {
                match th.join().unwrap() {
                    Ok(line) => TraceFile::append(tracefile.as_ref(), line)?,
                    Err(e) => {
                        error!("{e}");
                        errored = true;
                    }
                }
            }
            if errored {
                bail!("One of the build jobs failed");
            }
            if !force {
                if let Some(TraceFile { job, .. }) = TraceFile::current()? {
                    let rules = RuleSet::scan_for_do_files()?;
                    let restored = try_restore(&rules, &job)?;
                    if restored {
                        info!("{job}: Looks like we can bail out at this point!");
                        std::process::exit(102);
                    }
                }
            }
        }
        Command::EnvVar { vars } => {
            let tracefile = TraceFile::current()?;
            for key in vars {
                let val = std::env::var(&key)?;
                TraceFile::append(
                    tracefile.as_ref(),
                    TraceFileLine::EnvVar(EnvVar { key, val }),
                )?;
            }
        }
        Command::Stamp => {
            let mut hasher = blake3::Hasher::new();
            std::io::copy(&mut std::io::stdin(), &mut hasher)?;
            let hash = hasher.finalize();
            let tracefile = TraceFile::current()?;
            TraceFile::append(tracefile.as_ref(), TraceFileLine::Data(hash))?;
        }
        Command::Volatile { cache_for } => {
            let tracefile = TraceFile::current()?;
            if let Some(d) = cache_for {
                let t = humantime::Timestamp::from(SystemTime::now() + *d);
                TraceFile::append(tracefile.as_ref(), TraceFileLine::ValidUntil(t.into()))?;
            } else {
                let build_id = BuildId::current()?;
                TraceFile::append(tracefile.as_ref(), TraceFileLine::ValidFor(build_id))?;
            }
        }
    }
    Ok(())
}

fn get_jobserver(jobs: usize) -> anyhow::Result<jobserver::Client> {
    if let Some(client) = unsafe { jobserver::Client::from_env() } {
        return Ok(client);
    }
    let x = jobserver::Client::new(jobs)?;
    let exe = std::env::current_exe()?;
    let args = std::env::args().skip(1).collect::<Vec<_>>();
    let mut cmd = std::process::Command::new(exe);
    cmd.args(args);
    x.configure(&mut cmd);
    let status = cmd.spawn()?.wait()?;
    std::process::exit(status.code().unwrap_or(1));
}

fn which_do(target: Option<&Path>) -> anyhow::Result<()> {
    let rules = RuleSet::scan_for_do_files()?;
    if let Some(target) = target {
        match rules.job_for(target.into()) {
            Some(job) => println!("{}: {}", target.display(), job.rule),
            None => {
                eprintln!("{}: No rule found", target.display());
                std::process::exit(1);
            }
        }
    } else {
        for (glob, do_file) in rules.iter() {
            println!("{}: {}", glob, do_file);
        }
    }
    Ok(())
}

fn how_did(target: &Path) -> anyhow::Result<()> {
    let stamp = FileStamp::new(target.into())?;
    let dep_graph = DepGraph::load_all()?;
    match dep_graph.some_tree_for(&stamp) {
        Some(tree) => println!("{tree}"),
        None => println!("{}: No build tree found", target.display()),
    }
    Ok(())
}

fn dep_graph(target: Option<&Path>, all: bool) -> anyhow::Result<()> {
    let mut dep_graph = DepGraph::load_all()?;
    let rules = RuleSet::scan_for_do_files()?;
    if !all {
        dep_graph.drop_superseded(&rules);
        dep_graph.drop_out_of_date();
    }
    if let Some(target) = target {
        let job = rules
            .job_for(target.into())
            .ok_or_else(|| anyhow!("No rule"))?;
        let tree = dep_graph
            .valid_trace_for(&job)
            .ok_or_else(|| anyhow!("No valid traces found"))?;
        println!("{tree}");
    } else {
        for (j, ts) in dep_graph.traces {
            for t in ts {
                println!("{}: {t}", j.fancy());
            }
        }
    }
    Ok(())
}

fn sources(all: bool) -> anyhow::Result<()> {
    let dep_graph = DepGraph::load_all()?;
    let sources: BTreeSet<&LocalPath> = dep_graph.sources().map(|x| &x.path).collect();
    for s in sources {
        if all || s.exists() {
            println!("{s}");
        }
    }
    Ok(())
}

fn outputs(all: bool) -> anyhow::Result<()> {
    let dep_graph = DepGraph::load_all()?;
    let outputs: BTreeSet<&LocalPath> = dep_graph.outputs().map(|x| &x.path).collect();
    for s in outputs {
        if all || s.exists() {
            println!("{s}");
        }
    }
    Ok(())
}
