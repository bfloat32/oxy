//! What a typed word becomes: `resolve_zone` and `label_for_zone`, ported
//! from `bin/oxy-timezone` — the accent fold, the `Etc/GMT` sign flip, the
//! configured-label match, the abbreviation and country tables, and the
//! scored scan over the IANA list. The list itself is
//! `jiff::tz::db().available()` where the script asked `timedatectl` and
//! cached the answer in `~/.cache/oxy/zones`.

use std::sync::OnceLock;

use super::zones::Board;

/// POSIX `[[:space:]]` — the six bytes the script's patterns and trims mean
/// by whitespace. `char::is_whitespace` is wider, and nobody needs a
/// non-breaking space to separate anything here.
pub(super) fn ws(c: char) -> bool {
    matches!(c, ' ' | '\t' | '\n' | '\x0B' | '\x0C' | '\r')
}

/// `iconv -f UTF-8 -t ASCII//TRANSLIT | tr -d "'\`^~\""` — an accented letter
/// reads as its base ("são paulo" is "sao paulo"), combining marks and the
/// quote-and-tilde set are dropped, and anything the table has no answer for
/// becomes `?` — iconv's own "cannot say" marker, which matches nothing
/// either way.
pub(super) fn fold_accents(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        fold_char(c, &mut out);
    }
    out
}

fn fold_char(c: char, out: &mut String) {
    if c.is_ascii() {
        if !matches!(c, '\'' | '`' | '^' | '~' | '"') {
            out.push(c);
        }
        return;
    }
    let t: &str = match c {
        'à' | 'á' | 'â' | 'ã' | 'ä' | 'å' | 'ā' | 'ă' | 'ą' | 'ȁ' | 'ȃ' | 'ạ' | 'ả' | 'ấ' | 'ầ'
        | 'ẩ' | 'ẫ' | 'ậ' | 'ắ' | 'ằ' | 'ẳ' | 'ẵ' | 'ặ' => "a",
        'ç' | 'ć' | 'ĉ' | 'ċ' | 'č' => "c",
        'ď' | 'đ' | 'ð' => "d",
        'è' | 'é' | 'ê' | 'ë' | 'ē' | 'ĕ' | 'ė' | 'ę' | 'ě' | 'ȅ' | 'ȇ' | 'ẹ' | 'ẻ' | 'ẽ' | 'ế'
        | 'ề' | 'ể' | 'ễ' | 'ệ' => "e",
        'ĝ' | 'ğ' | 'ġ' | 'ģ' => "g",
        'ĥ' | 'ħ' => "h",
        'ì' | 'í' | 'î' | 'ï' | 'ī' | 'ĭ' | 'į' | 'ı' | 'ị' | 'ỉ' => "i",
        'ĵ' => "j",
        'ķ' | 'ĸ' => "k",
        'ĺ' | 'ļ' | 'ľ' | 'ŀ' | 'ł' => "l",
        'ñ' | 'ń' | 'ņ' | 'ň' | 'ŉ' | 'ŋ' => "n",
        'ò' | 'ó' | 'ô' | 'õ' | 'ö' | 'ø' | 'ō' | 'ŏ' | 'ő' | 'ȍ' | 'ȏ' | 'ơ' | 'ọ' | 'ỏ' | 'ố'
        | 'ồ' | 'ổ' | 'ỗ' | 'ộ' | 'ớ' | 'ờ' | 'ở' | 'ỡ' | 'ợ' => "o",
        'ŕ' | 'ŗ' | 'ř' => "r",
        'ś' | 'ŝ' | 'ş' | 'ș' | 'š' => "s",
        'ţ' | 'ť' | 'ŧ' => "t",
        'ù' | 'ú' | 'û' | 'ü' | 'ū' | 'ŭ' | 'ů' | 'ű' | 'ų' | 'ư' | 'ụ' | 'ủ' | 'ứ' | 'ừ' | 'ử'
        | 'ữ' | 'ự' => "u",
        'ŵ' => "w",
        'ý' | 'ÿ' | 'ỳ' | 'ỵ' | 'ỷ' | 'ỹ' => "y",
        'ź' | 'ż' | 'ž' => "z",
        'ß' => "ss",
        'æ' => "ae",
        'œ' => "oe",
        'þ' => "th",
        // iconv writes these as the ASCII quote/dash they look like, and the
        // `tr -d` set then removes the quotes; the dash lives to become `_`.
        '\u{2018}' | '\u{2019}' | '\u{201A}' | '\u{201B}' => "'",
        '\u{00AB}' | '\u{00BB}' | '\u{2039}' | '\u{203A}' | '\u{201C}' | '\u{201D}' => "\"",
        '\u{2010}'..='\u{2015}' | '\u{2212}' => "-",
        // A combining mark on an ASCII letter (NFD input): dropped, the way
        // iconv drops it after transliterating the letter itself.
        '\u{0300}'..='\u{036F}' => "",
        _ => "?",
    };
    for ch in t.chars() {
        if !matches!(ch, '\'' | '`' | '^' | '~' | '"') {
            out.push(ch);
        }
    }
}

