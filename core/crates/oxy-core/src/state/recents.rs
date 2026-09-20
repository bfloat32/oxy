//! Recents: the last queries that led somewhere, newest first, offered when
//! the box is empty.

/// The last queries that led somewhere, newest first, offered when the box is
/// empty. What is kept is the text, never the row it matched: a query is a
/// question and its answer changes.
pub(super) const RECENTS_LIMIT: usize = 20;

/// A bare `file:` is a mode you are entering, not a search you made.
fn worth_keeping(text: &str) -> bool {
    let entry = text.trim();
    if entry.is_empty() {
        return false;
    }
    // `keyword:` alone
    let bytes = entry.as_bytes();
    if bytes.len() > 1
        && bytes[bytes.len() - 1] == b':'
        && entry[..entry.len() - 1].chars().enumerate().all(|(i, c)| {
            if i == 0 {
                c.is_ascii_alphabetic()
            } else {
                c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '-'
            }
        })
    {
        return false;
    }
    !matches!(entry, "=" | ">" | "?" | "/")
}

pub fn recents_record(list: &mut Vec<String>, text: &str) {
    if !worth_keeping(text) {
        return;
    }
    let entry = text.trim().to_string();
    if list.first() == Some(&entry) {
        return;
    }
    list.retain(|e| *e != entry);
    list.insert(0, entry);
    list.truncate(RECENTS_LIMIT);
}
