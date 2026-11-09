//! The unit converter (SUPERSEARCH.md 3, "units"):
//! `<number> <unit> (to|in|as) <unit>`, e.g. "5 km to mi", "30c to f",
//! "2gb in mb".
//!
//! Every unit is a factor (and, for temperatures, an offset) to one base
//! unit per kind: meters, kilograms, kelvin, liters, bytes, seconds, m/s.
//! No currencies (that would need the internet).

use super::calc::format_sig;
use super::{Hit, HitAction, HitIcon, Provider, ProviderId, Query, score};

/// The ruler glyph (Segoe Fluent Icons).
const GLYPH: char = '\u{ED5E}';
/// Significant digits in the answer.
const DIGITS: i32 = 6;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Length,
    Mass,
    Temperature,
    Volume,
    Data,
    Time,
    Speed,
}

struct Unit {
    /// How it is shown in the answer.
    symbol: &'static str,
    /// What may be typed (lowercase).
    names: &'static [&'static str],
    kind: Kind,
    /// In base units: base = value × factor + offset.
    factor: f64,
    offset: f64,
}

const fn unit(
    symbol: &'static str,
    names: &'static [&'static str],
    kind: Kind,
    factor: f64,
) -> Unit {
    Unit {
        symbol,
        names,
        kind,
        factor,
        offset: 0.0,
    }
}

use Kind::*;

