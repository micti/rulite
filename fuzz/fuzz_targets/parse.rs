//! T6.5: the query-string and header parsers never panic.
//!
//! Input: `<query string>\n<Prefer>\n<Range>\n<Accept>` (missing lines are absent headers).

#![no_main]

use libfuzzer_sys::fuzz_target;

// rulite is a binary crate: the parser module is compiled in directly.
#[allow(dead_code)]
#[path = "../../src/query.rs"]
mod query;

fuzz_target!(|data: &[u8]| {
    let Ok(s) = std::str::from_utf8(data) else { return };
    let mut lines = s.split('\n');
    let raw = lines.next().unwrap_or_default();
    let (prefer, range, accept) = (lines.next(), lines.next(), lines.next());

    let _ = query::parse_query(raw);
    let _ = query::parse_select(raw);
    let _ = query::Headers::parse(prefer, range, accept);
});
