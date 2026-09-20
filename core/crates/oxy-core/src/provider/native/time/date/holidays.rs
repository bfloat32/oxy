use crate::provider::native::time::days;

// ------------------------------------------------------------- day maths

/// Easter Sunday, anonymous Gregorian — four of the days below hang off it
/// (Good Friday, Easter Monday, Carnival, Corpus Christi), which is why it is
/// worth the algorithm rather than a table of dates that runs out.
fn easter_of(y: i64) -> i64 {
    let a = y % 19;
    let b = y / 100;
    let c = y % 100;
    let d = b / 4;
    let e = b % 4;
    let f = (b + 8) / 25;
    let g = (b - f + 1) / 3;
    let h = (19 * a + b - d - g + 15) % 30;
    let i = c / 4;
    let k = c % 4;
    let l = (32 + 2 * e + 2 * i - h - k) % 7;
    let m = (a + 11 * h + 22 * l) / 451;
    let month = (h + l - 7 * m + 114) / 31;
    let day = (h + l - 7 * m + 114) % 31 + 1;
    days::days_from_civil(y, month, day)
}

/// The nth given weekday of a month: Thanksgiving is the fourth Thursday of
/// November and Mother's Day the second Sunday of May, and neither has a
/// fixed date to put in the table. `wday` is 1 = Monday … 7 = Sunday.
fn nth_weekday(y: i64, mon: i64, n: i64, wday: i64, plus: i64) -> i64 {
    let first = days::weekday_u(days::days_from_civil(y, mon, 1));
    let offset = (wday - first + 7) % 7;
    days::days_from_civil(y, mon, 1 + offset + 7 * (n - 1)) + plus
}

/// The first day of a quarter, where the quarter number may run off either
/// end of the year: "next quarter" in Q4 is Q1 of next year.
pub(super) fn quarter_start(y: i64, q: i64) -> i64 {
    let mut q = q;
    let mut y = y;
    while q > 4 {
        q -= 4;
        y += 1;
    }
    while q < 1 {
        q += 4;
        y -= 1;
    }
    days::days_from_civil(y, (q - 1) * 3 + 1, 1)
}

/// Monday of the week holding a day number, ISO style.
pub(super) fn monday_of(z: i64) -> i64 {
    z - (days::weekday_u(z) - 1)
}

// -------------------------------------------------------- the named days

enum Rule {
    /// A fixed date, month/day.
    Fixed(i64, i64),
    /// N days from Easter Sunday; N may be negative.
    Easter(i64),
    /// The `n`th `wday` (1 = Monday) of `month`, plus `plus` days.
    Nth(i64, i64, i64, i64),
}

struct Holiday {
    name: &'static str,
    aliases: &'static [&'static str],
    rule: Rule,
}

// Deliberately the days a person types rather than every public holiday there
// is. This desktop is in Brazil, so the national days are here beside the
// international ones and "independence day" means 7 September; anything that
// names a different date in two countries is spelled out rather than guessed,
// which is why there is no bare "father's day" (June in the US, August here).
const HOLIDAYS: &[Holiday] = &[
    Holiday {
        name: "New Year's Day",
        aliases: &[
            "new year",
            "new years",
            "new years day",
            "new year day",
            "ano novo",
        ],
        rule: Rule::Fixed(1, 1),
    },
    Holiday {
        name: "Valentine's Day",
        aliases: &[
            "valentine",
            "valentines",
            "valentines day",
            "dia dos namorados",
        ],
        rule: Rule::Fixed(2, 14),
    },
    Holiday {
        name: "Carnival",
        aliases: &["carnival", "carnaval"],
        rule: Rule::Easter(-47),
    },
    Holiday {
        name: "Ash Wednesday",
        aliases: &["ash wednesday", "quarta de cinzas"],
        rule: Rule::Easter(-46),
    },
    Holiday {
        name: "April Fools' Day",
        aliases: &["april fools", "april fool", "april fools day"],
        rule: Rule::Fixed(4, 1),
    },
    Holiday {
        name: "Good Friday",
        aliases: &["good friday", "sexta santa", "sexta-feira santa"],
        rule: Rule::Easter(-2),
    },
    Holiday {
        name: "Easter Sunday",
        aliases: &["easter", "easter sunday", "pascoa", "páscoa"],
        rule: Rule::Easter(0),
    },
    Holiday {
        name: "Easter Monday",
        aliases: &["easter monday"],
        rule: Rule::Easter(1),
    },
    Holiday {
        name: "Tiradentes",
        aliases: &["tiradentes"],
        rule: Rule::Fixed(4, 21),
    },
    Holiday {
        name: "Labour Day",
        aliases: &["labour day", "labor day", "may day", "dia do trabalho"],
        rule: Rule::Fixed(5, 1),
    },
    Holiday {
        name: "Mother's Day",
        aliases: &[
            "mother",
            "mothers",
            "mothers day",
            "dia das maes",
            "dia das mães",
        ],
        rule: Rule::Nth(5, 2, 7, 0),
    },
    Holiday {
        name: "Corpus Christi",
        aliases: &["corpus christi"],
        rule: Rule::Easter(60),
    },
    Holiday {
        name: "US Independence Day",
        aliases: &[
            "4th of july",
            "fourth of july",
            "july 4th",
            "us independence day",
        ],
        rule: Rule::Fixed(7, 4),
    },
    Holiday {
        name: "Brazilian Independence Day",
        aliases: &[
            "independence day",
            "independencia",
            "independência",
            "sete de setembro",
        ],
        rule: Rule::Fixed(9, 7),
    },
    Holiday {
        name: "Children's Day",
        aliases: &[
            "children",
            "childrens day",
            "dia das criancas",
            "dia das crianças",
        ],
        rule: Rule::Fixed(10, 12),
    },
    Holiday {
        name: "Our Lady of Aparecida",
        aliases: &["aparecida", "nossa senhora aparecida"],
        rule: Rule::Fixed(10, 12),
    },
    Holiday {
        name: "Halloween",
        aliases: &["halloween"],
        rule: Rule::Fixed(10, 31),
    },
    Holiday {
        name: "All Souls' Day",
        aliases: &["all souls", "all souls day", "finados"],
        rule: Rule::Fixed(11, 2),
    },
    Holiday {
        name: "Republic Day",
        aliases: &[
            "republic day",
            "proclamacao da republica",
            "proclamação da república",
        ],
        rule: Rule::Fixed(11, 15),
    },
    Holiday {
        name: "Black Consciousness Day",
        aliases: &[
            "black consciousness",
            "consciencia negra",
            "consciência negra",
        ],
        rule: Rule::Fixed(11, 20),
    },
    Holiday {
        name: "Thanksgiving",
        aliases: &["thanksgiving"],
        rule: Rule::Nth(11, 4, 4, 0),
    },
    Holiday {
        name: "Black Friday",
        aliases: &["black friday"],
        rule: Rule::Nth(11, 4, 4, 1),
    },
    Holiday {
        name: "Christmas Eve",
        aliases: &["christmas eve", "xmas eve", "vespera de natal"],
        rule: Rule::Fixed(12, 24),
    },
    Holiday {
        name: "Christmas Day",
        aliases: &["christmas", "xmas", "natal"],
        rule: Rule::Fixed(12, 25),
    },
    Holiday {
        name: "Boxing Day",
        aliases: &["boxing day"],
        rule: Rule::Fixed(12, 26),
    },
    Holiday {
        name: "New Year's Eve",
        aliases: &[
            "new years eve",
            "new year eve",
            "nye",
            "reveillon",
            "réveillon",
        ],
        rule: Rule::Fixed(12, 31),
    },
];

