use super::parse::{Kind, Question, awk_num, g_fmt, parse};
use super::table::{canon, currency, density_of};
use super::*;

fn asked(q: &str) -> Question {
    parse(q).expect("should parse")
}

fn exprs_of(q: &Question) -> Vec<String> {
    plan(q).iter().map(|(e, _)| e.clone()).collect()
}

#[test]
fn whitespace_and_nbsp_collapse() {
    let q = asked("  5\u{a0}km   in  miles ");
    assert_eq!(q.src, "km");
    assert_eq!(q.as_typed, "5 km in miles");
    assert!(parse("   ").is_none());
}

#[test]
fn punctuation_and_politeness() {
    let q = asked("please 5 km in miles!");
    assert_eq!(q.src, "km");
    assert_eq!(q.tgt, Some("mile"));
    assert_eq!(q.as_typed, "please 5 km in miles!");
}

#[test]
fn thousands_grouping() {
    let q = asked("100,000 km in miles");
    assert_eq!(q.quantity, "100000");
    assert!(q.notes.iter().any(|n| n.contains("thousands separator")));
    // "1,5 m" is left alone — and ",5 m" is no unit either side names, so
    // the question is refused rather than guessed at.
    assert!(parse("1,5 m in cm").is_none());
    let q = asked("1,234,567 m in km");
    assert_eq!(q.quantity, "1234567");
    let q = asked("100 000 km in miles");
    assert_eq!(q.quantity, "100000");
}

#[test]
fn case_carries_no_meaning() {
    assert_eq!(asked("5 KM IN MILES").src, "km");
    assert_eq!(asked("5 Km In Miles").tgt, Some("mile"));
}

#[test]
fn glued_number_and_unit() {
    let q = asked("180f in c");
    assert_eq!(q.quantity, "180");
    assert_eq!(q.src, "°F");
    assert_eq!(q.tgt, Some("°C"));

    assert_eq!(asked("5km in miles").src, "km");
    assert_eq!(asked("20 °C to °F").src, "°C");
    assert_eq!(asked("-40 c in f").quantity, "-40");
    // the exponent survives: "1e3 m" is not "1 e3 m"
    let q = asked("1e3 m in km");
    assert_eq!(q.quantity, "1e3");
    assert_eq!(q.src, "m");
}

#[test]
fn arrows_and_verbs() {
    assert_eq!(asked("5km -> miles").tgt, Some("mile"));
    assert_eq!(asked("5km => miles").tgt, Some("mile"));
    assert_eq!(asked("5 km = ? miles").tgt, Some("mile"));
    assert_eq!(asked("convert 5km to miles").quantity, "5");
    assert_eq!(asked("what is 5km in miles").quantity, "5");
}

#[test]
fn number_words() {
    assert_eq!(asked("half a mile in km").quantity, "0.5");
    assert_eq!(asked("a mile in km").quantity, "1");
    // "a hundred"/"one hundred" are the script's dead rules: "a "/"one "
    // rewrites first and "hundred" is left as a unit nobody named — silence,
    // like the script.
    assert!(parse("one hundred miles in km").is_none());
    assert!(parse("a hundred miles in km").is_none());
    assert_eq!(asked("two miles in km").quantity, "2");
}

#[test]
fn how_many_is_the_same_question_backwards() {
    let q = asked("how many feet in a mile");
    assert!(q.asked_backwards);
    assert_eq!(q.quantity, "1");
    assert_eq!(q.src, "mile");
    assert_eq!(q.tgt, Some("foot"));
    assert!(q.notes.iter().any(|n| n == "read as 1 mile to foot"));

    let q = asked("how many cm in an inch");
    assert_eq!(q.src, "inch");
    assert_eq!(q.tgt, Some("cm"));

    let q = asked("how many seconds are in a year");
    assert_eq!(q.src, "year");
    assert_eq!(q.tgt, Some("second"));
}

#[test]
fn inversions() {
    let q = asked("miles from 5km");
    assert!(q.asked_backwards);
    assert_eq!(q.src, "km");
    assert_eq!(q.tgt, Some("mile"));
    assert!(q.notes.iter().any(|n| n == "read as 5 km to mile"));

    let q = asked("cm in 6 ft");
    assert!(q.asked_backwards);
    assert_eq!(q.quantity, "6");
    assert_eq!(q.src, "foot");
    assert_eq!(q.tgt, Some("cm"));
}