/// The IANA list — what `timedatectl list-timezones` printed into the
/// `~/.cache/oxy/zones` file. jiff hands over the same names already loaded,
/// sorted, with no subprocess and no cache to go stale under a tzdata update.
pub(super) fn all_zones() -> &'static [String] {
    static ALL: OnceLock<Vec<String>> = OnceLock::new();
    ALL.get_or_init(|| {
        let mut v: Vec<String> = jiff::tz::db()
            .available()
            .map(|n| n.as_str().to_string())
            .collect();
        v.sort_unstable();
        v
    })
    .as_slice()
}

/// `resolve_zone` — `None` is the shell's `return 1`.
pub(super) fn resolve_zone(text: &str, board: &Board) -> Option<String> {
    let needle = fold_accents(&text.to_lowercase());

    // An offset, before anything rewrites the sign out of it.
    //
    // The POSIX zones spell offsets backwards: Etc/GMT+2 is two hours
    // *behind* UTC. So `tz:gmt+2` used to find Etc/GMT+2 through the ordinary
    // name search and answer with UTC-2, four hours from what was asked for,
    // with a row that said "GMT+2" over the top of it. That is the exact
    // shape of wrong this keyword must never be, so offsets are read here and
    // the sign is flipped on the way into the zone name.
    if let Some(off) = offset(&needle) {
        return off;
    }

    let mut needle: String = needle
        .chars()
        .map(|c| if ws(c) { '_' } else { c })
        .collect();
    needle = needle.replace('-', "_").replace('.', "");
    if let Some(rest) = needle.strip_prefix('/') {
        needle = rest.to_string();
    }
    if needle.is_empty() {
        return None;
    }

    // A configured label wins outright: naming a zone after a person is only
    // worth doing if typing the person finds it.
    for (label, zone) in &board.pairs {
        if norm_label(label) == needle {
            return Some(zone.clone());
        }
    }
    for (label, zone) in &board.pairs {
        if norm_label(label).starts_with(&needle) {
            return Some(zone.clone());
        }
    }

    // Abbreviations that are not in the IANA list at all, and the handful of
    // names shorter than the initials rule below.
    if let Some(zone) = table(&needle, &board.local) {
        return Some(zone);
    }

    list_match(&needle)
}

/// What to call a zone. A configured label wins, so somebody who named a
/// zone after a person reads the person back rather than the nearest
/// capital.
pub(super) fn label_for_zone(zone: &str, board: &Board) -> String {
    for (label, z) in &board.pairs {
        if z == zone {
            return label.clone();
        }
    }
    // Etc/GMT-5 is UTC+5, and putting the zone's own name on the row would
    // tell the reader the opposite of the time underneath it.
    if let Some(rest) = zone.strip_prefix("Etc/GMT") {
        let b = rest.as_bytes();
        if (b.len() == 2 || b.len() == 3)
            && matches!(b[0], b'+' | b'-')
            && b[1..].iter().all(u8::is_ascii_digit)
        {
            let digits = &rest[1..];
            return if b[0] == b'-' {
                format!("UTC+{digits}")
            } else {
                format!("UTC-{digits}")
            };
        }
    }
    zone.rsplit('/').next().unwrap_or(zone).replace('_', " ")
}

/// The label normalization the script applies before comparing: lowercase,
/// whitespace becomes `_`. No accent fold, no other punctuation handling —
/// labels compare against the already-folded needle.
fn norm_label(label: &str) -> String {
    label
        .to_lowercase()
        .chars()
        .map(|c| if ws(c) { '_' } else { c })
        .collect()
}

