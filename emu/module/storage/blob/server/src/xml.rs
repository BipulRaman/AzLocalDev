//! Minimal escaping helper for hand-built Azure Blob REST API XML responses (`List
//! Containers`/`List Blobs`). No XML crate dependency - the schema this emulator needs to
//! produce is small and fixed, so a couple of `format!` templates plus this escape function
//! are enough, and it keeps the dependency footprint the same as the rest of the project.

pub fn escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            _ => out.push(c),
        }
    }
    out
}