#[test]
fn heights_and_weights_are_two_numbers() {
    let q = asked("6ft2 in cm");
    assert_eq!(q.quantity, "74");
    assert_eq!(q.src, "inch");
    assert!(q.notes.iter().any(|n| n == "read as 74 inches"));

    assert_eq!(asked("6'2 in cm").quantity, "74");
    assert_eq!(asked("6 ft 2 in in cm").quantity, "74");
    let q = asked("5'11\"");
    assert_eq!(q.quantity, "71");
    assert_eq!(q.tgt, None);

    let q = asked("7lb 8oz in kg");
    assert_eq!(q.quantity, "120");
    assert_eq!(q.src, "ounce");

    let q = asked("12 stone 6 in kg");
    assert_eq!(q.quantity, "174");
    assert_eq!(q.src, "pound");
}

#[test]
fn feet_and_inches_is_the_mixed_case() {
    let q = asked("187 cm in feet and inches");
    assert!(q.mixed);
    assert_eq!(q.tgt, Some("foot"));
    // different families do not mix
    assert!(parse("187 cm in feet and ounces").is_none());
}

#[test]
fn density_bridges_volume_and_mass() {
    let q = asked("1 cup of flour in grams");
    assert_eq!(q.src, "cup");
    assert_eq!(q.tgt, Some("g"));
    assert_eq!(q.density, Some("0.53"));
    assert_eq!(q.ingredient.as_deref(), Some("flour"));
    assert!(q.notes.iter().any(|n| n == "flour at 0.53 g/mL"));

    let q = asked("2 cups sugar in g");
    assert_eq!(q.quantity, "2");
    assert_eq!(q.density, Some("0.85"));

    let q = asked("250g of flour in cups");
    assert_eq!(q.src, "g");
    assert_eq!(q.tgt, Some("cup"));

    // nothing named to weigh → refused, not guessed
    assert!(parse("1 cup in grams").is_none());
    assert!(parse("5 kg in miles").is_none());
}

#[test]
fn fractions_and_grouped_quantities() {
    let q = asked("1 1/2 cups in ml");
    assert_eq!(q.quantity, "(1+1/2)");
    assert_eq!(q.src, "cup");
    let q = asked("1/2 mile in m");
    assert_eq!(q.quantity, "1/2");
}

#[test]
fn the_gate_is_silence_not_guesses() {
    assert!(parse("").is_none());
    assert!(parse("firefox").is_none());
    assert!(parse("5").is_none());
    assert!(parse("5 xyzzy in km").is_none());
    assert!(parse("5 km in xyzzy").is_none());
    assert!(parse("5 apples in oranges").is_none());
    // a currency with nothing to convert it to parses, then has no plan
    let q = asked("5 usd");
    assert_eq!(q.kind, Kind::Currency);
    assert!(exprs_of(&q).is_empty());
    assert!(parse("how many feets").is_none());
}

#[test]
fn half_a_conversion_word() {
    let q = asked("5 km in");
    assert_eq!(q.src, "km");
    assert_eq!(q.tgt, None);
}

#[test]
fn currency_is_kept_apart_until_money_is_named() {
    let q = asked("100 usd in eur");
    assert_eq!(q.kind, Kind::Currency);
    assert_eq!(q.src, "USD");
    assert_eq!(q.tgt, Some("EUR"));

    // a pound is a mass until the other side of the sentence is money
    let q = asked("50 pounds in dollars");
    assert_eq!(q.kind, Kind::Currency);
    assert_eq!(q.src, "GBP");
    assert!(q.notes.iter().any(|n| n == "pounds read as GBP"));

    let q = asked("50 pounds in kg");
    assert_eq!(q.kind, Kind::Unit);
    assert_eq!(q.src, "pound");

    let q = asked("100,000 yen to usd");
    assert_eq!(q.kind, Kind::Currency);
    assert_eq!(q.src, "JPY");
}