/// The `^[[:space:]]*(utc|gmt)?[[:space:]]*([+-])[[:space:]]*([0-9]{1,2})
/// ([:.]?([0-9]{2}))?[[:space:]]*$` shape. The outer `Some` is "the pattern
/// matched" — `Some(None)` included, the regex matching while the minutes
/// say there is no Etc zone for it: half an hour out is a real place with a
/// real name, and guessing which one is worse than saying so.
fn offset(needle: &str) -> Option<Option<String>> {
    let mut s = needle.trim_matches(ws);
    if let Some(rest) = s.strip_prefix("utc").or_else(|| s.strip_prefix("gmt")) {
        s = rest.trim_start_matches(ws);
    }
    let (plus, rest) = match s.as_bytes().first() {
        Some(b'+') => (true, &s[1..]),
        Some(b'-') => (false, &s[1..]),
        _ => return None,
    };
    let rest = rest.trim_start_matches(ws);
    // The regex backtracks: "530" reads as 5:30, not 53 followed by one
    // digit, so the shorter hour wins whenever the longer one cannot close
    // the pattern.
    for hlen in [2usize, 1] {
        if rest.len() < hlen || !rest.as_bytes()[..hlen].iter().all(u8::is_ascii_digit) {
            continue;
        }
        let Ok(h) = rest[..hlen].parse::<i64>() else {
            continue;
        };
        let tail = rest[hlen..].trim_end_matches(ws);
        let m: i64 = if tail.is_empty() {
            0
        } else {
            let t = tail.strip_prefix([':', '.']).unwrap_or(tail);
            if t.len() != 2 || !t.bytes().all(|b| b.is_ascii_digit()) {
                continue;
            }
            match t.parse() {
                Ok(m) => m,
                Err(_) => continue,
            }
        };
        if m != 0 {
            return Some(None);
        }
        if h > 14 {
            return Some(None);
        }
        if h == 0 {
            return Some(Some("UTC".to_string()));
        }
        return Some(Some(if plus {
            format!("Etc/GMT-{h}")
        } else {
            format!("Etc/GMT+{h}")
        }));
    }
    None
}

