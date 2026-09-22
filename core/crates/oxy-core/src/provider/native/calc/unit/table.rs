//! The tables `bin/oxy-unit` carried, verbatim. Every unit the provider
//! answers for, spelled the way qalc wants it — a unit that is not here is
//! not answered, because the alternative is qalc turning the letters into a
//! product and this returning it with a straight face.
//!
//! Some of qalc's own names are traps and are spelled around here: `pt` is
//! not a point, `ct` is a centitonne rather than a carat, `st` is not a
//! stone, `qt` is not a quart, and a bare `F`, `C` and `mb` are farads,
//! coulombs and millibars.

/// Every unit the script will answer for, and the spelling qalc wants for
/// it. `None` is a refusal — qalc never gets to guess.
pub(super) fn canon(unit: &str) -> Option<&'static str> {
    let mut u = unit.to_lowercase();
    // A trailing full stop is punctuation, not part of the name.
    if u.ends_with('.') {
        u.pop();
    }
    if let Some(rest) = u.strip_prefix("the ") {
        u = rest.to_string();
    }
    u = u.replace('-', " ");
    // `km / h` is `km/h`; `m ^ 2` is `m^2` — a space next to the operator is
    // how a person writes it, and no spelling here has one.
    while u.contains(" /") || u.contains("/ ") {
        u = u.replacen(" /", "/", 1).replacen("/ ", "/", 1);
    }
    while u.contains(" ^") || u.contains("^ ") {
        u = u.replacen(" ^", "^", 1).replacen("^ ", "^", 1);
    }
    // `square feet` is `squarefeet` to the table.
    for pfx in ["square", "sq", "cubic", "cu"] {
        if let Some(rest) = u.strip_prefix(&format!("{pfx} ")) {
            u = format!("{pfx}{rest}");
            break;
        }
    }

    let canonical: &'static str = match u.as_str() {
        // -- length
        "mm" | "millimetre" | "millimeter" | "millimetres" | "millimeters" => "mm",
        "cm" | "centimetre" | "centimeter" | "centimetres" | "centimeters" => "cm",
        "dm" | "decimetre" | "decimeter" | "decimetres" | "decimeters" => "dm",
        "m" | "metre" | "meter" | "metres" | "meters" | "mtr" => "m",
        "km" | "kilometre" | "kilometer" | "kilometres" | "kilometers" | "kms" | "klick"
        | "klicks" => "km",
        "um" | "\u{b5}m" | "μm" | "micrometre" | "micrometer" | "micrometres" | "micrometers"
        | "micron" | "microns" => "μm",
        "nm" | "nanometre" | "nanometer" | "nanometres" | "nanometers" => "nm",
        "angstrom" | "angstroms" | "å" | "ångström" => "angstrom",
        "in" | "inch" | "inches" | "\"" | "″" | "ins" => "inch",
        "ft" | "foot" | "feet" | "'" | "′" | "fts" => "foot",
        "yd" | "yard" | "yards" | "yds" => "yard",
        "mi" | "mile" | "miles" => "mile",
        // `nm` is the nanometre here, because every other SI symbol in this
        // table is SI and 500 nm is light rather than 926 km of open water.
        // The nautical mile keeps `nmi` and its own name, and the reading is
        // printed on the row.
        "nmi" | "nautical mile" | "nautical miles" | "nauticalmile" | "knots distance" => {
            "nauticalmile"
        }
        "ly" | "lightyear" | "light year" | "light years" | "lightyears" => "lightyear",
        "au" | "astronomical unit" | "astronomical units" => "au",
        "thou" | "mil" | "mils" | "thous" => "thou",
        "furlong" | "furlongs" => "furlong",

        // -- mass
        "mg" | "milligram" | "milligrams" | "milligramme" | "milligrammes" => "mg",
        "ug" | "\u{b5}g" | "μg" | "microgram" | "micrograms" => "μg",
        "g" | "gram" | "grams" | "gramme" | "grammes" | "gr." => "g",
        "kg" | "kilo" | "kilos" | "kilogram" | "kilograms" | "kilogramme" | "kilogrammes"
        | "kgs" => "kg",
        "t" | "ton" | "tons" | "tonne" | "tonnes" | "metric ton" | "metric tons"
        | "metric tonne" => "tonne",
        "short ton" | "short tons" | "us ton" | "us tons" => "s_ton",
        "long ton" | "long tons" | "imperial ton" | "uk ton" => "l_ton",
        "lb" | "lbs" | "pound" | "pounds" | "#" => "pound",
        "oz" | "ounce" | "ounces" | "ozs" => "ounce",
        "st" | "stone" | "stones" | "stn" => "stone",
        "ct" | "carat" | "carats" | "karat" | "karats" => "carat",
        "grain" | "grains" => "gr",
        "cwt" | "hundredweight" => "cwt",
        "slug" | "slugs" => "slug",

        // -- volume
        "ml" | "millilitre" | "milliliter" | "millilitres" | "milliliters" | "cc" | "ccm" => "mL",
        "cl" | "centilitre" | "centiliter" | "centilitres" | "centiliters" => "cL",
        "dl" | "decilitre" | "deciliter" | "decilitres" | "deciliters" => "dL",
        "l" | "ltr" | "litre" | "liter" | "litres" | "liters" => "L",
        "gal" | "gallon" | "gallons" | "us gallon" | "us gallons" => "gal",
        "imperial gallon" | "imperial gallons" | "uk gallon" | "uk gallons" => "gal_UK",
        "qt" | "qts" | "quart" | "quarts" | "us quart" | "liquid quart" => "liquid_quart",
        "imperial quart" | "uk quart" | "imperial quarts" => "imperial_quart",
        "pt" | "pts" | "pint" | "pints" | "us pint" | "liquid pint" => "liquid_pint",
        "imperial pint" | "uk pint" | "imperial pints" | "uk pints" => "imperial_pint",
        "cup" | "cups" => "cup",
        "floz" | "fl oz" | "fl. oz" | "fluid ounce" | "fluid ounces" | "fl ozs" => "fl_oz",
        "tbsp" | "tbs" | "tblsp" | "tablespoon" | "tablespoons" => "tbsp",
        "tsp" | "teaspoon" | "teaspoons" => "tsp",
        "dram" | "drams" | "dr" => "dr",
        "barrel" | "barrels" | "bbl" | "bbls" => "bbl",
        "m3" | "m^3" | "cubicm" | "cubicmetre" | "cubicmeter" | "cubicmetres" | "cubicmeters"
        | "cubic m" => "m^3",
        "cm3" | "cm^3" | "cubiccm" | "cubiccentimetre" | "cubiccentimeter" => "cm^3",
        "ft3" | "ft^3" | "cubicft" | "cubicfoot" | "cubicfeet" => "ft^3",
        "in3" | "in^3" | "cubicin" | "cubicinch" | "cubicinches" => "in^3",

        // -- temperature. A bare "f" is farads to qalc and Fahrenheit to a
        // person.
        "f" | "°f" | "deg f" | "degf" | "degreesf" | "fahrenheit" | "farenheit"
        | "fahrenheight" => "°F",
        "c" | "°c" | "deg c" | "degc" | "degreesc" | "celsius" | "celcius" | "centigrade"
        | "celsius." => "°C",
        "k" | "kelvin" | "kelvins" | "°k" => "K",
        "r" | "rankine" | "°r" => "°R",

        // -- speed
        "kph"
        | "kmh"
        | "kmph"
        | "km/h"
        | "km per hour"
        | "kilometres per hour"
        | "kilometers per hour" => "km/h",
        "mph" | "mi/h" | "miles per hour" | "miles an hour" => "mph",
        "m/s" | "mps" | "metres per second" | "meters per second" => "m/s",
        "ft/s" | "fps" | "feet per second" => "ft/s",
        "knot" | "knots" | "kn" | "kts" => "knot",

        // -- data. qalc reads "mb" as millibar and "gb" as gram·bel, so
        // `2 gb in mb` used to come back as 5.08E-64 kg·m⁵.
        "b" | "byte" | "bytes" => "byte",
        "kb" | "kilobyte" | "kilobytes" => "kilobyte",
        "mb" | "megabyte" | "megabytes" | "meg" | "megs" => "megabyte",
        "gb" | "gigabyte" | "gigabytes" | "gig" | "gigs" => "gigabyte",
        "tb" | "terabyte" | "terabytes" => "terabyte",
        "pb" | "petabyte" | "petabytes" => "petabyte",
        "kib" | "kibibyte" | "kibibytes" => "kibibyte",
        "mib" | "mebibyte" | "mebibytes" => "mebibyte",
        "gib" | "gibibyte" | "gibibytes" => "gibibyte",
        "tib" | "tebibyte" | "tebibytes" => "tebibyte",
        "bit" | "bits" => "bit",
        "kbit" | "kilobit" | "kilobits" => "kilobit",
        "mbit" | "megabit" | "megabits" => "megabit",
        "gbit" | "gigabit" | "gigabits" => "gigabit",

        // -- area
        "m2" | "m^2" | "sqm" | "squarem" | "squaremetre" | "squaremeter" | "squaremetres"
        | "squaremeters" | "square m" => "m^2",
        "km2" | "km^2" | "sqkm" | "squarekm" | "squarekilometre" | "squarekilometer"
        | "squarekilometres" | "squarekilometers" => "km^2",
        "cm2" | "cm^2" | "sqcm" | "squarecm" | "squarecentimetre" | "squarecentimeter" => "cm^2",
        "mm2" | "mm^2" | "sqmm" | "squaremm" | "squaremillimetre" | "squaremillimeter" => "mm^2",
        "ft2" | "ft^2" | "sqft" | "squareft" | "squarefoot" | "squarefeet" | "sq feet" => "ft^2",
        "in2" | "in^2" | "sqin" | "squarein" | "squareinch" | "squareinches" => "in^2",
        "yd2" | "yd^2" | "sqyd" | "squareyd" | "squareyard" | "squareyards" => "yd^2",
        "mi2" | "mi^2" | "sqmi" | "squaremi" | "squaremile" | "squaremiles" => "mi^2",
        "acre" | "acres" => "acre",
        "ha" | "hectare" | "hectares" => "hectare",

        // -- energy
        "j" | "joule" | "joules" => "joule",
        "kj" | "kilojoule" | "kilojoules" => "kilojoule",
        "mj" | "megajoule" | "megajoules" => "megajoule",
        "cal" | "calorie" | "calories" => "cal_th",
        "kcal" | "kilocalorie" | "kilocalories" => "kilocalorie",
        "wh" | "watt hour" | "watt hours" | "watthour" => "Wh",
        "kwh" | "kilowatt hour" | "kilowatt hours" | "kilowatthour" => "kWh",
        "mwh" | "megawatt hour" | "megawatt hours" => "MWh",
        "btu" | "btus" | "british thermal unit" | "british thermal units" => "Btu",
        "ev" | "electronvolt" | "electronvolts" => "eV",
        "erg" | "ergs" => "erg",
        "therm" | "therms" => "therm",

        // -- power
        "w" | "watt" | "watts" => "watt",
        "kw" | "kilowatt" | "kilowatts" => "kilowatt",
        "mw" | "megawatt" | "megawatts" => "megawatt",
        "gw" | "gigawatt" | "gigawatts" => "gigawatt",
        "hp" | "horsepower" | "horsepowers" | "bhp" => "horsepower",

        // -- pressure
        "pa" | "pascal" | "pascals" => "pascal",
        "kpa" | "kilopascal" | "kilopascals" => "kilopascal",
        "hpa" | "hectopascal" | "hectopascals" => "hectopascal",
        "mpa" | "megapascal" | "megapascals" => "megapascal",
        "bar" | "bars" => "bar",
        "mbar" | "millibar" | "millibars" => "millibar",
        "psi" | "pounds per square inch" => "psi",
        "atm" | "atmosphere" | "atmospheres" => "atm",
        "mmhg" => "mmHg",
        "torr" => "Torr",
        "inhg" => "inHg",

        // -- angle
        "deg" | "degree" | "degrees" | "°" => "deg",
        "rad" | "radian" | "radians" => "radian",
        "grad" | "grads" | "gradian" | "gradians" => "gradian",
        "arcmin" | "arcminute" | "arcminutes" => "arcmin",
        "arcsec" | "arcsecond" | "arcseconds" => "arcsec",
        "turn" | "turns" | "rev" | "revolution" | "revolutions" => "turn",

        // -- frequency
        "hz" | "hertz" => "hertz",
        "khz" | "kilohertz" => "kilohertz",
        "mhz" | "megahertz" => "megahertz",
        "ghz" | "gigahertz" => "gigahertz",
        // qalc reads rpm as an angular velocity, so `60 rpm to Hz` came back
        // as "6.28319 Hz·rad". Per-minute is the reading people mean by it.
        "rpm" | "revolutions per minute" | "bpm" | "beats per minute" => "(1/minute)",

        // -- fuel
        "mpg" | "miles per gallon" => "mpg",
        "l/100km" | "l/100 km" | "litres per 100km" | "liters per 100km" => "L/(100 km)",
        "km/l" | "km per litre" | "km per liter" => "km/L",

        // -- time. Last, because "m" and "s" belong to length and to seconds
        // and a bare "m" is metres more often than it is minutes.
        "ms" | "millisecond" | "milliseconds" | "msec" | "msecs" => "millisecond",
        "us" | "\u{b5}s" | "μs" | "microsecond" | "microseconds" => "microsecond",
        "ns" | "nanosecond" | "nanoseconds" => "nanosecond",
        "s" | "sec" | "secs" | "second" | "seconds" => "second",
        "min" | "mins" | "minute" | "minutes" => "minute",
        "h" | "hr" | "hrs" | "hour" | "hours" => "hour",
        "d" | "day" | "days" => "day",
        "wk" | "wks" | "week" | "weeks" => "week",
        "fortnight" | "fortnights" => "fortnight",
        "mo" | "month" | "months" => "month",
        "yr" | "yrs" | "year" | "years" => "year",
        "decade" | "decades" => "(10 year)",
        "century" | "centuries" => "(100 year)",

        _ => {
            // A plural nobody spelled out above, once, rather than twice in
            // every line of the table.
            if u.ends_with('s') && !u.ends_with("ss") {
                return canon(&u[..u.len() - 1]);
            }
            return None;
        }
    };
    Some(canonical)
}

