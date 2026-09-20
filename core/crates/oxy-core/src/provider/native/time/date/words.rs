/// The forms the day parser does not know. "in 90 days" is the one people
/// type most, and "a day" is the one it would answer wrong.
pub(super) fn normalize_relative(t: &str) -> String {
    let mut t = t.strip_prefix("in ").unwrap_or(t).to_string();
    for suffix in [" from now", " from today"] {
        if let Some(r) = t.strip_suffix(suffix) {
            t = r.to_string();
        }
    }
    for prefix in ["an ", "a "] {
        if let Some(r) = t.strip_prefix(prefix) {
            t = format!("1 {r}");
            break;
        }
    }
    t
}

/// The words a person types that a strict grammar has never heard of,
/// rewritten into the ones it has. Ordinals are the biggest of them: "3rd of
/// march" is how a date is said out loud.
///
/// This runs after the named days have had their turn, because "4th of july"
/// is a holiday alias and stripping the "th" off it first would lose it.
pub(super) fn normalize_words(t: &str) -> String {
    let t = t.trim();
    let t = t
        .strip_prefix("the ")
        .or_else(|| t.strip_prefix("on "))
        .unwrap_or(t);
    let mut out = String::with_capacity(t.len());
    for word in t.split_whitespace() {
        let w = match word {
            "tmr" | "tmrw" | "2moro" | "2mrw" => "tomorrow".to_string(),
            "yday" => "yesterday".to_string(),
            "coming" | "upcoming" => "next".to_string(),
            "mon" => "monday".to_string(),
            "tue" | "tues" => "tuesday".to_string(),
            "wed" | "weds" => "wednesday".to_string(),
            "thu" | "thur" | "thurs" => "thursday".to_string(),
            "fri" => "friday".to_string(),
            "sat" => "saturday".to_string(),
            "sun" => "sunday".to_string(),
            "sept" => "september".to_string(),
            "of" => String::new(), // "3 of march" reads as "3 march"
            _ => {
                // Ordinals: "3rd" is "3".
                let digits = word.trim_end_matches(['s', 't', 'n', 'd', 'r', 'd', 't', 'h']);
                if !digits.is_empty()
                    && digits.chars().all(|c| c.is_ascii_digit())
                    && word.len() > digits.len()
                {
                    digits.to_string()
                } else {
                    word.to_string()
                }
            }
        };
        if w.is_empty() {
            continue;
        }
        if !out.is_empty() {
            out.push(' ');
        }
        out.push_str(&w);
    }
    out
}
