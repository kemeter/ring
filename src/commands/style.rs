//! Semantic colours for CLI output.
//!
//! One decision, made once: colour is on only when stdout is a real
//! terminal *and* `NO_COLOR` is unset (the https://no-color.org
//! convention). Piped output, files, CI and `--output json` therefore
//! never get ANSI escapes — scripts that parse Ring's output keep
//! working byte-for-byte.
//!
//! Helpers return owned `String`s already wrapped (or not) in escapes, so
//! callers just `eprintln!("{}", error(msg))` without thinking about it.

use owo_colors::OwoColorize;
use std::io::IsTerminal;
use std::sync::OnceLock;

/// Resolved once per process: is colour allowed on stdout?
fn colour_enabled() -> bool {
    static ENABLED: OnceLock<bool> = OnceLock::new();
    *ENABLED.get_or_init(|| {
        // NO_COLOR takes precedence: any non-empty value disables colour.
        if std::env::var_os("NO_COLOR").is_some_and(|v| !v.is_empty()) {
            return false;
        }
        std::io::stdout().is_terminal()
    })
}

/// Apply `body` only when colour is enabled, otherwise return `plain`
/// untouched. The `enabled` flag is passed in (not read here) so unit
/// tests can drive both branches deterministically without a tty or
/// mutating the process environment.
fn paint(enabled: bool, body: impl FnOnce() -> String, plain: &str) -> String {
    if enabled { body() } else { plain.to_string() }
}

/// Red — a failure the user must act on.
pub(crate) fn error(msg: &str) -> String {
    paint(colour_enabled(), || msg.red().to_string(), msg)
}

/// Yellow — a warning; the command continued.
#[allow(dead_code)] // Completes the semantic palette (error/warn/success); used as the CLI grows.
pub(crate) fn warn(msg: &str) -> String {
    paint(colour_enabled(), || msg.yellow().to_string(), msg)
}

/// Green — the operation succeeded.
pub(crate) fn success(msg: &str) -> String {
    paint(colour_enabled(), || msg.green().to_string(), msg)
}

/// Print a composed error line to stderr, coloured red when colour is on.
/// Centralises the one place messages from `http_error` / `transport_error`
/// reach the terminal, so those helpers stay pure (their unit tests assert
/// plain text) and callers don't each repeat the colouring.
pub(crate) fn print_error(msg: &str) {
    eprintln!("{}", error(msg));
}

/// Print a success line to stdout, coloured green when colour is on.
pub(crate) fn print_success(msg: &str) {
    println!("{}", success(msg));
}

/// Trim a timestamp down to second precision for table display.
///
/// The API returns chrono's `DateTime<Utc>` `Display` form —
/// `2026-05-03 22:22:21.595408437 UTC`. The sub-second digits and the
/// `UTC` suffix are noise in a list: every Ring timestamp is UTC, so we
/// say it once in the column header instead. Returns `2026-05-03
/// 22:22:21`.
///
/// Anything we can't parse is returned untouched — a surprising date
/// shape stays visible to the user rather than being silently blanked.
pub(crate) fn format_date(raw: &str) -> String {
    if raw.is_empty() {
        return String::new();
    }
    // chrono's `parse_from_str` won't accept the `UTC` zone *name* (it
    // wants a numeric offset), so strip the suffix ourselves and parse
    // the naive datetime. Every Ring timestamp is UTC by construction.
    let trimmed = raw.trim_end_matches(" UTC");
    match chrono::NaiveDateTime::parse_from_str(trimmed, "%Y-%m-%d %H:%M:%S%.f") {
        Ok(dt) => dt.format("%Y-%m-%d %H:%M:%S").to_string(),
        Err(_) => raw.to_string(),
    }
}

/// Colour a deployment status string by its meaning. Unknown values are
/// returned uncoloured rather than guessed at.
pub(crate) fn status(s: &str) -> String {
    colour_status(colour_enabled(), s)
}

/// Inner form of [`status`] with the enable flag injected, for tests.
fn colour_status(enabled: bool, s: &str) -> String {
    match s {
        "running" | "completed" => paint(enabled, || s.green().to_string(), s),
        "pending" | "starting" | "deleted" => paint(enabled, || s.yellow().to_string(), s),
        "error"
        | "failed"
        | "crash_loop_back_off"
        | "image_pull_back_off"
        | "create_container_error"
        | "network_error"
        | "config_error"
        | "file_system_error"
        | "insufficient_resources" => paint(enabled, || s.red().to_string(), s),
        _ => s.to_string(),
    }
}

