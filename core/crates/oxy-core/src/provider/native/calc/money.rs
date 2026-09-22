use std::sync::OnceLock;

use fancy_regex::Regex;

use super::numbers::NumberToken;
use super::units::{unit_after_re, unit_before_re};

// ------------------------------------------------------------------- money

fn symbol_code(sym: &str) -> Option<&'static str> {
    Some(match sym.to_lowercase().as_str() {
        "r$" => "BRL",
        "us$" | "u$s" => "USD",
        "c$" | "ca$" => "CAD",
        "a$" | "au$" => "AUD",
        "nz$" => "NZD",
        "hk$" => "HKD",
        "s$" => "SGD",
        "nt$" => "TWD",
        "mx$" => "MXN",
        "cn¥" => "CNY",
        "₺" => "TRY",
        "₩" => "KRW",
        "₫" => "VND",
        "₴" => "UAH",
        "฿" => "THB",
        "₪" => "ILS",
        _ => return None,
    })
}

fn currency_name(name: &str) -> Option<&'static str> {
    Some(match name.to_lowercase().as_str() {
        "dolar" | "dolares" | "dólar" | "dólares" | "dolari" | "dollari" => "USD",
        "reais" => "BRL",
        "iene" | "ienes" | "yens" | "yenes" => "JPY",
        "esterlina" | "esterlinas" | "sterline" | "sterlina" => "GBP",
        "franc" | "francs" | "franco" | "francos" | "franchi" | "franken" => "CHF",
        "yuan" | "yuanes" | "renminbi" | "rmb" => "CNY",
        "rupee" | "rupees" | "rupia" | "rupias" | "roupie" => "INR",
        "rupiah" => "IDR",
        "shekel" | "shekels" | "sheqel" => "ILS",
        "baht" => "THB",
        "ringgit" => "MYR",
        "dong" => "VND",
        "hryvnia" | "hryvnias" => "UAH",
        "rublo" | "rublos" | "rubel" => "RUB",
        "zloty" | "zlotys" => "PLN",
        "forint" | "forints" => "HUF",
        "rands" => "ZAR",
        "wons" => "KRW",
        "dirham" | "dirhams" => "AED",
        "lira" | "liras" => "TRY",
        _ => return None,
    })
}

fn currency_in_context(name: &str) -> Option<&'static str> {
    Some(match name.to_lowercase().as_str() {
        "pound" | "pounds" | "libra" | "libras" | "livre" | "livres" | "pfund" => "GBP",
        "real" => "BRL",
        _ => return None,
    })
}

fn is_preposition(word: &str) -> bool {
    matches!(
        word.to_lowercase().as_str(),
        "to" | "in" | "into" | "as" | "para" | "pra" | "em" | "en" | "de" | "of"
    )
}

fn is_fiat(token: &str) -> bool {
    matches!(
        token.to_lowercase().as_str(),
        "usd"
            | "eur"
            | "gbp"
            | "jpy"
            | "chf"
            | "cad"
            | "aud"
            | "nzd"
            | "cny"
            | "rmb"
            | "brl"
            | "mxn"
            | "ars"
            | "clp"
            | "cop"
            | "pen"
            | "uyu"
            | "inr"
            | "rub"
            | "krw"
            | "sek"
            | "nok"
            | "dkk"
            | "pln"
            | "czk"
            | "huf"
            | "ron"
            | "try"
            | "zar"
            | "ils"
            | "aed"
            | "sar"
            | "egp"
            | "ngn"
            | "kes"
            | "hkd"
            | "sgd"
            | "twd"
            | "thb"
            | "idr"
            | "myr"
            | "php"
            | "vnd"
            | "isk"
            | "uah"
            | "$"
            | "€"
            | "£"
            | "¥"
            | "₹"
            | "₽"
            | "r$"
            | "us$"
            | "dollar"
            | "dollars"
            | "euro"
            | "euros"
            | "pound"
            | "pounds"
            | "yen"
            | "real"
            | "reais"
            | "peso"
            | "pesos"
            | "rupee"
            | "rupees"
    )
}

fn symbol_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        let keys = [
            "r\\$", "us\\$", "u\\$s", "c\\$", "ca\\$", "a\\$", "au\\$", "nz\\$", "hk\\$", "s\\$",
            "nt\\$", "mx\\$", "cn¥", "₺", "₩", "₫", "₴", "฿", "₪",
        ];
        // `(?i)`, as the JS carried: `US$ 50` and `us$ 50` are the same price.
        Regex::new(&format!(
            "(?i)(^|[^0-9A-Za-z])({})(?![A-Za-z])",
            keys.join("|")
        ))
        .expect("symbol regex compiles")
    })
}