/// The currency codes and names qalc's exchange rates cover, kept apart from
/// the unit table because "pound" is a mass until the other side is money.
pub(super) fn currency(unit: &str) -> Option<&'static str> {
    let mut u = unit.to_lowercase();
    if u.ends_with('s') && !u.ends_with("ss") {
        u.pop();
    }
    let code: &'static str = match u.as_str() {
        "usd" | "$" | "us dollar" | "american dollar" | "buck" | "dollar" => "USD",
        "eur" | "€" | "euro" => "EUR",
        // A bare "pound" is a mass in the unit table above and only reaches
        // this function when the other side of the conversion is already
        // money.
        "gbp" | "£" | "pound sterling" | "sterling" | "quid" | "british pound" | "pound" => "GBP",
        "jpy" | "¥" | "yen" => "JPY",
        "cny" | "rmb" | "yuan" | "renminbi" => "CNY",
        "brl" | "r$" | "real" | "reais" | "reai" | "brazilian real" => "BRL",
        "cad" | "canadian dollar" => "CAD",
        "aud" | "australian dollar" => "AUD",
        "nzd" => "NZD",
        "chf" | "swiss franc" | "franc" => "CHF",
        "inr" | "₹" | "rupee" | "indian rupee" => "INR",
        "krw" | "₩" | "won" => "KRW",
        "rub" | "₽" | "ruble" | "rouble" => "RUB",
        "mxn" | "mexican peso" => "MXN",
        "ars" => "ARS",
        "clp" => "CLP",
        "cop" => "COP",
        "sek" | "swedish krona" => "SEK",
        "nok" | "norwegian krone" => "NOK",
        "dkk" => "DKK",
        "pln" | "zloty" | "złoty" => "PLN",
        "czk" => "CZK",
        "huf" => "HUF",
        "ron" => "RON",
        "try" | "turkish lira" | "lira" => "TRY",
        "zar" | "rand" => "ZAR",
        "php" => "PHP",
        "thb" | "baht" => "THB",
        "sgd" => "SGD",
        "hkd" => "HKD",
        "twd" => "TWD",
        "ils" | "₪" | "shekel" => "ILS",
        "aed" | "dirham" => "AED",
        "sar" | "riyal" => "SAR",
        "ngn" | "naira" => "NGN",
        "egp" => "EGP",
        "uah" | "hryvnia" => "UAH",
        "vnd" | "dong" => "VND",
        "idr" | "rupiah" => "IDR",
        "myr" | "ringgit" => "MYR",
        "isk" => "ISK",
        "btc" | "bitcoin" => "BTC",
        "eth" | "ethereum" => "ETH",
        _ => return None,
    };
    Some(code)
}