/// Strip every ANSI escape sequence (CSI `ESC [ ... <final>`) from `s`.
///
/// `cli-table` unconditionally wraps its borders in colour resets
/// (`ESC[0m`), even with no styling and even into a pipe. That pollutes
/// any `... | grep` / `| jq` and breaks the script-safety contract. We
/// render the table to a string and run it through this when colour is
/// off, so non-tty output is byte-clean.
fn strip_ansi(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = String::with_capacity(s.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == 0x1b && i + 1 < bytes.len() && bytes[i + 1] == b'[' {
            // Skip until the final byte of the CSI sequence (0x40..=0x7e).
            i += 2;
            while i < bytes.len() && !(0x40..=0x7e).contains(&bytes[i]) {
                i += 1;
            }
            i += 1; // consume the final byte too
        } else {
            // Copy this UTF-8 scalar whole (escapes are ASCII, so any
            // multi-byte char here is ordinary text).
            let ch_len = utf8_len(bytes[i]);
            out.push_str(&s[i..i + ch_len]);
            i += ch_len;
        }
    }
    out
}

fn utf8_len(first: u8) -> usize {
    match first {
        b if b < 0x80 => 1,
        b if b >> 5 == 0b110 => 2,
        b if b >> 4 == 0b1110 => 3,
        _ => 4,
    }
}

/// Render a `cli-table` table and print it to stdout, stripping ANSI when
/// colour is off so piped/CI output is byte-clean. Replaces direct
/// `cli_table::print_stdout`, which leaks resets into pipes.
pub(crate) fn print_table<T: cli_table::Table>(table: T) {
    let rendered = match table.table().display() {
        Ok(d) => d.to_string(),
        Err(_) => return, // a table that won't render: print nothing
    };
    if colour_enabled() {
        print!("{rendered}");
    } else {
        print!("{}", strip_ansi(&rendered));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn disabled_yields_plain_text_byte_for_byte() {
        // The script-safety contract: no ANSI when colour is off.
        assert_eq!(paint(false, || "x".red().to_string(), "x"), "x");
        assert_eq!(paint(false, || "ok".green().to_string(), "ok"), "ok");
    }

    #[test]
    fn enabled_wraps_in_escapes() {
        let painted = paint(true, || "x".red().to_string(), "x");
        assert_ne!(painted, "x");
        assert!(
            painted.contains("\x1b["),
            "expected ANSI escape, got {painted:?}"
        );
        assert!(painted.contains('x'));
    }

    #[test]
    fn strip_ansi_removes_csi_sequences_keeps_text() {
        // The exact shape cli-table emits: bordered cells wrapped in resets.
        let dirty = "\x1b[0m+\x1b[0m\x1b[0m---\x1b[0m\x1b[32mrunning\x1b[0m";
        assert_eq!(strip_ansi(dirty), "+---running");
        // No escapes in, identical out.
        assert_eq!(strip_ansi("plain text 123"), "plain text 123");
        // Multi-byte UTF-8 around escapes survives intact.
        assert_eq!(strip_ansi("caf\u{e9}\x1b[0m\u{2014}"), "caf\u{e9}\u{2014}");
    }

    #[test]
    fn status_unknown_is_passed_through_even_when_enabled() {
        // We don't invent a colour for a status we have no meaning for.
        assert_eq!(colour_status(true, "weird"), "weird");
    }

    #[test]
    fn format_date_trims_subseconds_and_zone() {
        // The exact shape the API hands us.
        assert_eq!(
            format_date("2026-05-03 22:22:21.595408437 UTC"),
            "2026-05-03 22:22:21"
        );
        // No sub-second digits, still has the zone name.
        assert_eq!(
            format_date("2026-05-03 22:22:21 UTC"),
            "2026-05-03 22:22:21"
        );
    }

    #[test]
    fn format_date_empty_stays_empty() {
        // An absent updated_at must render as a blank cell, not "now" or junk.
        assert_eq!(format_date(""), "");
    }

    #[test]
    fn format_date_unparseable_is_returned_verbatim() {
        // A surprising shape stays visible rather than being blanked —
        // the user should see that something is off, not a silent gap.
        assert_eq!(format_date("not a date"), "not a date");
        assert_eq!(format_date("2026-05-03"), "2026-05-03");
    }

    #[test]
    fn status_maps_known_values_when_enabled() {
        assert!(colour_status(true, "running").contains("\x1b["));
        assert!(colour_status(true, "failed").contains("\x1b["));
        // Disabled: identical bytes regardless of the status meaning.
        assert_eq!(colour_status(false, "running"), "running");
        assert_eq!(colour_status(false, "failed"), "failed");
    }
}
