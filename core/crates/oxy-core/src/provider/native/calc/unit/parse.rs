//! The sentence half of `bin/oxy-unit`: the words people write around a
//! unit question, read exactly the way the script read them — one regex and
//! one table rule at a time, in the script's order, every `return None` a
//! place it said `exit 0`.

use std::sync::OnceLock;

use fancy_regex::Regex;

use super::table::{ambiguity, canon, currency, density_of, family_of, pretty};

/// POSIX `[[:space:]]`, the ASCII set — a no-break space is a space to a
/// person and is replaced as a character before anything else runs.
pub(super) fn is_space(c: char) -> bool {
    matches!(c, ' ' | '\t' | '\n' | '\x0b' | '\x0c' | '\r')
}

fn is_digit(c: char) -> bool {
    c.is_ascii_digit()
}

/// `sed 's/\xc2\xa0/ /g; s/[[:space:]]+/ /g; s/^ //; s/ $//'` — collapse every
/// whitespace run to one space and drop the ends. The lazy space means a
/// leading or trailing run never lands at all.
fn squash(query: &str) -> String {
    let s = query.replace('\u{a0}', " ");
    let mut out = String::with_capacity(s.len());
    let mut pending = false;
    for c in s.chars() {
        if is_space(c) {
            pending = true;
            continue;
        }
        if pending && !out.is_empty() {
            out.push(' ');
        }
        pending = false;
        out.push(c);
    }
    out
}