fn name_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        let names = [
            "dolari",
            "dollari",
            "dolares",
            "dólares",
            "dolar",
            "dólar",
            "reais",
            "ienes",
            "iene",
            "yenes",
            "yens",
            "esterlinas",
            "esterlina",
            "sterline",
            "sterlina",
            "francos",
            "franchi",
            "franco",
            "francs",
            "franc",
            "franken",
            "yuanes",
            "yuan",
            "renminbi",
            "rmb",
            "rupees",
            "rupee",
            "rupias",
            "rupia",
            "roupie",
            "rupiah",
            "shekels",
            "shekel",
            "sheqel",
            "baht",
            "ringgit",
            "dong",
            "hryvnias",
            "hryvnia",
            "rublos",
            "rublo",
            "rubel",
            "zlotys",
            "zloty",
            "forints",
            "forint",
            "rands",
            "wons",
            "dirhams",
            "dirham",
            "liras",
            "lira",
        ];
        Regex::new(&format!("(?i)\\b({})\\b", names.join("|"))).expect("name regex compiles")
    })
}

fn context_name_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r"(?i)\b(pounds|pound|libras|libra|livres|livre|pfund|real)\b")
            .expect("context regex compiles")
    })
}

fn tokens_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"[a-z$¥£€₹₽]+").unwrap())
}

fn target_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"(?i)\bto\s+([^\s]+)\s*$").unwrap())
}

/// Is any token in here money, ignoring the ones that are only money in
/// company? This is what decides whether `pounds` is sterling or mass.
pub(super) fn names_currency(text: &str) -> bool {
    // Lowercased first, the way the original read it — the rewrite above
    // emits uppercase codes, and `BRL` is fiat the same as `brl`.
    let t = text.to_lowercase();
    for m in tokens_re().find_iter(&t).map_while(|m| m.ok()) {
        let token = m.as_str();
        if is_fiat(token) && currency_in_context(token).is_none() {
            return true;
        }
        if currency_name(token).is_some() {
            return true;
        }
    }
    false
}

/// Symbols and names to ISO codes, so everything downstream is looking at one
/// spelling of a currency.
pub(super) fn with_currencies(text: &str) -> String {
    let out = symbol_re()
        .replace_all(text, |caps: &fancy_regex::Captures| {
            let lead = caps.get(1).map(|m| m.as_str()).unwrap_or("");
            let sym = caps.get(2).map(|m| m.as_str()).unwrap_or("");
            format!("{}{}", lead, symbol_code(sym).unwrap_or(sym))
        })
        .into_owned();
    let out = name_re()
        .replace_all(&out, |caps: &fancy_regex::Captures| {
            let name = caps.get(1).map(|m| m.as_str()).unwrap_or("");
            currency_name(name).unwrap_or(name).to_string()
        })
        .into_owned();
    if !names_currency(&out) {
        return out;
    }
    context_name_re()
        .replace_all(&out, |caps: &fancy_regex::Captures| {
            let name = caps.get(1).map(|m| m.as_str()).unwrap_or("");
            currency_in_context(name).unwrap_or(name).to_string()
        })
        .into_owned()
}

/// Is this literal a price? The unit written against it decides, and only a
/// currency quoted to two decimals counts: `1.005 btc` is a real quantity.
pub(super) fn priced_in(text: &str, token: &NumberToken) -> bool {
    if let Ok(Some(caps)) = unit_before_re().captures(&text[..token.start])
        && let Some(word) = caps.get(1)
        && is_fiat(word.as_str())
    {
        return true;
    }

    if let Ok(Some(caps)) = unit_after_re().captures(&text[token.end..])
        && let Some(word) = caps.get(1)
    {
        let word = word.as_str();
        if is_fiat(word) {
            return true;
        }
        if !is_preposition(word) {
            return false;
        }
    }

    if let Ok(Some(caps)) = target_re().captures(text)
        && let Some(target) = caps.get(1)
    {
        return is_fiat(target.as_str());
    }
    false
}

/// Money, and specifically money the answer will be *in*: the right-hand side
/// of a conversion decides.
pub(super) fn is_money(text: &str) -> bool {
    let t = text.to_lowercase();
    if let Ok(Some(caps)) = target_re().captures(&t) {
        return is_fiat(caps.get(1).unwrap().as_str());
    }
    tokens_re()
        .find_iter(&t)
        .map_while(|m| m.ok())
        .any(|m| is_fiat(m.as_str()))
}
