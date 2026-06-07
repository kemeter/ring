//! Cross-cutting CLI presentation helpers, shared by the commands:
//! terminal styling, the `--output` format enum, and RFC 7807 error rendering.

pub(crate) mod output;
pub(crate) mod problem_json;
pub(crate) mod style;