/// Which family a canonical unit belongs to. Both sides of a conversion have
/// to agree, which is what turns "5 kg to mile" from a wrong answer into no
/// answer.
pub(super) fn family_of(canonical: &str) -> Option<&'static str> {
    Some(match canonical {
        "mm" | "cm" | "dm" | "m" | "km" | "μm" | "nm" | "angstrom" | "inch" | "foot" | "yard"
        | "mile" | "nauticalmile" | "lightyear" | "au" | "thou" | "furlong" => "length",
        "mg" | "μg" | "g" | "kg" | "tonne" | "s_ton" | "l_ton" | "pound" | "ounce" | "stone"
        | "carat" | "gr" | "cwt" | "slug" => "mass",
        "mL" | "cL" | "dL" | "L" | "gal" | "gal_UK" | "liquid_quart" | "imperial_quart"
        | "liquid_pint" | "imperial_pint" | "cup" | "fl_oz" | "tbsp" | "tsp" | "dr" | "bbl"
        | "m^3" | "cm^3" | "ft^3" | "in^3" => "volume",
        "°F" | "°C" | "K" | "°R" => "temperature",
        "km/h" | "mph" | "m/s" | "ft/s" | "knot" => "speed",
        "byte" | "kilobyte" | "megabyte" | "gigabyte" | "terabyte" | "petabyte" | "kibibyte"
        | "mebibyte" | "gibibyte" | "tebibyte" | "bit" | "kilobit" | "megabit" | "gigabit" => {
            "data"
        }
        "m^2" | "km^2" | "cm^2" | "mm^2" | "ft^2" | "in^2" | "yd^2" | "mi^2" | "acre"
        | "hectare" => "area",
        "joule" | "kilojoule" | "megajoule" | "cal_th" | "kilocalorie" | "Wh" | "kWh" | "MWh"
        | "Btu" | "eV" | "erg" | "therm" => "energy",
        "watt" | "kilowatt" | "megawatt" | "gigawatt" | "horsepower" => "power",
        "pascal" | "kilopascal" | "hectopascal" | "megapascal" | "bar" | "millibar" | "psi"
        | "atm" | "mmHg" | "Torr" | "inHg" => "pressure",
        "deg" | "radian" | "gradian" | "arcmin" | "arcsec" | "turn" => "angle",
        "hertz" | "kilohertz" | "megahertz" | "gigahertz" | "(1/minute)" => "frequency",
        "mpg" | "L/(100 km)" | "km/L" => "fuel",
        "millisecond" | "microsecond" | "nanosecond" | "second" | "minute" | "hour" | "day"
        | "week" | "fortnight" | "month" | "year" | "(10 year)" | "(100 year)" => "time",
        _ => return None,
    })
}