const UNITS: &[Unit] = &[
    // Length (meters).
    unit(
        "mm",
        &[
            "mm",
            "millimeter",
            "millimeters",
            "millimetre",
            "millimetres",
        ],
        Length,
        0.001,
    ),
    unit(
        "cm",
        &[
            "cm",
            "centimeter",
            "centimeters",
            "centimetre",
            "centimetres",
        ],
        Length,
        0.01,
    ),
    unit(
        "m",
        &["m", "meter", "meters", "metre", "metres"],
        Length,
        1.0,
    ),
    unit(
        "km",
        &["km", "kilometer", "kilometers", "kilometre", "kilometres"],
        Length,
        1000.0,
    ),
    unit("in", &["in", "inch", "inches", "\""], Length, 0.0254),
    unit("ft", &["ft", "foot", "feet", "'"], Length, 0.3048),
    unit("yd", &["yd", "yard", "yards"], Length, 0.9144),
    unit("mi", &["mi", "mile", "miles"], Length, 1609.344),
    unit(
        "nmi",
        &["nmi", "nautical mile", "nautical miles"],
        Length,
        1852.0,
    ),
    // Mass (kilograms).
    unit("mg", &["mg", "milligram", "milligrams"], Mass, 1e-6),
    unit("g", &["g", "gram", "grams"], Mass, 0.001),
    unit(
        "kg",
        &["kg", "kilo", "kilos", "kilogram", "kilograms"],
        Mass,
        1.0,
    ),
    unit("t", &["t", "tonne", "tonnes"], Mass, 1000.0),
    unit("oz", &["oz", "ounce", "ounces"], Mass, 0.028_349_523_125),
    unit("lb", &["lb", "lbs", "pound", "pounds"], Mass, 0.453_592_37),
    unit("st", &["st", "stone", "stones"], Mass, 6.350_293_18),
    // Temperature (kelvin).
    Unit {
        symbol: "°C",
        names: &["c", "°c", "celsius", "degc"],
        kind: Temperature,
        factor: 1.0,
        offset: 273.15,
    },
    Unit {
        symbol: "°F",
        names: &["f", "°f", "fahrenheit", "degf"],
        kind: Temperature,
        factor: 5.0 / 9.0,
        offset: 459.67 * 5.0 / 9.0,
    },
    unit("K", &["k", "kelvin"], Temperature, 1.0),
    // Volume (liters; US measures).
    unit(
        "ml",
        &[
            "ml",
            "milliliter",
            "milliliters",
            "millilitre",
            "millilitres",
        ],
        Volume,
        0.001,
    ),
    unit("cl", &["cl"], Volume, 0.01),
    unit("dl", &["dl"], Volume, 0.1),
    unit(
        "l",
        &["l", "liter", "liters", "litre", "litres"],
        Volume,
        1.0,
    ),
    unit(
        "m³",
        &["m3", "m³", "cubic meter", "cubic meters"],
        Volume,
        1000.0,
    ),
    unit("gal", &["gal", "gallon", "gallons"], Volume, 3.785_411_784),
    unit("qt", &["qt", "quart", "quarts"], Volume, 0.946_352_946),
    unit("pt", &["pt", "pint", "pints"], Volume, 0.473_176_473),
    unit("cup", &["cup", "cups"], Volume, 0.236_588_236_5),
    unit(
        "fl oz",
        &["floz", "fl oz", "fluid ounce", "fluid ounces"],
        Volume,
        0.029_573_529_562_5,
    ),
    unit(
        "tbsp",
        &["tbsp", "tablespoon", "tablespoons"],
        Volume,
        0.014_786_764_781_25,
    ),
    unit(
        "tsp",
        &["tsp", "teaspoon", "teaspoons"],
        Volume,
        0.004_928_921_593_75,
    ),
    // Data size (bytes). KB = 1000 bytes, KiB = 1024.
    unit("bit", &["bit", "bits"], Data, 0.125),
    unit("B", &["b", "byte", "bytes"], Data, 1.0),
    unit("KB", &["kb", "kilobyte", "kilobytes"], Data, 1e3),
    unit("MB", &["mb", "megabyte", "megabytes"], Data, 1e6),
    unit("GB", &["gb", "gigabyte", "gigabytes"], Data, 1e9),
    unit("TB", &["tb", "terabyte", "terabytes"], Data, 1e12),
    unit("PB", &["pb", "petabyte", "petabytes"], Data, 1e15),
    unit("KiB", &["kib", "kibibyte", "kibibytes"], Data, 1024.0),
    unit("MiB", &["mib", "mebibyte", "mebibytes"], Data, 1_048_576.0),
    unit(
        "GiB",
        &["gib", "gibibyte", "gibibytes"],
        Data,
        1_073_741_824.0,
    ),
    unit(
        "TiB",
        &["tib", "tebibyte", "tebibytes"],
        Data,
        1_099_511_627_776.0,
    ),
    // Time (seconds).
    unit("ms", &["ms", "millisecond", "milliseconds"], Time, 0.001),
    unit("s", &["s", "sec", "secs", "second", "seconds"], Time, 1.0),
    unit("min", &["min", "mins", "minute", "minutes"], Time, 60.0),
    unit("h", &["h", "hr", "hrs", "hour", "hours"], Time, 3600.0),
    unit("d", &["d", "day", "days"], Time, 86_400.0),
    unit("wk", &["wk", "week", "weeks"], Time, 604_800.0),
    unit("mo", &["mo", "month", "months"], Time, 2_629_746.0),
    unit(
        "yr",
        &["y", "yr", "yrs", "year", "years"],
        Time,
        31_556_952.0,
    ),
    // Speed (meters per second).
    unit("m/s", &["m/s", "mps"], Speed, 1.0),
    unit(
        "km/h",
        &["km/h", "kmh", "kph", "kmph"],
        Speed,
        1000.0 / 3600.0,
    ),
    unit("mph", &["mph", "mi/h"], Speed, 1609.344 / 3600.0),
    unit("kn", &["kn", "kt", "knot", "knots"], Speed, 1852.0 / 3600.0),
    unit("ft/s", &["ft/s", "fps"], Speed, 0.3048),
];

fn find(name: &str) -> Option<&'static Unit> {
    UNITS.iter().find(|u| u.names.contains(&name))
}

pub struct UnitsProvider;

impl Provider for UnitsProvider {
    fn id(&self) -> ProviderId {
        ProviderId::Units
    }

    fn query(&self, q: &Query, out: &mut Vec<Hit>) {
        let Some((value, to)) = convert(&q.lower) else {
            return;
        };
        let answer = format_sig(value, DIGITS);
        out.push(Hit {
            provider: ProviderId::Units,
            title: format!("{answer} {}", to.symbol),
            subtitle: format!("{} =", q.text),
            icon: HitIcon::Glyph(GLYPH),
            action: HitAction::Copy(answer),
            score: score::ANSWER,
            confirm: None,
        });
    }
}

