//! Shared `--output` format for CLI commands that can print either a human
//! table or raw JSON. Deriving clap's `ValueEnum` lets clap parse and validate
//! the flag straight into this type and generate the `[possible values: …]`
//! help, instead of each command carrying a `["table", "json"]` whitelist and
//! a hand-rolled `== "json"` check.

use clap::{Arg, ValueEnum};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, ValueEnum)]
pub(crate) enum OutputFormat {
    #[default]
    Table,
    Json,
}

impl OutputFormat {
    pub(crate) fn is_json(self) -> bool {
        matches!(self, OutputFormat::Json)
    }
}

/// The standard `-o/--output` argument, defaulting to `table`. Use with
/// `args.get_one::<OutputFormat>("output")` to read the parsed value back.
pub(crate) fn output_arg() -> Arg {
    Arg::new("output")
        .short('o')
        .long("output")
        .help("Output format")
        .value_parser(clap::value_parser!(OutputFormat))
        .default_value("table")
}

/// Read the `--output` value from parsed args, falling back to the default
/// (`table`) when absent.
pub(crate) fn output_format(args: &clap::ArgMatches) -> OutputFormat {
    args.get_one::<OutputFormat>("output")
        .copied()
        .unwrap_or_default()
}