/// A rule and a year in, a day number out.
fn holiday_date(rule: &Rule, year: i64) -> i64 {
    match *rule {
        Rule::Fixed(m, d) => days::days_from_civil(year, m, d),
        Rule::Easter(off) => easter_of(year) + off,
        Rule::Nth(m, n, w, plus) => nth_weekday(year, m, n, w, plus),
    }
}

#[derive(Clone, Copy, PartialEq)]
pub(super) enum Prefer {
    Next,
    Past,
}

/// Every holiday the text names, as (rank, day, name), best first.
///
/// Two ways in, and the difference is what keeps this usable while you type.
/// An exact name or alias always matches, so `nye` answers. A prefix only
/// matches from four characters, so typing towards `christmas` answers at
/// `chri` while `car` stays out of the way: without the prefix rule nothing
/// answers until the last keystroke, and with it at two characters a date row
/// lands on top of half the searches in the launcher.
///
/// Without a year the answer is the next occurrence: a holiday that has gone
/// by is almost never the one being asked about, and "christmas" in January
/// is eleven months ahead, not one behind. `Prefer::Past` is how `since`
/// looks backwards instead.
pub(super) fn match_holidays(
    text: &str,
    year: i64,
    year_given: bool,
    prefer: Prefer,
    today: i64,
    today_year: i64,
) -> Vec<(i64, i64, &'static str)> {
    let mut out: Vec<(i64, i64, &'static str)> = Vec::new();
    for h in HOLIDAYS {
        // A sort key, not a flag. 0 is a name typed in full; anything else is
        // a name still being typed, ranked by how much of it is still missing.
        // "christmas" is Christmas Day exactly and Christmas Eve only by
        // accident of the Eve falling first in the year, and "chri" is four
        // letters short of one and eight short of the other.
        let mut hit: Option<i64> = None;
        if h.name.to_lowercase() == text {
            hit = Some(0);
        }
        if hit.is_none() {
            for alias in h.aliases {
                if *alias == text {
                    hit = Some(0);
                    break;
                }
                if text.len() >= 4 && alias.starts_with(text) {
                    hit = Some(1000 + (alias.len() - text.len()) as i64);
                    break;
                }
            }
        }
        if hit.is_none() && text.len() >= 4 && h.name.to_lowercase().starts_with(text) {
            hit = Some(1000 + (h.name.len() - text.len()) as i64);
        }
        let Some(hit) = hit else { continue };

        let mut day = holiday_date(&h.rule, year);
        if !year_given && year == today_year {
            // "since" looks backwards and everything else looks forwards.
            // Without this `date:days since new year` resolved New Year to the
            // one still to come and answered with a gap measured from a date
            // in the future: a confident number for a question nobody asked.
            match prefer {
                Prefer::Past if day > today => day = holiday_date(&h.rule, today_year - 1),
                Prefer::Next if day < today => day = holiday_date(&h.rule, today_year + 1),
                _ => {}
            }
        }
        out.push((hit, day, h.name));
    }
    // The script's `sort -u` on "hit|iso|name": rank first, then the earlier
    // day, then the name — and identical lines collapse.
    out.sort_by(|a, b| a.0.cmp(&b.0).then(a.1.cmp(&b.1)).then(a.2.cmp(b.2)));
    out.dedup();
    out
}