/// What else to offer when nobody named a target, or when one was named and
/// the next question is the same number in a third unit. Ordered by what is
/// useful at that size: five feet in kilometres is a true answer and a
/// useless row.
pub(super) fn targets_for(family: &str, src: &str) -> Vec<&'static str> {
    match family {
        "length" => match src {
            "mm" | "cm" | "μm" | "nm" | "angstrom" | "thou" | "inch" => {
                vec!["cm", "inch", "mm", "foot"]
            }
            "m" | "foot" | "yard" => vec!["foot", "m", "inch", "yard"],
            _ => vec!["km", "mile", "m", "foot"],
        },
        "mass" => match src {
            "mg" | "μg" | "g" | "ounce" | "carat" | "gr" | "dr" => {
                vec!["g", "ounce", "pound", "kg"]
            }
            _ => vec!["kg", "pound", "stone", "ounce"],
        },
        "volume" => match src {
            "mL" | "cL" | "tsp" | "tbsp" | "fl_oz" | "dr" => {
                vec!["mL", "fl_oz", "tbsp", "cup"]
            }
            _ => vec!["L", "gal", "cup", "mL"],
        },
        "temperature" => vec!["°C", "°F", "K"],
        "speed" => vec!["km/h", "mph", "m/s", "knot"],
        "data" => vec!["megabyte", "gigabyte", "mebibyte", "gibibyte"],
        "area" => vec!["m^2", "ft^2", "hectare", "acre"],
        "energy" => vec!["kilojoule", "kilocalorie", "Wh"],
        "power" => vec!["watt", "kilowatt", "horsepower"],
        "pressure" => vec!["bar", "psi", "kilopascal", "atm"],
        "angle" => vec!["deg", "radian"],
        "frequency" => vec!["hertz", "kilohertz", "megahertz", "(1/minute)"],
        "fuel" => vec!["mpg", "L/(100 km)", "km/L"],
        "time" => vec!["hour", "minute", "second", "day"],
        _ => Vec::new(),
    }
}