/// `s/^(word|…)[[:space:]]+//` — the word list is literal, so the try order
/// is longest-first: what POSIX leftmost-longest picked ("whats the" before
/// "whats"). Applied once, the way one `s` command is.
fn strip_lead(q: &mut String, words: &[&'static str]) {
    for w in words {
        if let Some(rest) = q.strip_prefix(w)
            && rest.chars().next().is_some_and(is_space)
        {
            *q = rest.trim_start_matches(is_space).to_string();
            return;
        }
    }
}

/// `s/[[:space:]]+(word|…)$//` — the trailing counterpart.
fn strip_trail(q: &mut String, words: &[&'static str]) {
    for w in words {
        if let Some(rest) = q.strip_suffix(w)
            && rest.ends_with(is_space)
        {
            *q = rest.trim_end_matches(is_space).to_string();
            return;
        }
    }
}

/// `unarticle`: an article is not part of a unit's name on either side —
/// the first of a/an/one/the that prefixes, stripped once.
fn unarticle(v: &str) -> String {
    for a in ["a ", "an ", "one ", "the "] {
        if let Some(rest) = v.strip_prefix(a) {
            return rest.to_string();
        }
    }
    v.to_string()
}

/// A word for a small number is what people type when the number is not
/// worth writing. `s/^word /n/` in the script's order; the first hit wins
/// because every later rule would see a string that now starts with a digit.
const WORD_NUMBERS: &[(&str, &str)] = &[
    ("one and a half ", "1.5"),
    ("two and a half ", "2.5"),
    ("three and a half ", "3.5"),
    ("a half ", "0.5"),
    ("half a ", "0.5"),
    ("half an ", "0.5"),
    ("half ", "0.5"),
    ("a quarter of a ", "0.25"),
    ("a quarter of an ", "0.25"),
    ("a quarter ", "0.25"),
    ("a third of a ", "0.333333"),
    ("a third of an ", "0.333333"),
    ("a couple of ", "2"),
    ("a couple ", "2"),
    ("a dozen ", "12"),
    ("a ", "1"),
    ("an ", "1"),
    ("one ", "1"),
    ("two ", "2"),
    ("three ", "3"),
    ("four ", "4"),
    ("five ", "5"),
    ("six ", "6"),
    ("seven ", "7"),
    ("eight ", "8"),
    ("nine ", "9"),
    ("ten ", "10"),
    ("eleven ", "11"),
    ("twelve ", "12"),
    ("fifteen ", "15"),
    ("twenty ", "20"),
    ("thirty ", "30"),
    ("fifty ", "50"),
    ("hundred ", "100"),
    ("a hundred ", "100"),
    ("one hundred ", "100"),
    ("a thousand ", "1000"),
    ("one thousand ", "1000"),
];

macro_rules! re {
    ($name:ident, $pat:literal) => {
        fn $name() -> &'static Regex {
            static RE: OnceLock<Regex> = OnceLock::new();
            RE.get_or_init(|| Regex::new($pat).unwrap())
        }
    };
}

// `([0-9])[,\ ]([0-9]{3})([^0-9]|$)` — "1,234 miles" is 1234 miles. A comma
// between digits with exactly three digits after it and no digit following
// them is a thousands separator everywhere it is written; a comma anywhere
// else is left alone, so "1,5 m" still reads as qalc reads it.
re!(group_re, r"([0-9])[, ]([0-9]{3})([^0-9]|$)");
re!(arrow_re, r"[[:space:]]*(->|=>|-->|→|>>)[[:space:]]*");
re!(eq_q_re, r"[[:space:]]*=[[:space:]]*\?[[:space:]]*");
re!(eq_end_re, r"[[:space:]]*=[[:space:]]*$");
re!(equals_re, r"[[:space:]]+equals[[:space:]]+");
re!(how_many_re, r"^how[[:space:]]+many[[:space:]]+(.+)$");
re!(verb_re, r"[[:space:]]+(are|is|do|does|there)[[:space:]]+");
re!(
    back_split_re,
    r"^(.*[^[:space:]])[[:space:]]+(in|to|per|make|makes|within)[[:space:]]+([^[:space:]].*)$"
);
// "180f" is one token to a person and two to qalc. So is "20°c". The
// exponent of 1e3 is hidden behind a marker first, or the same rule that
// turns "180f" into "180 f" turns "1e3 m" into "1 e3 m".
re!(exponent_re, r"^([-+]?[0-9]*[.]?[0-9]+)[eE]([-+]?[0-9]+)");
re!(degree_re, r"([0-9])[[:space:]]*(°)");
re!(glued_re, r"^([-+]?[0-9]+([.][0-9]+)?)[[:space:]]*([a-z°])");
re!(
    split_re,
    r"^(.*[^[:space:]])[[:space:]]+(in|to|as|into|inn?to)[[:space:]]+([^[:space:]].*)$"
);
re!(
    from_re,
    r"^([^[:space:]]+)[[:space:]]+(from|out[[:space:]]+of)[[:space:]]+([^[:space:]].*)$"
);
// A stick of butter is half a cup everywhere it is written on a wrapper.
re!(stick_re, r"^([0-9.]+) ?sticks? of (butter|margarine)$");
re!(
    stick2_re,
    r"^([0-9.]+)[[:space:]]stick_butter[[:space:]](.*)$"
);
// "1 cup of flour", "2 cups flour", "250g of flour".
re!(
    of_re,
    r"^(.+[^[:space:]])[[:space:]]+of[[:space:]]+([a-z][a-z[:space:]]*)$"
);
re!(bare_ing_re, r"^(.+[^[:space:]])[[:space:]]+([a-z]+)$");
// A height is two numbers: 6ft2, 6'2", 6 ft 2 in, 5 foot 11. So is 7lb 8oz
// and 12 stone 6. Each collapses to one quantity in the smaller unit.
re!(
    ft_in_re,
    r#"^([0-9]+)[[:space:]]*(ft|foot|feet|'|′)[[:space:]]*([0-9]+(\.[0-9]+)?)[[:space:]]*(in|inch|inches|"|″)?$"#
);
re!(
    lb_oz_re,
    r"^([0-9]+)[[:space:]]*(lb|lbs|pound|pounds)[[:space:]]*([0-9]+(\.[0-9]+)?)[[:space:]]*(oz|ounce|ounces)?$"
);
re!(
    st_lb_re,
    r"^([0-9]+)[[:space:]]*(st|stone|stones)[[:space:]]*([0-9]+(\.[0-9]+)?)[[:space:]]*(lb|lbs|pound|pounds)?$"
);
// "1 1/2 cups" is one and a half cups on every recipe card ever written,
// and two numbers side by side to a parser.
re!(
    frac_re,
    r"^([0-9]+)[[:space:]]+([0-9]+/[0-9]+)[[:space:]]+(.*)$"
);
// The number in front, and the unit behind it. No number means one of them.
re!(
    qty_paren_re,
    r"^(\([0-9]+\+[0-9]+/[0-9]+\))[[:space:]]*(.*)$"
);
re!(
    qty_num_re,
    r"^([-+]?([0-9]+(\.[0-9]+)?|\.[0-9]+)([eE][-+]?[0-9]+)?([[:space:]]*[*/^][[:space:]]*[-+]?[0-9]+(\.[0-9]+)?)*)[[:space:]]*(.*)$"
);
// "187 cm in feet and inches" wants the answer people say out loud.
re!(
    mixed_re,
    r"^(.+[^[:space:]])[[:space:]]*(and|&|\+|,)[[:space:]]*([^[:space:]].+)$"
);

/// `awk -v n="…" 'BEGIN{printf "%g", …}'` — strtod's numeric prefix: digits,
/// one dot, digits. "1.2.3" reads as 1.2 to awk too.
pub(super) fn awk_num(s: &str) -> f64 {
    let b = s.as_bytes();
    let mut i = 0;
    while i < b.len() && b[i].is_ascii_digit() {
        i += 1;
    }
    if i < b.len() && b[i] == b'.' {
        let mut j = i + 1;
        while j < b.len() && b[j].is_ascii_digit() {
            j += 1;
        }
        // "1." is 1 to strtod; a bare "." is nothing.
        if j > i + 1 || i > 0 {
            i = j;
        }
    }
    s[..i].parse().unwrap_or(0.0)
}

/// `printf %g` at the default precision of 6: fixed notation, trailing zeros
/// trimmed, switching to `e±dd` below 1e-4 and at 1e6.
pub(super) fn g_fmt(x: f64) -> String {
    if x == 0.0 || !x.is_finite() {
        return format!("{x}");
    }
    let s = format!("{x:.5e}");
    let epos = s.find('e').unwrap();
    let exp: i32 = s[epos + 1..].parse().unwrap_or(0);
    if !(-4..6).contains(&exp) {
        let mant = s[..epos].trim_end_matches('0').trim_end_matches('.');
        return format!("{mant}e{}{:02}", if exp < 0 { "-" } else { "+" }, exp.abs());
    }
    let prec = (5 - exp).max(0) as usize;
    format!("{x:.prec$}")
        .trim_end_matches('0')
        .trim_end_matches('.')
        .to_string()
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) enum Kind {
    Unit,
    Currency,
}

/// What `parse` understood of the sentence: the two canonical spellings,
/// which side is money, the density bridge, and the notes the row prints —
/// everything `plan` and `assemble` need and nothing they don't.
pub(super) struct Question {
    pub as_typed: String,
    pub quantity: String,
    /// The unit text as typed, for the ambiguity note and the currency
    /// reswitch message. Write-only outside tests: the notes are read by
    /// then.
    #[allow(dead_code)]
    pub source_unit: String,
    /// The target text as typed (after a mixed split rewrote it).
    #[allow(dead_code)]
    pub rhs: String,
    pub src: &'static str,
    pub tgt: Option<&'static str>,
    pub kind: Kind,
    pub sfam: &'static str,
    pub density: Option<&'static str>,
    #[allow(dead_code)]
    pub ingredient: Option<String>,
    pub mixed: bool,
    #[allow(dead_code)]
    pub asked_backwards: bool,
    pub notes: Vec<String>,
}

/// The whole gate: sentence → Question, or silence. Every `None` here is a
/// place the script said `exit 0` — a wrong answer is the one thing this
/// provider is not allowed to give.
pub(super) fn parse(query: &str) -> Option<Question> {
    if query.is_empty() {
        return None;
    }
    let raw = squash(query);
    if raw.is_empty() {
        return None;
    }
    let as_typed = raw.clone();
    let mut q = raw.to_lowercase();

    // A trailing question mark or full stop is punctuation, not a unit.
    q = q
        .trim_end_matches(['?', '.', '!'])
        .trim_end_matches(is_space)
        .to_string();
    strip_lead(&mut q, &["please", "plz"]);
    strip_trail(&mut q, &["please", "plz"]);

    // The reading is said out loud below, because a European writing "1,234"
    // for one-point-two-three-four deserves to see which way it was taken.
    let mut grouped = false;
    while group_re().is_match(&q).unwrap_or(false) {
        grouped = true;
        q = group_re().replace_all(&q, "$1$2$3").into_owned();
    }

    // The sentence people put in front of the question.
    strip_lead(
        &mut q,
        &["calculate", "convert", "show me", "tell me", "calc"],
    );
    strip_lead(
        &mut q,
        &[
            "how much are",
            "how much is",
            "how many is",
            "whats the",
            "what is",
            "what's",
            "what are",
            "whats",
        ],
    );
    strip_lead(&mut q, &["is", "of"]);
    // An arrow is a conversion word drawn instead of spelled.
    q = arrow_re().replace_all(&q, " to ").into_owned();
    q = eq_q_re().replace_all(&q, " to ").into_owned();
    q = eq_end_re().replace(&q, "").into_owned();
    q = equals_re().replace_all(&q, " to ").into_owned();

    for (from, to) in WORD_NUMBERS {
        if let Some(rest) = q.strip_prefix(from) {
            q = format!("{to} {rest}");
            break;
        }
    }

    let mut notes: Vec<String> = Vec::new();

    // ------------------------------------------------------- the two halves
    //
    // "how many feet in a mile" is the same question as "1 mile in feet"
    // with the halves written the other way round, and it is the way the
    // question is asked out loud.
    let mut asked_backwards = false;
    if let Ok(Some(c)) = how_many_re().captures(&q) {
        let rest = verb_re()
            .replace_all(c.get(1).unwrap().as_str(), " ")
            .into_owned();
        let Ok(Some(sp)) = back_split_re().captures(&rest) else {
            return None;
        };
        q = format!(
            "{} to {}",
            sp.get(3).unwrap().as_str(),
            sp.get(1).unwrap().as_str()
        );
        asked_backwards = true;
    }

    q = exponent_re().replace(&q, "$1@e@$2").into_owned();
    q = degree_re().replace_all(&q, "$1 $2").into_owned();
    q = glued_re().replace(&q, "$1 $3").into_owned();
    q = q.replacen("@e@", "e", 1);

    let mut lhs = q.clone();
    let mut rhs = String::new();
    if let Ok(Some(c)) = split_re().captures(&q) {
        lhs = c.get(1).unwrap().as_str().to_string();
        rhs = c.get(3).unwrap().as_str().to_string();
    } else if let Ok(Some(c)) = from_re().captures(&q) {
        // "miles from 5km": the answer named first and the question second.
        lhs = c.get(3).unwrap().as_str().to_string();
        rhs = c.get(1).unwrap().as_str().to_string();
        asked_backwards = true;
    }

    // "miles in 5km" is the same inversion written with the ordinary word.
    // It is only an inversion when one side carries the number and the
    // other does not.
    if !rhs.is_empty() && !lhs.chars().any(is_digit) && rhs.chars().any(is_digit) {
        std::mem::swap(&mut lhs, &mut rhs);
        asked_backwards = true;
    }

    // Half a conversion word, because the query arrives on every keystroke
    // and "5 km in" is one letter into "5 km in miles". The family answer
    // stands in until the second half arrives.
    if rhs.is_empty() {
        for w in [" in", " to", " as", " into"] {
            if let Some(rest) = lhs.strip_suffix(w) {
                lhs = rest.to_string();
                break;
            }
        }
    }

    rhs = unarticle(&rhs);
    lhs = unarticle(&lhs);
    if let Some(rest) = lhs.strip_suffix(" pls") {
        lhs = rest.to_string();
    }
    if let Some(rest) = lhs.strip_suffix(" plz") {
        lhs = rest.to_string();
    }

    // ----------------------------------------------------- the left side
    lhs = stick_re().replace(&lhs, "$1 stick_butter $2").into_owned();
    if let Ok(Some(c)) = stick2_re().captures(&lhs) {
        let n = awk_num(c.get(1).unwrap().as_str());
        lhs = format!("{} cup {}", g_fmt(n * 0.5), c.get(2).unwrap().as_str());
        notes.push("1 stick of butter read as half a cup".to_string());
    }

    let mut ingredient: Option<String> = None;
    let mut density: Option<&'static str> = None;
    if let Ok(Some(c)) = of_re().captures(&lhs)
        && let Some(d) = density_of(c.get(2).unwrap().as_str())
    {
        ingredient = Some(c.get(2).unwrap().as_str().to_string());
        density = Some(d);
        lhs = c.get(1).unwrap().as_str().to_string();
    }
    if ingredient.is_none()
        && let Ok(Some(c)) = bare_ing_re().captures(&lhs)
    {
        let tail = c.get(2).unwrap().as_str();
        // a word that is both an ingredient and a unit is the unit
        if let Some(d) = density_of(tail)
            && canon(tail).is_none()
        {
            ingredient = Some(tail.to_string());
            density = Some(d);
            lhs = c.get(1).unwrap().as_str().to_string();
        }
    }

    if let Ok(Some(c)) = ft_in_re().captures(&lhs) {
        let v =
            g_fmt(awk_num(c.get(1).unwrap().as_str()) * 12.0 + awk_num(c.get(3).unwrap().as_str()));
        lhs = format!("{v} inch");
        notes.push(format!("read as {v} inches"));
    } else if let Ok(Some(c)) = lb_oz_re().captures(&lhs) {
        let v =
            g_fmt(awk_num(c.get(1).unwrap().as_str()) * 16.0 + awk_num(c.get(3).unwrap().as_str()));
        lhs = format!("{v} ounce");
        notes.push(format!("read as {v} ounces"));
    } else if let Ok(Some(c)) = st_lb_re().captures(&lhs) {
        let v =
            g_fmt(awk_num(c.get(1).unwrap().as_str()) * 14.0 + awk_num(c.get(3).unwrap().as_str()));
        lhs = format!("{v} pound");
        notes.push(format!("read as {v} pounds"));
    }

    if let Ok(Some(c)) = frac_re().captures(&lhs) {
        lhs = format!(
            "({}+{}) {}",
            c.get(1).unwrap().as_str(),
            c.get(2).unwrap().as_str(),
            c.get(3).unwrap().as_str()
        );
    }

    let mut quantity = "1".to_string();
    let mut source_unit = lhs.clone();
    if let Ok(Some(c)) = qty_paren_re().captures(&lhs) {
        quantity = c.get(1).unwrap().as_str().to_string();
        source_unit = c.get(2).unwrap().as_str().to_string();
    } else if let Ok(Some(c)) = qty_num_re().captures(&lhs) {
        quantity = c.get(1).unwrap().as_str().to_string();
        source_unit = c.get(7).unwrap().as_str().to_string();
    }
    if let Some(rest) = source_unit.strip_prefix(' ') {
        source_unit = rest.to_string();
    }
    if let Some(rest) = source_unit.strip_suffix(' ') {
        source_unit = rest.to_string();
    }
    if source_unit.is_empty() {
        return None;
    }

    // ------------------------------------------------------------- the gate
    let (mut src, mut kind) = if let Some(s) = canon(&source_unit) {
        (s, Kind::Unit)
    } else {
        (currency(&source_unit)?, Kind::Currency)
    };

    // "187 cm in feet and inches" wants the answer people say out loud,
    // which is the one qalc gives with its autoconversion left on. Every
    // other expression keeps it off, so this one is run on its own.
    let mut mixed = false;
    if !rhs.is_empty()
        && let Ok(Some(c)) = mixed_re().captures(&rhs)
        && let (Some(a), Some(b)) = (
            canon(c.get(1).unwrap().as_str()),
            canon(c.get(3).unwrap().as_str()),
        )
        && family_of(a).unwrap_or("") == family_of(b).unwrap_or("")
    {
        rhs = c.get(1).unwrap().as_str().to_string();
        mixed = true;
    }

    let mut tgt: Option<&'static str> = None;
    if !rhs.is_empty() {
        if kind == Kind::Currency {
            tgt = Some(currency(&rhs)?);
        } else if let Some(t) = canon(&rhs) {
            tgt = Some(t);
        } else if let Some(t) = currency(&rhs)
            && let Some(s) = currency(&source_unit)
        {
            // "50 pounds in dollars" is money on both sides, and a pound is
            // a mass right up until the other half of the sentence says it
            // is not.
            kind = Kind::Currency;
            src = s;
            tgt = Some(t);
            notes.push(format!("{source_unit} read as {src}"));
        } else {
            return None;
        }
    }

    let mut sfam = "";
    if kind == Kind::Unit {
        sfam = family_of(src)?;
        if let Some(t) = tgt {
            let tfam = family_of(t)?;
            if sfam != tfam {
                // A density is the only bridge between how much room it
                // takes and how much it weighs, and only when the query
                // named what it is made of.
                let bridged = density.is_some()
                    && ((sfam == "volume" && tfam == "mass")
                        || (sfam == "mass" && tfam == "volume"));
                if !bridged {
                    return None;
                }
            }
        }
    }

    if grouped {
        notes.push("thousands separator read as grouping".to_string());
    }
    if asked_backwards && tgt.is_some() {
        notes.push(format!(
            "read as {} {} to {}",
            quantity,
            pretty(src),
            pretty(tgt.unwrap_or(""))
        ));
    }
    if let (Some(i), Some(d)) = (&ingredient, density) {
        notes.push(format!("{i} at {d} g/mL"));
    }
    if !asked_backwards {
        if let Some(a) = ambiguity(&source_unit) {
            notes.push(a.to_string());
        }
        if !rhs.is_empty()
            && let Some(a) = ambiguity(&rhs)
        {
            notes.push(a.to_string());
        }
    }
    if matches!(src, "liquid_quart" | "liquid_pint" | "gal" | "fl_oz") {
        notes.push("US liquid measure".to_string());
    }
    if let Some(t) = tgt
        && matches!(t, "liquid_quart" | "liquid_pint" | "gal" | "fl_oz")
    {
        notes.push("US liquid measure".to_string());
    }

    Some(Question {
        as_typed,
        quantity,
        source_unit,
        rhs,
        src,
        tgt,
        kind,
        sfam,
        density,
        ingredient,
        mixed,
        asked_backwards,
        notes,
    })
}
