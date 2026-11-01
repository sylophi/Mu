//! The software updates waiting for this Mac, from `softwareupdate`, which asks Apple and can take a
//! while.

use std::process::Command;

use anyhow::Result;

#[derive(Debug, PartialEq)]
pub struct Update {
    pub title: String,
    /// In bytes.
    pub size: Option<u64>,
}

/// Where System Settings installs them.
pub const SETTINGS: &str = "x-apple.systempreferences:com.apple.Software-Update-Settings.extension";

pub fn pending() -> Result<Vec<Update>> {
    let output = crate::fs::run(Command::new("/usr/sbin/softwareupdate").arg("--list"))?;
    Ok(parse(&output))
}

/// Each update is a line like `Title: macOS 27.2, Version: 27.2, Size: 6067473KiB, Recommended: YES,`.
fn parse(output: &str) -> Vec<Update> {
    output
        .lines()
        .filter_map(|line| line.trim().strip_prefix("Title: "))
        .map(|line| {
            let mut fields = line.split(", ");
            let title = fields.next().unwrap_or_default().to_owned();
            let size = fields
                .find_map(|field| field.strip_prefix("Size: "))
                .and_then(|size| size.trim_end_matches(',').strip_suffix("KiB")?.parse::<u64>().ok())
                .map(|kib| kib * 1024);
            Update { title, size }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::{Update, parse};

    #[test]
    fn reads_what_softwareupdate_lists() {
        let output = "Software Update Tool

Finding available software
Software Update found the following new or updated software:
* Label: Command Line Tools for Xcode 27.2-27.2
\tTitle: Command Line Tools for Xcode 27.2, Version: 27.2, Size: 521120KiB, Recommended: YES,
* Label: macOS 27.2-26B123
\tTitle: macOS 27.2, Version: 27.2, Size: 6067473KiB, Recommended: YES, Action: restart,
";
        assert_eq!(
            parse(output),
            [
                Update { title: "Command Line Tools for Xcode 27.2".into(), size: Some(521120 * 1024) },
                Update { title: "macOS 27.2".into(), size: Some(6067473 * 1024) },
            ]
        );
        assert_eq!(parse("Software Update Tool\n\nFinding available software\n"), []);
    }
}