#[test]
fn ambiguity_is_printed() {
    let q = asked("500nm in m");
    assert!(
        q.notes
            .iter()
            .any(|n| n == "nm read as nanometre — nmi is the nautical mile")
    );
    let q = asked("1 pint in ml");
    assert!(q.notes.iter().any(|n| n == "US liquid measure"));
}

#[test]
fn plan_for_a_targeted_unit() {
    let q = asked("20 miles in km");
    let plan = plan(&q);
    let exprs: Vec<&str> = plan.iter().map(|(e, _)| e.as_str()).collect();
    assert_eq!(
        exprs,
        vec![
            "20 mile to km",
            "1 mile to km",
            "20 mile to m",
            "20 mile to foot"
        ]
    );
    assert!(matches!(plan[0].1, Leg::Answer));
    assert!(matches!(plan[1].1, Leg::Rate));
}

#[test]
fn plan_for_a_family() {
    let q = asked("20 miles");
    let plan = plan(&q);
    let exprs: Vec<&str> = plan.iter().map(|(e, _)| e.as_str()).collect();
    assert_eq!(
        exprs,
        vec!["20 mile to km", "20 mile to m", "20 mile to foot"]
    );

    // a temperature carries no rate: a scale with an offset has none
    let q = asked("180f in c");
    assert_eq!(exprs_of(&q), vec!["180 °F to °C", "180 °F to K"]);

    // currency: answer and rate, no family
    let q = asked("100 usd in eur");
    assert_eq!(exprs_of(&q), vec!["100 USD to EUR", "1 USD to EUR"]);
}

#[test]
fn plan_for_a_density_bridge() {
    let q = asked("1 cup of flour in grams");
    let p = plan(&q);
    assert_eq!(p[0].0, "(1 cup) * (0.53 g/mL) to g");
    // no rate on a bridged answer; the family legs still ask "1 cup to …"
    assert!(p.iter().all(|(_, l)| !matches!(l, Leg::Rate)));
    let q = asked("250g of flour in cups");
    assert_eq!(exprs_of(&q)[0], "(250 g) / (0.53 g/mL) to cup");
}

#[test]
fn qalc_spellings() {
    assert_eq!(canon("pt"), Some("liquid_pint"));
    assert_eq!(canon("pint"), Some("liquid_pint"));
    assert_eq!(canon("imperial pint"), Some("imperial_pint"));
    assert_eq!(canon("mb"), Some("megabyte"));
    assert_eq!(canon("gb"), Some("gigabyte"));
    assert_eq!(canon("mbar"), Some("millibar"));
    assert_eq!(canon("st"), Some("stone"));
    assert_eq!(canon("ct"), Some("carat"));
    assert_eq!(canon("f"), Some("°F"));
    assert_eq!(canon("c"), Some("°C"));
    assert_eq!(canon("r"), Some("°R"));
    assert_eq!(canon("b"), Some("byte"));
    assert_eq!(canon("rpm"), Some("(1/minute)"));
    assert_eq!(canon("bpm"), Some("(1/minute)"));
    assert_eq!(canon("nmi"), Some("nauticalmile"));
    assert_eq!(canon("nm"), Some("nm"));
    assert_eq!(canon("fl. oz"), Some("fl_oz"));
    assert_eq!(canon("square feet"), Some("ft^2"));
    assert_eq!(canon("sq ft"), Some("ft^2"));
    assert_eq!(canon("km / h"), Some("km/h"));
    assert_eq!(canon("m ^ 2"), Some("m^2"));
    assert_eq!(canon("the mile"), Some("mile"));
    assert_eq!(canon("miles."), Some("mile"));
    assert_eq!(canon("klicks"), Some("km"));
    assert_eq!(canon("\u{b5}m"), Some("μm"));
    assert_eq!(canon("mo"), Some("month"));
    assert_eq!(canon("in"), Some("inch"));
    assert_eq!(canon("firefox"), None);
    assert_eq!(canon("glass"), None); // *ss never unpluralises
    assert_eq!(canon("feets"), Some("foot")); // one retry, one s
    assert_eq!(canon("foots"), Some("foot"));
}

