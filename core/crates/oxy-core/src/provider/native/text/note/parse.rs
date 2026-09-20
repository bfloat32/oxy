//! The text half of `note:` — the pieces of `bin/oxy-note` that turn a note
//! file and a query into what the rows show, as pure functions over strings.
//!
//! A note is one `.md` file under the notes directory: the first `# ` line
//! is its title (the filename when it has none) and everything else is the
//! body. Each function names the pipeline it ports.

use crate::provider::native::time::days;

/// The script's `ROW_LIMIT` — comfortably more than the tallest card can
/// draw, so the view still decides where the list ends.
pub const ROW_LIMIT: usize = 24;

/// The longest a first line may be before it stops being a title. Only
/// `--save` cuts at this, which stays in the script — kept here for the
/// comment that would otherwise go looking for it.
#[allow(dead_code)]
pub const TITLE_MAX: usize = 72;

/// `slug` — lowercase, every run of what is not `[a-z0-9]` becomes one dash,
/// the edge dashes come off, sixty characters is the name, and the cut gets
/// its own trailing-dash pass because it can slice a word off its dash.
pub fn slug(text: &str) -> String {
    let mut out = String::new();
    for c in text.to_lowercase().chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c);
        } else if !out.is_empty() && !out.ends_with('-') {
            out.push('-');
        }
    }
    let cut: String = out.chars().take(60).collect();
    cut.trim_end_matches('-').to_string()
}

/// `^#\+ ` — a heading is one or more `#` and then a literal space. `#title`
/// is not one, and neither is a tab where the space goes.
fn is_heading(line: &str) -> bool {
    let hashes = line.bytes().take_while(|&b| b == b'#').count();
    hashes > 0 && line.as_bytes().get(hashes) == Some(&b' ')
}

/// `sed 's/^#\+ *//'` — the hashes and the spaces after them, and nothing
/// more: a tab in front of the title text is the note's own business.
fn heading_text(line: &str) -> &str {
    let hashes = line.bytes().take_while(|&b| b == b'#').count();
    line[hashes..].trim_start_matches(' ')
}

