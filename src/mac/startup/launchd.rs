//! The jobs launchd starts at login or with the Mac: your own agents, the agents installed for every
//! user, and the daemons, which run as root. Switching one off disables it, so it doesn't start
//! again, and stops it.

use std::{collections::HashMap, path::PathBuf, process::Command};

use anyhow::Result;

use crate::fs::{
    self,
    admin::{self, Step},
    app_bundle, home_dir,
};

/// Where a job's plist is, which says who it runs for.
#[derive(Clone, Copy, PartialEq)]
pub enum Scope {
    /// In your own Library, as you.
    Yours,
    /// In /Library/LaunchAgents, as each user who logs in.
    Everyone,
    /// In /Library/LaunchDaemons, as root.
    System,
}

impl Scope {
    pub const ALL: [Scope; 3] = [Scope::Yours, Scope::Everyone, Scope::System];

    fn dir(self) -> PathBuf {
        match self {
            Scope::Yours => home_dir().join("Library/LaunchAgents"),
            Scope::Everyone => "/Library/LaunchAgents".into(),
            Scope::System => "/Library/LaunchDaemons".into(),
        }
    }

    /// The launchd domain its jobs run in. Agents run in yours, so switching one installed for
    /// everyone switches it for you alone, which needs no password.
    fn domain(self) -> String {
        match self {
            Scope::System => "system".into(),
            _ => format!("gui/{}", unsafe { libc::getuid() }),
        }
    }
}

#[derive(Clone)]
pub struct Job {
    pub label: String,
    pub plist: PathBuf,
    /// The app its program is in, if it's in one.
    pub app: Option<PathBuf>,
    /// What it goes by: its app's name, or its program's.
    pub name: String,
    pub scope: Scope,
    pub on: bool,
}

/// Every job in the folders launchd starts them from, by name.
pub fn jobs() -> Vec<Job> {
    let mut jobs: Vec<Job> = Scope::ALL
        .into_iter()
        .flat_map(|scope| {
            let overrides = overrides(&scope.domain());
            let plists = std::fs::read_dir(scope.dir()).into_iter().flatten().flatten().map(|entry| entry.path());
            plists
                .filter(|path| path.extension().is_some_and(|extension| extension == "plist"))
                .filter_map(move |plist| job(plist, scope, &overrides))
        })
        .collect();
    jobs.sort_by(|a, b| fs::natural_cmp(&a.name, &b.name));
    jobs
}

fn job(plist: PathBuf, scope: Scope, overrides: &HashMap<String, bool>) -> Option<Job> {
    let job = plist::Value::from_file(&plist).ok()?.into_dictionary()?;
    let label = job.get("Label")?.as_string()?.to_owned();
    let program = job.get("Program").or_else(|| job.get("ProgramArguments")?.as_array()?.first());
    let program = program.and_then(plist::Value::as_string).map(PathBuf::from);
    let app = program.as_deref().and_then(app_bundle);
    let name = program.as_deref().and_then(fs::program_name).unwrap_or_else(|| label.clone());
    let disabled = job.get("Disabled").and_then(plist::Value::as_boolean).unwrap_or(false);
    let on = overrides.get(&label).copied().unwrap_or(!disabled);
    Some(Job { label, plist, app, name, scope, on })
}

/// The jobs launchctl was told to switch on or off in `domain`, over what their plists say.
fn overrides(domain: &str) -> HashMap<String, bool> {
    let output = fs::run(Command::new("/bin/launchctl").args(["print-disabled", domain])).unwrap_or_default();
    parse_overrides(&output)
}

/// Lines like `"com.example.agent" => disabled`.
fn parse_overrides(output: &str) -> HashMap<String, bool> {
    output
        .lines()
        .filter_map(|line| {
            let (label, state) = line.trim().split_once(" => ")?;
            let on = match state {
                "enabled" => true,
                "disabled" => false,
                _ => return None,
            };
            Some((label.trim_matches('"').to_owned(), on))
        })
        .collect()
}

/// Switches `job` on or off, for good and right away, asking for an administrator's password for a
/// daemon.
pub fn switch(job: &Job, on: bool) -> Result<()> {
    let domain = job.scope.domain();
    let target = format!("{domain}/{}", job.label);
    let plist = job.plist.to_string_lossy().into_owned();
    let (keep, now) = match on {
        true => (vec!["enable", &target], vec!["bootstrap", &domain, &plist]),
        false => (vec!["disable", &target], vec!["bootout", &target]),
    };
    let launchctl = |args: Vec<&str>| match job.scope {
        Scope::System => admin::run(Step::Launchctl(args.into_iter().map(String::from).collect())),
        _ => fs::run(Command::new("/bin/launchctl").args(args)).map(drop),
    };
    admin::session(|| {
        launchctl(keep)?;
        // Starting or stopping it now fails if it already is, or was.
        _ = launchctl(now);
        Ok(())
    })
}

#[cfg(test)]
mod tests {
    use super::parse_overrides;

    #[test]
    fn reads_what_launchctl_switched() {
        let output =
            "\tdisabled services = {\n\t\t\"com.example.off\" => disabled\n\t\t\"com.example.on\" => enabled\n\t}\n";
        let overrides = parse_overrides(output);
        assert_eq!(overrides.len(), 2);
        assert!(!overrides["com.example.off"]);
        assert!(overrides["com.example.on"]);
    }
}