/// How heavy a cup of it is, in grams per millilitre, so "1 cup of flour in
/// grams" is a number instead of "3.98529E−40 g·B²·L²". Kitchen figures, and
/// the one used is printed on the row, because a cup of flour is between
/// 120 g and 145 g depending on who packed it.
pub(super) fn density_of(ingredient: &str) -> Option<&'static str> {
    let mut i = ingredient
        .to_lowercase()
        .replace("all-purpose", "all purpose");
    for pfx in [
        "all purpose ",
        "plain ",
        "white ",
        "granulated ",
        "caster ",
        "whole ",
    ] {
        if let Some(rest) = i.strip_prefix(pfx) {
            i = rest.to_string();
            break;
        }
    }
    if i.ends_with('s') {
        i.pop();
    }
    Some(match i.as_str() {
        "water" => "1.00",
        "milk" => "1.03",
        "cream" => "1.01",
        "flour" => "0.53",
        "bread flour" => "0.55",
        "cake flour" => "0.48",
        "sugar" => "0.85",
        "brown sugar" => "0.93",
        "icing sugar" | "powdered sugar" | "confectioners sugar" => "0.50",
        "butter" | "margarine" => "0.911",
        "oil" | "olive oil" | "vegetable oil" => "0.92",
        "honey" => "1.42",
        "syrup" | "maple syrup" => "1.33",
        "rice" => "0.80",
        "oat" | "rolled oat" => "0.40",
        "salt" => "1.20",
        "cocoa" | "cocoa powder" => "0.42",
        "yogurt" | "yoghurt" => "1.03",
        "peanut butter" => "1.08",
        "corn starch" | "cornstarch" | "cornflour" => "0.53",
        _ => return None,
    })
}