/// `head -c 2000`, kept on a character boundary — the script could cut a
/// multibyte character in half and hand the broken bytes to jq; here the
/// excerpt just ends one character earlier.
fn head_bytes(s: &str, n: usize) -> &str {
    if s.len() <= n {
        return s;
    }
    let mut end = n;
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

/// What one note file contributes to its row.
pub struct Parsed {
    /// The first `# ` line's text, or the file's basename when the file has
    /// no heading — or the heading has no words.
    pub title: String,
    /// Blank lines squeezed out and capped at 2000 bytes — what the card
    /// excerpts and what a query is matched against.
    pub body: String,
    /// `wc -w` over the body before the squeeze — the count is the same with
    /// or without the blank lines, so one pass does both.
    pub words: usize,
}

/// The script's three reads of the file in one pass:
///
/// ```text
/// title = grep -m1 '^#\+ ' | sed 's/^#\+ *//'   # empty → the basename
/// body  = sed '0,/^#\+ /{/^#\+ /d}' | sed '/^[[:space:]]*$/d' | head -c 2000
/// words = sed '0,/^#\+ /{/^#\+ /d}' | wc -w
/// ```
///
/// Only the FIRST heading line leaves the body — a second `# ` line is text
/// like any other.
pub fn parse(text: &str, base: &str) -> Parsed {
    let mut title = String::new();
    let mut body_lines: Vec<&str> = Vec::new();
    let mut heading_seen = false;
    for line in text.lines() {
        if !heading_seen && is_heading(line) {
            heading_seen = true;
            title = heading_text(line).to_string();
            continue;
        }
        body_lines.push(line);
    }
    if title.is_empty() {
        title = base.to_string();
    }
    let words = body_lines
        .iter()
        .map(|l| l.split_whitespace().count())
        .sum();
    let mut squeezed = String::new();
    for line in &body_lines {
        if line.trim().is_empty() {
            continue;
        }
        squeezed.push_str(line);
        squeezed.push('\n');
    }
    // `$()` stripped the trailing newline off the way out, so the body a
    // row carries never ends in one.
    let body = head_bytes(&squeezed, 2000)
        .trim_end_matches('\n')
        .to_string();
    Parsed { title, body, words }
}

/// `${body:0:400}` — the first four hundred characters of the body, which
/// is what the card's two lines and Ctrl+Enter's preview both draw.
pub fn excerpt(body: &str) -> String {
    body.chars().take(400).collect()
}

/// The query check: a hit on the title or the filename outranks one buried
/// in the body — 88000 versus 80000, the script's two tiers. `None` is the
/// note that does not match at all.
pub fn match_score(query: &str, title: &str, base: &str, body: &str) -> Option<i64> {
    if query.is_empty() {
        return Some(88000);
    }
    let q = query.to_lowercase();
    if title.to_lowercase().contains(&q) || base.to_lowercase().contains(&q) {
        Some(88000)
    } else if body.to_lowercase().contains(&q) {
        Some(80000)
    } else {
        None
    }
}

/// `ago` — how long since the note was touched, in the words a person uses
/// about their own notes: "just now" under an hour, "3h ago" under a day,
/// "yesterday", "5d ago" under a week. Past that the card shows the date —
/// `detail`, already rendered, minus its year.
pub fn ago(now: i64, then: i64, detail: &str) -> String {
    let diff = (now - then).max(0);
    if diff < 3600 {
        "just now".to_string()
    } else if diff < 86400 {
        format!("{}h ago", diff / 3600)
    } else if diff < 172800 {
        "yesterday".to_string()
    } else if diff < 604800 {
        format!("{}d ago", diff / 86400)
    } else {
        day_mon(detail)
    }
}

/// `+%-d %b` is `+%-d %b %Y` without its last word — one `date` formatting
/// serves both the detail line and the old-note stamp.
pub fn day_mon(detail: &str) -> String {
    detail
        .rsplit_once(' ')
        .map(|(head, _)| head)
        .unwrap_or(detail)
        .to_string()
}

/// `date -d "@then" '+%-d %b %Y'` with no `date` to ask: the civil date of
/// the UTC day number with English month names — the same convention
/// `time::days::today` uses for the rest of the core.
pub fn stamp_date_utc(then: i64) -> String {
    let (y, m, d) = days::civil_from_days(then.div_euclid(86400));
    format!("{d} {} {y}", days::MONTHS_ABBR[(m - 1) as usize])
}

/// The count line above the list. "1 note" says it plainly; a query that
/// kept only some of the notes splits it into "3 of 7 notes" — except at
/// zero, where "Nothing matches · 7 notes" is the useful half of the
/// arithmetic. A just-saved note appends "· 1 new".
pub fn tally(total: usize, matched: usize, searching: bool, fresh_seen: bool) -> String {
    let mut t = if total == 1 {
        "1 note".to_string()
    } else {
        format!("{total} notes")
    };
    if searching && matched != total {
        t = if matched == 0 {
            format!("Nothing matches · {t}")
        } else {
            format!("{matched} of {total} notes")
        };
    }
    if fresh_seen {
        t = format!("{t} · 1 new");
    }
    t
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slug_makes_a_filename_of_the_words() {
        assert_eq!(slug("remember the video"), "remember-the-video");
        assert_eq!(slug("  Hello, World!  "), "hello-world");
        assert_eq!(slug("under_score and-dash"), "under-score-and-dash");
        // The `--save` instructions can never collide: a leading dash is
        // punctuation like any other and slugs away.
        assert_eq!(slug("--save x"), "save-x");
    }

    #[test]
    fn slug_of_pure_punctuation_is_nothing() {
        assert_eq!(slug("!!!"), "");
        assert_eq!(slug("…"), "");
        assert_eq!(slug("///"), "");
        // é is not [a-z0-9] — it slugs to a dash, so only the 't' survives.
        assert_eq!(slug("été"), "t");
    }

    #[test]
    fn slug_is_capped_at_sixty_characters() {
        let long = "a very long note title that just keeps going and going and going";
        let s = slug(long);
        assert!(s.chars().count() <= 60);
        assert!(!s.ends_with('-'));
        // The cap can land between a word and its dash; the tail strip is
        // what keeps "foo-" out of a filename.
        let at_sixty = format!("{} rest", "x".repeat(59));
        assert_eq!(slug(&at_sixty), "x".repeat(59).as_str());
    }

    #[test]
    fn a_heading_is_hashes_then_a_space() {
        assert!(is_heading("# Title"));
        assert!(is_heading("## Title"));
        assert!(is_heading("# "));
        // No space, no heading; a leading indent is not one either.
        assert!(!is_heading("#Title"));
        assert!(!is_heading(" # Indented"));
        assert!(!is_heading("#\tTabbed"));
        assert!(!is_heading(""));
        assert!(!is_heading("plain text"));
    }

    #[test]
    fn the_heading_text_strips_hashes_and_spaces_only() {
        assert_eq!(heading_text("# Title"), "Title");
        assert_eq!(heading_text("###   Spaced"), "Spaced");
        // `sed 's/^#\+ *//'` eats spaces, not tabs.
        assert_eq!(heading_text("# \tTabbed"), "\tTabbed");
        assert_eq!(heading_text("# "), "");
    }

    #[test]
    fn parse_pulls_the_first_heading_and_squeezes_the_body() {
        let p = parse(
            "# Standup notes\n\nTalked to the team.\n\nSecond line.\n",
            "x",
        );
        assert_eq!(p.title, "Standup notes");
        assert_eq!(p.body, "Talked to the team.\nSecond line.");
        assert_eq!(p.words, 6);
    }

    #[test]
    fn parse_falls_back_to_the_basename() {
        let p = parse("no heading here\njust text\n", "plain");
        assert_eq!(p.title, "plain");
        assert_eq!(p.body, "no heading here\njust text");
        assert_eq!(p.words, 5);
    }

    #[test]
    fn a_heading_that_is_not_first_still_names_the_note() {
        let p = parse("preamble line\n# Real title\nbody\n", "x");
        assert_eq!(p.title, "Real title");
        assert_eq!(p.body, "preamble line\nbody");
    }

    #[test]
    fn only_the_first_heading_leaves_the_body() {
        let p = parse("# One\n# Two\nbody\n", "x");
        assert_eq!(p.title, "One");
        assert_eq!(p.body, "# Two\nbody");
    }

    #[test]
    fn an_empty_heading_names_the_file() {
        let p = parse("# \nbody\n", "fallback");
        assert_eq!(p.title, "fallback");
        assert_eq!(p.body, "body");
    }

    #[test]
    fn an_empty_file_is_all_basename() {
        let p = parse("", "empty-note");
        assert_eq!(p.title, "empty-note");
        assert_eq!(p.body, "");
        assert_eq!(p.words, 0);
    }

    #[test]
    fn a_note_without_a_trailing_newline_reads_the_same() {
        let p = parse("# T\n\nlast line", "x");
        assert_eq!(p.title, "T");
        assert_eq!(p.body, "last line");
    }

    #[test]
    fn a_title_only_note_is_zero_words() {
        let p = parse("# Just a title\n\n", "x");
        assert_eq!(p.title, "Just a title");
        assert_eq!(p.body, "");
        assert_eq!(p.words, 0);
    }

    #[test]
    fn the_body_is_capped_at_two_thousand_bytes() {
        let long = "word ".repeat(600); // 3000 bytes
        let p = parse(&format!("# t\n{long}"), "x");
        assert!(p.body.len() <= 2000);
        // `wc -w` counted the whole body — the cap is on what is carried,
        // not on what is counted.
        assert_eq!(p.words, 600);
    }

    #[test]
    fn the_excerpt_is_four_hundred_characters() {
        let body = "x".repeat(500);
        assert_eq!(excerpt(&body).chars().count(), 400);
        assert_eq!(excerpt("short"), "short");
    }

    #[test]
    fn matching_is_substring_with_the_title_outranking_the_body() {
        // No query matches everything at the top tier.
        assert_eq!(match_score("", "anything", "x", ""), Some(88000));
        assert_eq!(match_score("stand", "Standup notes", "x", ""), Some(88000));
        // The filename matches like the title does.
        assert_eq!(
            match_score("stand", "Unrelated", "standup-notes", ""),
            Some(88000)
        );
        assert_eq!(
            match_score("milk", "Groceries", "g", "- milk\n- eggs"),
            Some(80000)
        );
        assert_eq!(match_score("zzz", "Groceries", "g", "- milk"), None);
        // Case is nothing to either side.
        assert_eq!(match_score("MILK", "x", "x", "- Milk"), Some(80000));
    }

    #[test]
    fn ago_speaks_in_the_words_a_person_uses() {
        let now = 1_700_000_000i64;
        assert_eq!(ago(now, now - 10, ""), "just now");
        assert_eq!(ago(now, now - 3599, ""), "just now");
        assert_eq!(ago(now, now - 3600, ""), "1h ago");
        assert_eq!(ago(now, now - 86399, ""), "23h ago");
        assert_eq!(ago(now, now - 86400, ""), "yesterday");
        assert_eq!(ago(now, now - 172800, ""), "2d ago");
        assert_eq!(ago(now, now - 604799, ""), "6d ago");
        // A week or more is the date, minus its year.
        assert_eq!(ago(now, now - 604800, "9 Nov 2023"), "9 Nov");
        // A clock that read the future is not a negative age.
        assert_eq!(ago(now, now + 500, ""), "just now");
    }

    #[test]
    fn day_mon_drops_the_year() {
        assert_eq!(day_mon("5 Aug 2027"), "5 Aug");
        assert_eq!(day_mon("15 Nov 2023"), "15 Nov");
        assert_eq!(day_mon("unusual"), "unusual");
    }

    #[test]
    fn the_utc_fallback_renders_the_same_shape() {
        // 1700000000 is 2023-11-14 in UTC (the local answer may be the 15th).
        assert_eq!(stamp_date_utc(1_700_000_000), "14 Nov 2023");
        assert_eq!(stamp_date_utc(0), "1 Jan 1970");
    }

    #[test]
    fn the_tally_counts_and_splits() {
        assert_eq!(tally(0, 0, false, false), "0 notes");
        assert_eq!(tally(1, 1, false, false), "1 note");
        assert_eq!(tally(7, 7, false, false), "7 notes");
        // A query showing everything does not get a fraction.
        assert_eq!(tally(7, 7, true, false), "7 notes");
        assert_eq!(tally(7, 3, true, false), "3 of 7 notes");
        assert_eq!(tally(7, 0, true, false), "Nothing matches · 7 notes");
        assert_eq!(tally(7, 7, false, true), "7 notes · 1 new");
        assert_eq!(tally(7, 3, true, true), "3 of 7 notes · 1 new");
    }
}
