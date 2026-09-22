//! The word work — all of it pure: the term cleanup that decides whether a
//! query is a word at all, the stem list, and the edit distance that keeps
//! a Datamuse guess honest.

/// What the script does to the query before the dictionary sees it: the
/// trim, the one trailing punctuation mark, the quotes, the whitespace
/// collapse — then the gates: two characters at least, letters plus `'`,
/// space and `-` only, and no more than three words (the dictionary is not
/// indexed by sentences). `None` is the script's silent exit.
pub(super) fn clean_term(query: &str) -> Option<String> {
    let term = query.trim_matches(|c: char| c.is_whitespace());
    // `${term%%[.,;:!?]}` — a sentence ends with punctuation; a word is not
    // spelled with it. One character's worth.
    let term = match term.chars().last() {
        Some(c) if ".,;:!?".contains(c) => &term[..term.len() - c.len_utf8()],
        _ => term,
    };
    // `${term#\"}` / `${term%\"}` — one quote off each end.
    let term = term.strip_prefix('"').unwrap_or(term);
    let term = term.strip_suffix('"').unwrap_or(term);
    // `sed -E 's/[[:space:]]+/ /g'` — every whitespace run folds to a space.
    let mut collapsed = String::with_capacity(term.len());
    let mut ws = false;
    for c in term.chars() {
        if c.is_whitespace() {
            if !ws {
                collapsed.push(' ');
                ws = true;
            }
        } else {
            collapsed.push(c);
            ws = false;
        }
    }
    if collapsed.chars().count() < 2 {
        return None;
    }
    // `^[[:alpha:]][[:alpha:]\'\ -]*$` — a word or a short phrase, not a
    // path and not a query string.
    let mut chars = collapsed.chars();
    if !chars.next().is_some_and(|c| c.is_alphabetic()) {
        return None;
    }
    if !chars.all(|c| c.is_alphabetic() || c == '\'' || c == ' ' || c == '-') {
        return None;
    }
    if collapsed.bytes().filter(|b| *b == b' ').count() > 2 {
        return None;
    }
    Some(collapsed)
}

/// The spellings a word has that the dictionary does not: plurals, tenses,
/// comparatives and adverbs, in the order they are worth trying. Every one
/// is checked against the dictionary before it is shown, so a stem that
/// happens to be a different word only ever appears with its own entry.
pub(super) fn stems(w: &str) -> Vec<String> {
    let chars: Vec<char> = w.chars().collect();
    let n = chars.len();
    let mut out: Vec<String> = Vec::new();
    let head = |k: usize| chars[..k].iter().collect::<String>();

    if w.ends_with("'s") {
        out.push(w.strip_suffix("'s").unwrap().to_string());
    }
    if w.ends_with("ies") {
        if n > 4 {
            out.push(format!("{}y", w.strip_suffix("ies").unwrap()));
        }
    } else if ["sses", "shes", "ches", "xes", "zes"]
        .iter()
        .any(|s| w.ends_with(s))
    {
        if n > 4 {
            out.push(w.strip_suffix("es").unwrap().to_string());
        }
    } else if w.ends_with("ss") {
        // *ss) ;; — a double s is not a plural marker.
    } else if w.ends_with("s") && n > 3 {
        out.push(w.strip_suffix("s").unwrap().to_string());
    }
    if w.ends_with("iest") {
        if n > 5 {
            out.push(format!("{}y", w.strip_suffix("iest").unwrap()));
        }
    } else if w.ends_with("est") && n > 5 {
        out.push(w.strip_suffix("est").unwrap().to_string());
        out.push(w.strip_suffix("st").unwrap().to_string());
    }
    if w.ends_with("ier") {
        if n > 4 {
            out.push(format!("{}y", w.strip_suffix("ier").unwrap()));
        }
    } else if w.ends_with("er") && n > 4 {
        out.push(w.strip_suffix("er").unwrap().to_string());
        out.push(w.strip_suffix("r").unwrap().to_string());
    }
    if w.ends_with("ing") {
        if n <= 5 {
            return out;
        }
        let stem = w.strip_suffix("ing").unwrap();
        out.push(stem.to_string());
        out.push(format!("{stem}e"));
        // "running" is "run" with the consonant doubled to keep the vowel
        // short.
        if chars[n - 5] == chars[n - 4] {
            out.push(head(n - 4));
        }
    }
    if w.ends_with("ied") {
        if n > 4 {
            out.push(format!("{}y", w.strip_suffix("ied").unwrap()));
        }
    } else if w.ends_with("ed") {
        if n <= 3 {
            return out;
        }
        out.push(w.strip_suffix("ed").unwrap().to_string());
        out.push(w.strip_suffix("d").unwrap().to_string());
        if chars[n - 4] == chars[n - 3] {
            out.push(head(n - 3));
        }
    }
    if w.ends_with("ily") {
        if n > 4 {
            out.push(format!("{}y", w.strip_suffix("ily").unwrap()));
        }
    } else if w.ends_with("ly") && n > 4 {
        out.push(w.strip_suffix("ly").unwrap().to_string());
    }
    out
}

/// How far apart two spellings are — the awk Levenshtein in the script,
/// char-indexed the way `substr` reads them.
fn distance(a: &[char], b: &[char]) -> usize {
    let (la, lb) = (a.len(), b.len());
    let mut d = vec![vec![0usize; lb + 1]; la + 1];
    for (i, row) in d.iter_mut().enumerate() {
        row[0] = i;
    }
    for (j, cell) in d[0].iter_mut().enumerate() {
        *cell = j;
    }
    for i in 1..=la {
        for j in 1..=lb {
            let c = usize::from(a[i - 1] != b[j - 1]);
            d[i][j] = (d[i - 1][j] + 1)
                .min(d[i][j - 1] + 1)
                .min(d[i - 1][j - 1] + c);
        }
    }
    d[la][lb]
}

/// A guess is only worth showing when it is a typo away from what was
/// typed: "recieve" and "receive" are two edits apart; "asdfghjkl" and
/// "oesophageal" are not a guess, they are a different word Datamuse
/// thought rhymed.
pub(super) fn near(a: &str, b: &str) -> bool {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    if a.first() != b.first() {
        return false;
    }
    if a.len().abs_diff(b.len()) > 2 {
        return false;
    }
    distance(&a, &b) <= 2
}
