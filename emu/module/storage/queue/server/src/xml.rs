//! Minimal escaping/unescaping helpers for hand-built Azure Queue REST API XML request/
//! response bodies. Same philosophy as `emu-storage-blob-server`'s `xml` module: no XML
//! crate dependency, since the schema this emulator needs is small and fixed.

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

fn unescape(s: &str) -> String {
    s.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
        .replace("&amp;", "&")
}

/// Extracts the text between `<MessageText>...</MessageText>` from a `Put Message` request
/// body (`<QueueMessage><MessageText>...</MessageText></QueueMessage>`), unescaped. Returns
/// `None` if the tag isn't found (a malformed request body).
pub fn extract_message_text(body: &str) -> Option<String> {
    let start_tag = "<MessageText>";
    let end_tag = "</MessageText>";
    let start = body.find(start_tag)? + start_tag.len();
    let end = body[start..].find(end_tag)? + start;
    Some(unescape(&body[start..end]))
}