/// The name a person reads, for the names qalc wants written its way.
pub(super) fn pretty(canonical: &str) -> &str {
    match canonical {
        "m^2" => "m²",
        "km^2" => "km²",
        "cm^2" => "cm²",
        "mm^2" => "mm²",
        "ft^2" => "ft²",
        "in^2" => "in²",
        "yd^2" => "yd²",
        "mi^2" => "mi²",
        "m^3" => "m³",
        "cm^3" => "cm³",
        "ft^3" => "ft³",
        "in^3" => "in³",
        "cal_th" => "calorie",
        "gal_UK" => "imperial gallon",
        "s_ton" => "short ton",
        "l_ton" => "long ton",
        "liquid_quart" => "quart",
        "liquid_pint" => "pint",
        "imperial_quart" => "imperial quart",
        "imperial_pint" => "imperial pint",
        "fl_oz" => "fl oz",
        "(10 year)" => "decade",
        "(100 year)" => "century",
        "(1/minute)" => "per minute",
        other => other,
    }
}

/// A short unit name that means two things is read one way here, and the row
/// says which. The list is only the ones people actually collide on: `nm` is
/// either light or open water, `t` is either a tonne or a tesla, and `oz` is
/// either a weight or a volume. A bare c, f and k are not on the list: the
/// answer already ends in °C, °F or K, which says the reading out loud.
pub(super) fn ambiguity(typed: &str) -> Option<&'static str> {
    Some(match typed.to_lowercase().as_str() {
        "nm" => "nm read as nanometre — nmi is the nautical mile",
        "t" => "t read as tonne",
        "oz" => "oz read as weight — fl oz is the volume",
        "st" => "st read as stone",
        "ct" => "ct read as carat",
        "cal" => "cal read as the 4.184 J calorie — kcal is the food Calorie",
        _ => return None,
    })
}