/// The `case` statement verbatim: abbreviations, countries by the city
/// almost everyone in them means, the cities the IANA list does not carry,
/// and `me`/`here`/`local` for the machine's own zone. Order matters —
/// `case` takes the first match, so `pt` is the Los Angeles arm and
/// `portugal` is what is left of it.
fn table(needle: &str, local: &str) -> Option<String> {
    let zone = match needle {
        "utc" | "gmt" | "z" | "zulu" => "UTC",
        "est" | "edt" | "et" | "nyc" => "America/New_York",
        "pst" | "pdt" | "pt" | "sf" | "bayarea" | "cali" => "America/Los_Angeles",
        "cst" | "cdt" | "ct" => "America/Chicago",
        "mst" | "mdt" | "mt" => "America/Denver",
        "cet" | "cest" => "Europe/Berlin",
        "bst" | "uk" | "gb" | "england" | "britain" => "Europe/London",

        // Countries, by the city almost everyone in them means. The IANA
        // list is a list of cities, so a country either misses entirely
        // (germany, france) or matches the wrong thing: "spain" found Port
        // of Spain, in Trinidad, and "canada" found Yukon.
        "spain" | "es" => "Europe/Madrid",
        "germany" | "de" | "deutschland" => "Europe/Berlin",
        "france" | "fr" => "Europe/Paris",
        "italy" | "it" => "Europe/Rome",
        "netherlands" | "nl" | "holland" => "Europe/Amsterdam",
        "poland" | "pl" => "Europe/Warsaw",
        "sweden" | "se" => "Europe/Stockholm",
        "norway" | "no" => "Europe/Oslo",
        "ireland" | "ie" => "Europe/Dublin",
        "ukraine" | "ua" => "Europe/Kyiv",
        "turkey" | "tr" => "Europe/Istanbul",
        "china" | "cn" => "Asia/Shanghai",
        "japan" | "jp" => "Asia/Tokyo",
        "korea" | "kr" | "southkorea" => "Asia/Seoul",
        "india" | "in_" | "bharat" => "Asia/Kolkata",
        "singapore" | "sg" => "Asia/Singapore",
        "indonesia" | "id" => "Asia/Jakarta",
        "philippines" | "ph" => "Asia/Manila",
        "vietnam" | "vn" => "Asia/Ho_Chi_Minh",
        "thailand" | "th" => "Asia/Bangkok",
        "israel" | "il" => "Asia/Jerusalem",
        "uae" | "dubai" | "ae" => "Asia/Dubai",
        "brazil" | "br" | "brasil" => "America/Sao_Paulo",
        "argentina" | "ar" => "America/Argentina/Buenos_Aires",
        "chile" | "cl" => "America/Santiago",
        "colombia" | "co" => "America/Bogota",
        "peru" | "pe" => "America/Lima",
        "mexico" | "mx" => "America/Mexico_City",
        "canada" | "ca" => "America/Toronto",
        "australia" | "au" => "Australia/Sydney",
        "newzealand" | "nz" => "Pacific/Auckland",
        "southafrica" | "za" => "Africa/Johannesburg",
        "nigeria" | "ng" => "Africa/Lagos",
        "egypt" | "eg" => "Africa/Cairo",
        "kenya" | "ke" => "Africa/Nairobi",
        // "pt" was claimed by Los Angeles above; what is left of Portugal.
        "portugal" => "Europe/Lisbon",

        // American cities the list spells differently, or not at all.
        "washington" | "dc" | "washingtondc" => "America/New_York",
        "seattle" | "wa" => "America/Los_Angeles",
        "austin" | "dallas" | "houston" | "texas" => "America/Chicago",
        "boston" | "philly" | "philadelphia" | "miami" | "atlanta" => "America/New_York",
        "ist" => "Asia/Kolkata",
        "jst" => "Asia/Tokyo",
        "kst" => "Asia/Seoul",
        "aest" | "aedt" | "awst" | "acst" => "Australia/Sydney",
        "brt" | "brasilia" => "America/Sao_Paulo",
        "wet" | "west" => "Europe/Lisbon",
        "eet" | "eest" => "Europe/Athens",
        "msk" | "moscow" => "Europe/Moscow",
        "sgt" => "Asia/Singapore",

        // Cities the IANA list does not carry, under the names people call
        // them. Every one of these answered "No timezone matches" and every
        // one of them is somewhere a colleague sits.
        "bangalore" | "bengaluru" | "mumbai" | "bombay" | "delhi" | "new_delhi" | "hyderabad"
        | "chennai" | "pune" => "Asia/Kolkata",
        "barcelona" | "valencia" | "seville" | "sevilla" | "bilbao" => "Europe/Madrid",
        "munich" | "muenchen" | "munchen" | "frankfurt" | "hamburg" | "cologne" | "koln"
        | "stuttgart" | "dusseldorf" => "Europe/Berlin",
        "milan" | "milano" | "turin" | "torino" | "naples" | "napoli" | "florence" | "venice"
        | "bologna" => "Europe/Rome",
        "lyon" | "marseille" | "nice" | "toulouse" | "bordeaux" | "lille" => "Europe/Paris",
        "porto" | "oporto" | "coimbra" | "braga" | "faro" => "Europe/Lisbon",
        "edinburgh" | "manchester" | "glasgow" | "liverpool" | "birmingham" | "leeds"
        | "bristol" | "cardiff" | "reading" => "Europe/London",
        "geneva" | "zurich" | "basel" | "bern" | "lausanne" => "Europe/Zurich",
        "rio" | "rio_de_janeiro" | "belo_horizonte" | "curitiba" | "porto_alegre"
        | "florianopolis" | "campinas" => "America/Sao_Paulo",
        "osaka" | "kyoto" | "nagoya" | "yokohama" | "kobe" | "fukuoka" => "Asia/Tokyo",
        "beijing" | "peking" | "shenzhen" | "guangzhou" | "chengdu" | "hangzhou" | "wuhan" => {
            "Asia/Shanghai"
        }
        "busan" | "incheon" => "Asia/Seoul",
        "tel_aviv" | "telaviv" | "haifa" => "Asia/Jerusalem",
        "abu_dhabi" | "abudhabi" | "sharjah" => "Asia/Dubai",
        "cape_town" | "capetown" | "durban" | "pretoria" | "joburg" | "jozi" => {
            "Africa/Johannesburg"
        }
        "melbourne" | "canberra" | "hobart" => "Australia/Sydney",
        "wellington" | "christchurch" => "Pacific/Auckland",
        "montreal" | "ottawa" | "quebec" => "America/Toronto",
        "calgary" | "edmonton" => "America/Edmonton",
        "las_vegas" | "vegas" | "san_diego" | "sacramento" | "portland" | "san_jose"
        | "palo_alto" | "oakland" => "America/Los_Angeles",
        "orlando" | "tampa" | "charlotte" | "raleigh" | "pittsburgh" | "baltimore" => {
            "America/New_York"
        }
        "minneapolis" | "milwaukee" | "kansas_city" | "st_louis" | "nashville" | "memphis"
        | "new_orleans" => "America/Chicago",
        "guadalajara" | "monterrey" => "America/Mexico_City",

        // "us" is a country, not the pronoun: `tz:us` is asking about
        // America, and the zone almost everybody means by it is the eastern
        // one.
        "us" | "usa" | "america" | "united_states" => "America/New_York",

        "here" | "me" | "local" | "home" | "my_time" => return Some(local.to_string()),
        _ => return None,
    };
    Some(zone.to_string())
}