/// "5 km to mi" → (3.10686, mile). `text` is lowercase.
fn convert(text: &str) -> Option<(f64, &'static Unit)> {
    let (value, rest) = leading_number(text)?;
    let rest = rest.trim();
    // "<from> <to|in|as> <to>". Units may have a space ("fl oz"), so try
    // every connector position.
    let words: Vec<&str> = rest.split(' ').filter(|w| !w.is_empty()).collect();
    for (i, w) in words.iter().enumerate().skip(1) {
        if !matches!(*w, "to" | "in" | "as" | "->") {
            continue;
        }
        let (Some(from), Some(to)) = (find(&words[..i].join(" ")), find(&words[i + 1..].join(" ")))
        else {
            continue;
        };
        if from.kind != to.kind {
            return None;
        }
        let base = value * from.factor + from.offset;
        let result = (base - to.offset) / to.factor;
        return result.is_finite().then_some((result, to));
    }
    None
}

/// A number at the start ("-12.5", "3e2") and the text after it.
fn leading_number(text: &str) -> Option<(f64, &str)> {
    let bytes = text.as_bytes();
    let mut end = 0;
    if matches!(bytes.first(), Some(b'-' | b'+')) {
        end = 1;
    }
    let digits_start = end;
    while end < bytes.len() && (bytes[end].is_ascii_digit() || bytes[end] == b'.') {
        end += 1;
    }
    if end == digits_start {
        return None;
    }
    let value: f64 = text[..end].parse().ok()?;
    Some((value, &text[end..]))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn conv(s: &str) -> Option<String> {
        let mut out = Vec::new();
        UnitsProvider.query(&Query::new(s, false), &mut out);
        out.pop().map(|h| h.title)
    }

    #[test]
    fn examples_from_the_spec() {
        assert_eq!(conv("5 km to mi").as_deref(), Some("3.10686 mi"));
        assert_eq!(conv("30c to f").as_deref(), Some("86 °F"));
        assert_eq!(conv("2gb in mb").as_deref(), Some("2000 MB"));
    }

    #[test]
    fn more_conversions() {
        assert_eq!(conv("100 F to C").as_deref(), Some("37.7778 °C"));
        assert_eq!(conv("0 k to c").as_deref(), Some("-273.15 °C"));
        assert_eq!(conv("-40 c in f").as_deref(), Some("-40 °F"));
        assert_eq!(conv("1 mi in km").as_deref(), Some("1.60934 km"));
        assert_eq!(conv("6 ft to cm").as_deref(), Some("182.88 cm"));
        assert_eq!(conv("12 in to cm").as_deref(), Some("30.48 cm"));
        assert_eq!(conv("1 gib to mb").as_deref(), Some("1073.74 MB"));
        assert_eq!(conv("8 bits to bytes").as_deref(), Some("1 B"));
        assert_eq!(conv("90 min to h").as_deref(), Some("1.5 h"));
        assert_eq!(conv("100 km/h to mph").as_deref(), Some("62.1371 mph"));
        assert_eq!(conv("1 lb to kg").as_deref(), Some("0.453592 kg"));
        assert_eq!(conv("2 cups to ml").as_deref(), Some("473.176 ml"));
        assert_eq!(conv("1 fl oz to ml").as_deref(), Some("29.5735 ml"));
        assert_eq!(conv("1.5 KB to bytes").as_deref(), Some("1500 B"));
    }

    #[test]
    fn copies_just_the_number() {
        let mut out = Vec::new();
        UnitsProvider.query(&Query::new("2gb in mb", false), &mut out);
        assert_eq!(out[0].action, HitAction::Copy("2000".into()));
        assert_eq!(out[0].subtitle, "2gb in mb =");
    }

    #[test]
    fn not_conversions() {
        for s in [
            "5 km to kg",
            "km to mi",
            "5 km",
            "5 km to",
            "5 parsecs to m",
            "notepad",
            "12*8",
            "5 km to mi please",
            "",
        ] {
            assert_eq!(conv(s), None, "{s}");
        }
    }
}
