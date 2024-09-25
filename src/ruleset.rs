use crate::{local_path::project_base, trace::JobSpec, LocalPath};
use globset::{Glob, GlobSet};
use std::{cmp::Ordering, path::Path};
use tracing::trace;

#[derive(Default)]
pub struct RuleSet {
    rules: Vec<Rule>,         // Indexed by rule ID
    do_files: Vec<LocalPath>, // Indexed by rule ID
    globs: Vec<Glob>,         // Indexed by rule ID
    globset: GlobSet,         // Indexed by rule ID
}

pub struct Rule {
    dir: LocalPath,
    default: bool,
    /// Doesn't include the ".do" extension.
    /// If default: Doesn't include the "default".  Includes the dot.  May be
    ///             empty (ie. "default.do")
    /// Otherwise: The filename.  Non-empty.
    name: String,
}

impl Rule {
    fn new(path: &Path) -> Option<Rule> {
        let stem = path.file_name()?.to_str()?.strip_suffix(".do")?;
        // It's a dofile
        let dir = LocalPath::from(path.parent()?);
        if stem.is_empty() {
            return None; // Invalid
        }
        match stem.strip_prefix("default") {
            Some("") => Some(Rule {
                dir,
                default: true,
                name: "".to_owned(),
            }),
            Some(x) if x.starts_with('.') => Some(Rule {
                dir,
                default: true,
                name: x.to_owned(),
            }),
            _ => Some(Rule {
                dir,
                default: false,
                name: stem.to_owned(),
            }),
        }
    }

    fn to_glob(&self) -> Glob {
        let star = if self.default { "*" } else { "" };
        let slash = if self.dir.depth() == 0 { "" } else { "/" };
        Glob::new(&format!("{}{}**/{}{}", self.dir, slash, star, self.name)).unwrap()
    }

    fn to_path(&self) -> LocalPath {
        let default = if self.default { "default" } else { "" };
        let fname = format!("{}{}.do", default, self.name);
        self.dir.join(&fname)
    }

    /// Compare two rules.  _If_ both rules match a given target, then the rule
    /// with the greater priority should be used.
    ///
    /// For rules which match disjoint sets of targets, the results of this
    /// method are arbitrary.  It could even return `Equal`.
    fn priority(&self, other: &Self) -> Ordering {
        // Deeper rules always trump shallower rules.  That's because, if both
        // rules match, it means the deeper rules lives in a subdirectory of the
        // shallower rule's directory.  Therefore, the deeper rule shadows the
        // shallower rule when they both match.
        let by_dir = self.dir.depth().cmp(&other.dir.depth());
        // The depth is equal, and both rules match.  That means that the rules
        // are in the _same_ directory.  In this case, more specific rules beat
        // more generic ones.
        let by_specificity = self.default.cmp(&other.default).reverse();
        // Long extensions beat short extensions This only applies if both rules
        // are "default" (if they're both specific and in the same dir, then
        // they're actually the same rule).
        let by_extension = self.name.len().cmp(&other.name.len());
        by_dir.then(by_specificity).then(by_extension)
    }
}

impl RuleSet {
    pub fn new(rules: Vec<Rule>) -> Self {
        let mut rules2 = RuleSet {
            rules,
            ..Default::default()
        };
        // Highest priority first
        rules2.rules.sort_by(|x, y| x.priority(y).reverse());
        let mut bldr = GlobSet::builder();
        for rule in &rules2.rules {
            rules2.do_files.push(rule.to_path());
            let glob = rule.to_glob();
            bldr.add(glob.clone());
            rules2.globs.push(glob);
        }
        rules2.globset = bldr.build().unwrap();
        rules2
    }

    pub fn job_for(&self, target: LocalPath) -> Option<JobSpec> {
        trace!("Looking for a rule for {}", target);
        let matches = self.globset.matches(target.as_path());
        let rule_id = *matches.first()?;
        Some(JobSpec {
            rule: self.do_files[rule_id].clone(),
            target,
            env: vec![],
        })
    }

    pub fn is_job_valid(&self, job: &JobSpec) -> bool {
        self.job_for(job.target.clone()).as_ref() == Some(job)
    }

    // TODO: Add a variant which scans a tree in the local git repo, instead of the working tree
    pub fn scan_for_do_files() -> anyhow::Result<RuleSet> {
        let mut rules = vec![];
        for ent in walkdir::WalkDir::new(project_base()) {
            let ent = ent?;
            let path = ent.path();
            let Some(rule) = Rule::new(path) else {
                continue;
            };
            rules.push(rule);
        }
        Ok(RuleSet::new(rules))
    }

    pub fn iter(&self) -> impl Iterator<Item = (&Glob, &LocalPath)> + '_ {
        self.globs.iter().zip(&self.do_files)
    }
}