#[test]
fn currency_codes() {
    assert_eq!(currency("usd"), Some("USD"));
    assert_eq!(currency("dollars"), Some("USD"));
    assert_eq!(currency("pounds"), Some("GBP"));
    assert_eq!(currency("yen"), Some("JPY"));
    assert_eq!(currency("reais"), Some("BRL"));
    assert_eq!(currency("złoty"), Some("PLN"));
    assert_eq!(currency("bitcoin"), Some("BTC"));
    assert_eq!(currency("mile"), None);
}

#[test]
fn densities() {
    assert_eq!(density_of("flour"), Some("0.53"));
    assert_eq!(density_of("all-purpose flour"), Some("0.53"));
    assert_eq!(density_of("oats"), Some("0.40"));
    assert_eq!(density_of("brown sugar"), Some("0.93"));
    assert_eq!(density_of("confectioners sugar"), Some("0.50"));
    assert_eq!(density_of("mile"), None);
}

#[test]
fn awk_and_g() {
    assert_eq!(awk_num("1.2.3"), 1.2);
    assert_eq!(awk_num("74"), 74.0);
    assert_eq!(g_fmt(74.0), "74");
    assert_eq!(g_fmt(0.5), "0.5");
    assert_eq!(g_fmt(120.0), "120");
    assert_eq!(g_fmt(1.5), "1.5");
}

#[test]
fn usable_and_readable() {
    assert!(!usable("5 km to miles", "5 km to miles"));
    assert!(!usable("  ", "x"));
    assert!(!usable("error: nope", "x"));
    assert!(!usable("no digits here", "x"));
    assert!(usable("3.1 mi", "x"));
    assert_eq!(readable("12.0 fl_oz"), "12.0 fl oz");
    assert_eq!(readable("5 cal_th"), "5 cal");
    assert_eq!(readable("6 min^-1"), "6 /min");
}

#[test]
fn rows_have_the_script_shape() {
    let r = row("unit-answer", "3.1 mi", "5 km in miles", "d", "hero", 99000);
    assert_eq!(r["id"], "unit-answer");
    assert_eq!(r["title"], "3.1 mi");
    assert_eq!(r["subtitle"], "5 km in miles");
    assert_eq!(r["view"], "hero");
    assert_eq!(r["score"], 99000);
    assert_eq!(r["exec"], "printf %s 3.1\\ mi | wl-copy");
    assert_eq!(
        r["actions"][0],
        json!({"title": "Copy Answer", "shortcut": "↵", "exec": "printf %s 3.1\\ mi | wl-copy"})
    );
    assert_eq!(
        r["actions"][1]["exec"],
        "printf %s 5\\ km\\ in\\ miles\\ =\\ 3.1\\ mi | wl-copy"
    );
}

#[test]
fn assemble_hero_and_family() {
    let mut q = asked("20 miles in km");
    let p = plan(&q);
    let out: Vec<String> = p.iter().map(|_| "32.1869 km".to_string()).collect();
    let NativeOutcome::Rows(rows) = assemble(&mut q, &p, &out, None) else {
        panic!("wanted rows");
    };
    assert_eq!(rows.len(), 3); // hero + 2 family rows; the rate is detail
    assert_eq!(rows[0]["view"], "hero");
    assert_eq!(rows[0]["score"], 99000);
    assert_eq!(rows[0]["detail"], "1 mile = 32.1869 km");
    assert_eq!(rows[1]["view"], "list");
    assert_eq!(rows[1]["score"], 94000);

    // the "x" line has no digit and is skipped, like the script's usable()
    let mut q = asked("20 miles");
    let plan2 = plan(&q);
    let out = vec!["32 km".to_string(), "32000 m".to_string(), "x".to_string()];
    let NativeOutcome::Rows(rows) = assemble(&mut q, &plan2, &out, None) else {
        panic!("wanted rows");
    };
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0]["score"], 95000);
    assert_eq!(rows[1]["score"], 94900);
}

#[test]
fn assemble_refuses_echoes() {
    let mut q = asked("20 miles in km");
    let plan = plan(&q);
    let out: Vec<String> = plan.iter().map(|(e, _)| e.clone()).collect();
    assert!(matches!(
        assemble(&mut q, &plan, &out, None),
        NativeOutcome::Empty
    ));
}