/// The awk pass over the IANA list: exact name, then city, then squashed,
/// then initials, then prefix, then substring — best score wins and a
/// shorter name breaks a tie, which keeps Asia/Tokyo ahead of anything that
/// merely contains it. A substring hit needs three characters: with two,
/// "of" is inside Sofia and `tz:of` answered with the time in Bulgaria.
fn list_match(needle: &str) -> Option<String> {
    let n = needle;
    let len = n.chars().count();
    let mut best: Option<(std::cmp::Reverse<i32>, usize, &str)> = None;
    for z in all_zones() {
        let lz = z.to_lowercase();
        let city = lz.rsplit('/').next().unwrap_or(lz.as_str());
        let squash: String = city.chars().filter(|c| *c != '_').collect();
        // The initials of a multi-word city: "sp" for Sao_Paulo, "ny" for
        // New_York, "la" for Los_Angeles — the way people abbreviate cities
        // out loud, so it ranks above a plain substring hit.
        let ini: String = city.split('_').filter_map(|p| p.chars().next()).collect();

        let s = if lz == n {
            100
        } else if city == n {
            90
        } else if squash == n {
            85
        } else if len >= 2 && ini == n {
            80
        } else if city.starts_with(n) {
            70
        } else if squash.starts_with(n) {
            65
        } else if len >= 3 && city.contains(n) {
            45
        } else if len >= 3 && lz.contains(n) {
            30
        } else {
            0
        };
        if s == 0 {
            continue;
        }
        // `sort -k1,1nr -k2,2nr`: score descending, name length ascending,
        // the name itself as the last resort.
        let key = (std::cmp::Reverse(s), z.len(), z.as_str());
        if best.is_none_or(|prev| key < prev) {
            best = Some(key);
        }
    }
    best.map(|(_, _, z)| z.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::Settings;

    fn board() -> Board {
        Board::load(&Settings::merge(&serde_json::json!({})))
    }

    fn board_with() -> Board {
        Board::load(&Settings::merge(&serde_json::json!({
            "timezones": [
                {"label": "Ana", "zone": "Europe/Lisbon"},
                {"zone": "America/Toronto"}
            ]
        })))
    }

    #[test]
    fn accents_fold_and_punctuation_normalizes() {
        assert_eq!(fold_accents("são paulo"), "sao paulo");
        assert_eq!(fold_accents("Köln"), "Koln");
        assert_eq!(fold_accents("it's~fine"), "itsfine");
        assert_eq!(fold_accents("e\u{0301}tude"), "etude");
    }

    #[test]
    fn offsets_flip_the_posix_sign() {
        let b = board();
        assert_eq!(resolve_zone("gmt+2", &b).as_deref(), Some("Etc/GMT-2"));
        assert_eq!(resolve_zone("utc+5", &b).as_deref(), Some("Etc/GMT-5"));
        assert_eq!(resolve_zone("gmt-3", &b).as_deref(), Some("Etc/GMT+3"));
        assert_eq!(resolve_zone("utc+0", &b).as_deref(), Some("UTC"));
        assert_eq!(resolve_zone("gmt+0:00", &b).as_deref(), Some("UTC"));
        // A half hour has no Etc zone, and neither does an hour past 14.
        assert_eq!(resolve_zone("+5:30", &b), None);
        assert_eq!(resolve_zone("gmt+530", &b), None);
        assert_eq!(resolve_zone("utc-15", &b), None);
        assert_eq!(resolve_zone("+1400", &b).as_deref(), Some("Etc/GMT-14"));
    }

    #[test]
    fn the_tables_answer_what_the_list_cannot() {
        let b = board();
        assert_eq!(
            resolve_zone("sao paulo", &b).as_deref(),
            Some("America/Sao_Paulo")
        );
        assert_eq!(
            resolve_zone("são paulo", &b).as_deref(),
            Some("America/Sao_Paulo")
        );
        assert_eq!(
            resolve_zone("bangalore", &b).as_deref(),
            Some("Asia/Kolkata")
        );
        assert_eq!(resolve_zone("nice", &b).as_deref(), Some("Europe/Paris"));
        assert_eq!(
            resolve_zone("rio", &b).as_deref(),
            Some("America/Sao_Paulo")
        );
        assert_eq!(resolve_zone("us", &b).as_deref(), Some("America/New_York"));
        assert_eq!(resolve_zone("jst", &b).as_deref(), Some("Asia/Tokyo"));
        assert_eq!(resolve_zone("japan", &b).as_deref(), Some("Asia/Tokyo"));
        assert_eq!(resolve_zone("spain", &b).as_deref(), Some("Europe/Madrid"));
        assert_eq!(
            resolve_zone("pt", &b).as_deref(),
            Some("America/Los_Angeles")
        );
        assert_eq!(resolve_zone("me", &b).as_deref(), Some(b.local.as_str()));
    }

    #[test]
    fn the_iana_list_scores_the_way_the_awk_did() {
        let b = board();
        assert_eq!(
            resolve_zone("new york", &b).as_deref(),
            Some("America/New_York")
        );
        assert_eq!(
            resolve_zone("saopaulo", &b).as_deref(),
            Some("America/Sao_Paulo")
        );
        assert_eq!(resolve_zone("of", &b), None);
        assert_eq!(resolve_zone("zzznotazone", &b), None);
        assert_eq!(
            resolve_zone("europe/lisbon", &b).as_deref(),
            Some("Europe/Lisbon")
        );
    }

    #[test]
    fn a_configured_label_wins_exact_then_prefix() {
        let b = board_with();
        assert_eq!(resolve_zone("Ana", &b).as_deref(), Some("Europe/Lisbon"));
        assert_eq!(resolve_zone("an", &b).as_deref(), Some("Europe/Lisbon"));
        assert_eq!(label_for_zone("Europe/Lisbon", &b), "Ana");
        assert_eq!(label_for_zone("America/Toronto", &b), "America/Toronto");
        assert_eq!(label_for_zone("Etc/GMT-5", &b), "UTC+5");
        assert_eq!(label_for_zone("Etc/GMT+2", &b), "UTC-2");
        assert_eq!(label_for_zone("Asia/Ho_Chi_Minh", &b), "Ho Chi Minh");
    }

    #[test]
    fn the_env_line_wins_over_the_file() {
        let s = Settings::merge(&serde_json::json!({
            "timezones": [{"label": "File", "zone": "Europe/Lisbon"}],
            "extensionSettings": {"tz": {"zones": "Tokyo, nowhere-nope, Ana:Spain"}}
        }));
        let b = Board::load(&s);
        // "Ana:Spain" is not a zone name — the env keeps what resolves.
        assert_eq!(
            b.pairs,
            vec![("Tokyo".to_string(), "Asia/Tokyo".to_string())]
        );
        let s = Settings::merge(&serde_json::json!({
            "timezones": [{"label": "File", "zone": "Europe/Lisbon"}],
            "extensionSettings": {"tz": {"zones": "nowhere-nope, zzz"}}
        }));
        // Nothing resolved: the file's list stays.
        assert_eq!(Board::load(&s).pairs[0].0, "File");
    }
}
