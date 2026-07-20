//! Strict hand-rolled CLI parsing shared by the dev bins.
//!
//! These bins predate any argument crate and keep bespoke flag shapes (`sweep
//! run --roms A B C` takes a variable number of values per flag), so they are
//! not worth moving to clap. What they must share is the *rejection* rule: an
//! undeclared `--flag` is an error, never a silent no-op. A typo that changes
//! what a long regression sweep measured — without saying so — is the worst
//! failure mode these tools have.
//!
//! [`classify`] is the single source of truth for "is this flag known", used by
//! both entry points:
//!   * [`Cli::parse`] — full parse into positionals/values/switches, for bins
//!     whose flags are strictly `--flag value` or `--flag` (`harness`).
//!   * [`reject_unknown_flags`] — validation only, for bins that keep their own
//!     accessors because a flag may take several values (`sweep`, `movie`).

/// What a declared flag expects after it.
enum Kind {
    /// `--flag value`
    Value,
    /// `--flag`
    Switch,
}

/// Look `a` up in the subcommand's declared flags. `None` = undeclared.
///
/// Only `--`-prefixed tokens are flags: no bin here accepts `--flag=value`, and
/// none accepts a value that itself starts with `--`, so this cannot mistake a
/// value for a flag.
fn classify(a: &str, value_flags: &[&str], switch_flags: &[&str]) -> Option<Kind> {
    if value_flags.contains(&a) {
        Some(Kind::Value)
    } else if switch_flags.contains(&a) {
        Some(Kind::Switch)
    } else {
        None
    }
}

fn unknown(a: &str) -> String {
    format!("unknown flag {a} (try --help)")
}

/// Error on the first undeclared `--flag`, leaving everything else to the
/// caller's own accessors. For bins whose flags are not uniformly
/// `--flag value` — `sweep run --roms A B C` consumes values until the next
/// flag — so a full parse would have to model per-flag arity that only the
/// caller knows.
pub fn reject_unknown_flags(
    args: &[String],
    value_flags: &[&str],
    switch_flags: &[&str],
) -> Result<(), String> {
    for a in args {
        if a.starts_with("--") && classify(a, value_flags, switch_flags).is_none() {
            return Err(unknown(a));
        }
    }
    Ok(())
}

#[derive(Default)]
pub struct Cli {
    pub positionals: Vec<String>,
    /// `--flag value` occurrences in order (flags may repeat, e.g. `--press`).
    pub values: Vec<(String, String)>,
    pub switches: Vec<String>,
}

impl Cli {
    /// Parse `args` against the subcommand's declared flags. An undeclared
    /// `--flag` is an error instead of being silently ignored.
    pub fn parse(
        args: &[String],
        value_flags: &[&str],
        switch_flags: &[&str],
    ) -> Result<Cli, String> {
        let mut cli = Cli::default();
        let mut i = 0;
        while i < args.len() {
            let a = args[i].as_str();
            if a.starts_with("--") {
                match classify(a, value_flags, switch_flags) {
                    Some(Kind::Value) => {
                        let v = args.get(i + 1).ok_or_else(|| format!("{a} requires a value"))?;
                        cli.values.push((a.to_string(), v.clone()));
                        i += 2;
                    }
                    Some(Kind::Switch) => {
                        cli.switches.push(a.to_string());
                        i += 1;
                    }
                    None => return Err(unknown(a)),
                }
            } else {
                cli.positionals.push(a.to_string());
                i += 1;
            }
        }
        Ok(cli)
    }

    /// First occurrence of `--name value`.
    pub fn value(&self, name: &str) -> Option<&str> {
        self.values.iter().find(|(n, _)| n == name).map(|(_, v)| v.as_str())
    }

    pub fn values<'a>(&'a self, name: &'a str) -> impl Iterator<Item = &'a str> {
        self.values.iter().filter(move |(n, _)| n == name).map(|(_, v)| v.as_str())
    }

    pub fn has(&self, name: &str) -> bool {
        self.switches.iter().any(|s| s == name)
    }

    pub fn parsed<T: std::str::FromStr>(&self, name: &str, default: T) -> Result<T, String> {
        match self.value(name) {
            Some(v) => v.parse().map_err(|_| format!("bad {name} {v:?}")),
            None => Ok(default),
        }
    }

    pub fn no_positionals(&self) -> Result<(), String> {
        match self.positionals.first() {
            Some(p) => Err(format!("unexpected argument {p:?} (try --help)")),
            None => Ok(()),
        }
    }
}

/// Comma-separated frame list (`--shots`, `--vram-frames`).
pub fn parse_frame_list<T: std::str::FromStr>(spec: &str) -> Result<Vec<T>, String> {
    spec.split(',')
        .filter(|s| !s.is_empty())
        .map(|s| s.parse().map_err(|_| format!("bad frame index {s:?}")))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn rejects_undeclared_flags() {
        let a = args(["--out", "d", "--frobnicate"].as_slice());
        let err = reject_unknown_flags(&a, &["--out"], &[]).unwrap_err();
        assert!(err.contains("--frobnicate"), "{err}");
    }

    #[test]
    fn a_typo_is_not_silently_ignored() {
        // The whole point: `--stripnames` must not quietly mean "not stripped".
        let a = args(["--stripnames"].as_slice());
        assert!(reject_unknown_flags(&a, &[], &["--strip-names"]).is_err());
    }

    #[test]
    fn accepts_declared_flags_and_free_values() {
        // Values may follow a flag in any number (sweep's `--roms A B C`).
        let a = args(["--roms", "a", "b", "c", "--strip-names"].as_slice());
        assert!(reject_unknown_flags(&a, &["--roms"], &["--strip-names"]).is_ok());
    }

    #[test]
    fn parse_splits_positionals_values_and_switches() {
        let a = args(["rom.gb", "--hw", "dmg", "--detect-only"].as_slice());
        let cli = Cli::parse(&a, &["--hw"], &["--detect-only"]).unwrap();
        assert_eq!(cli.positionals, ["rom.gb"]);
        assert_eq!(cli.value("--hw"), Some("dmg"));
        assert!(cli.has("--detect-only"));
        assert!(!cli.has("--hw"));
    }

    #[test]
    fn parse_errors_on_unknown_and_on_missing_value() {
        assert!(Cli::parse(&args(["--nope"].as_slice()), &[], &[]).is_err());
        assert!(Cli::parse(&args(["--hw"].as_slice()), &["--hw"], &[]).is_err());
    }

    #[test]
    fn repeated_value_flags_are_kept_in_order() {
        let a = args(["--press", "A@1", "--press", "B@2"].as_slice());
        let cli = Cli::parse(&a, &["--press"], &[]).unwrap();
        assert_eq!(cli.values("--press").collect::<Vec<_>>(), ["A@1", "B@2"]);
        assert_eq!(cli.value("--press"), Some("A@1"));
    }

    #[test]
    fn frame_lists_parse_and_reject_junk() {
        assert_eq!(parse_frame_list::<usize>("5,10").unwrap(), [5, 10]);
        assert_eq!(parse_frame_list::<usize>("").unwrap(), Vec::<usize>::new());
        assert!(parse_frame_list::<usize>("5,x").is_err());
    }
}
