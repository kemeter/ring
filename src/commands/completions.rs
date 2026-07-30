//! `ring completions <shell>` — print a shell completion script on stdout.
//!
//! The script is generated from the very same `Command` tree the CLI is parsed
//! with (see `crate::build_cli`), so completions can never drift from the real
//! commands and flags: adding a subcommand anywhere makes it completable with
//! no extra work here.
//!
//! Completion covers command names and flags only. Dynamic values (deployment
//! ids, namespaces, …) would require querying the API from inside the shell's
//! completion hook, which is deliberately out of scope.

use clap::{Arg, ArgMatches, Command};
use clap_complete::{Shell, generate};
use std::io;

pub(crate) fn command_config() -> Command {
    Command::new("completions")
        .about("Print a shell completion script")
        .long_about(
            "Print a shell completion script on stdout.\n\n\
             Bash:\n  \
               ring completions bash > /etc/bash_completion.d/ring\n\n\
             Zsh (the directory must be on your $fpath):\n  \
               ring completions zsh > ~/.zsh/completions/_ring\n\n\
             Fish:\n  \
               ring completions fish > ~/.config/fish/completions/ring.fish",
        )
        .arg(
            Arg::new("shell")
                .required(true)
                .value_parser(SUPPORTED_SHELLS)
                .help("Shell to generate the script for"),
        )
}

/// Shells we generate for. Narrower than clap_complete's own `Shell` enum (it
/// also knows elvish and powershell): these three are the ones Ring documents
/// and can reasonably support on the Linux/macOS hosts it targets. Listing them
/// explicitly keeps `--help`, the error message and the docs in agreement.
const SUPPORTED_SHELLS: [&str; 3] = ["bash", "zsh", "fish"];

/// Write the completion script for the requested shell to stdout.
///
/// Takes the CLI tree by value because `generate` needs `&mut Command` (clap
/// builds the help/version args lazily on first use).
pub(crate) fn execute(sub_matches: &ArgMatches, mut cli: Command) {
    // `shell` is `required(true)` and constrained to `SUPPORTED_SHELLS`, so
    // clap has already rejected a missing or unknown value before we get here.
    let shell = sub_matches
        .get_one::<String>("shell")
        .and_then(|name| name.parse::<Shell>().ok())
        .expect("shell is required and validated against SUPPORTED_SHELLS");

    generate(shell, &mut cli, "ring", &mut io::stdout());
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Generate into a buffer rather than stdout, so tests can assert on the
    /// script's content.
    fn script_for(shell: Shell) -> String {
        let mut cli = crate::build_cli();
        let mut buf = Vec::new();
        generate(shell, &mut cli, "ring", &mut buf);
        String::from_utf8(buf).expect("completion scripts are valid UTF-8")
    }

    #[test]
    fn every_supported_shell_generates_a_non_empty_script() {
        for shell in [Shell::Bash, Shell::Zsh, Shell::Fish] {
            assert!(
                !script_for(shell).trim().is_empty(),
                "{shell} script is empty"
            );
        }
    }

    /// The whole point of generating from `build_cli`: top-level commands are
    /// completable. Guards against the tree being rebuilt or trimmed by mistake.
    #[test]
    fn scripts_mention_top_level_subcommands() {
        for shell in [Shell::Bash, Shell::Zsh, Shell::Fish] {
            let script = script_for(shell);
            for command in ["deployment", "namespace", "secret", "server", "completions"] {
                assert!(
                    script.contains(command),
                    "{shell} script is missing the `{command}` command"
                );
            }
        }
    }

    /// Nested subcommands and their flags must be completable too, not just the
    /// top level.
    #[test]
    fn scripts_cover_nested_subcommands_and_flags() {
        let script = script_for(Shell::Bash);
        assert!(script.contains("health-checks"));
        assert!(script.contains("--follow"));
    }

    /// Every advertised shell must actually parse into a `Shell`, otherwise
    /// `execute` would panic on a value clap had accepted.
    #[test]
    fn supported_shells_all_parse() {
        for name in SUPPORTED_SHELLS {
            assert!(
                name.parse::<Shell>().is_ok(),
                "`{name}` is advertised but not a valid clap_complete shell"
            );
        }
    }

    /// Values outside the advertised list are rejected by clap rather than
    /// reaching `execute`. `powershell` is a real `Shell` variant we choose not
    /// to support, so it guards the narrowing itself, not just a typo.
    #[test]
    fn unsupported_shells_are_rejected() {
        for name in ["powershell", "elvish", "nushell"] {
            let result = command_config().try_get_matches_from(["completions", name]);
            assert!(result.is_err(), "`{name}` should have been rejected");
        }
    }
}
