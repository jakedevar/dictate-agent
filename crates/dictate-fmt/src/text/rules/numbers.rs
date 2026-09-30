//! `numbers`: spoken numbers to digits, conservatively.
//!
//! - multi-word cardinals: "twenty five" → `25`, "one hundred and twenty" →
//!   `120`, "twenty five thousand" → `25,000` (commas from 10,000 up, so
//!   "port eight thousand" stays pasteable as `8000`), "two million" →
//!   `2 million`;
//! - decimals and versions: "three point five" → `3.5`, "version two point
//!   one point three" → `version 2.1.3`;
//! - percent and currency: "five percent" → `5%`, "ten dollars" → `$10`,
//!   "twenty dollars and fifty cents" → `$20.50`, "five euros" → `€5`;
//! - clock times with a marker: "five thirty pm" → `5:30 PM`, "nine a.m." →
//!   `9 AM`;
//! - single numbers below ten stay words in prose ("one of the best", "two
//!   people") unless a unit, percent, currency, or a preceding "version"
//!   makes them quantities.
//!
//! A run of number words must parse as exactly one well-formed number, or it
//! is left alone: "twenty twenty four", "one two three", "nineteen eighty
//! four" and "a hundred" stay words. Digits Whisper already wrote are never
//! touched, and nothing is glued onto them.

use crate::text::lex::{Editor, Kind};
use crate::text::{FormatContext, TextDoc, TextStage};

/// Converts spoken numbers.
#[derive(Debug, Default, Clone, Copy)]
pub struct Numbers;

impl TextStage for Numbers {
    fn name(&self) -> &'static str {
        "numbers"
    }

    fn apply(&self, doc: &mut TextDoc, _ctx: &FormatContext) {
        doc.edit(convert);
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Num {
    Ones(u64),
    Teens(u64),
    Tens(u64),
    Hundred,
    Scale(u64),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Item {
    N(Num),
    And,
    Point,
}

const ONES: [&str; 10] = [
    "zero", "one", "two", "three", "four", "five", "six", "seven", "eight", "nine",
];
const TEENS: [&str; 10] = [
    "ten",
    "eleven",
    "twelve",
    "thirteen",
    "fourteen",
    "fifteen",
    "sixteen",
    "seventeen",
    "eighteen",
    "nineteen",
];
const TENS: [&str; 8] = [
    "twenty", "thirty", "forty", "fifty", "sixty", "seventy", "eighty", "ninety",
];
const SCALES: [(&str, u64); 4] = [
    ("thousand", 1_000),
    ("million", 1_000_000),
    ("billion", 1_000_000_000),
    ("trillion", 1_000_000_000_000),
];

fn classify(w: &str) -> Option<Num> {
    if w.len() > 9 || !w.is_ascii() {
        return None;
    }
    let eq = |s: &&str| s.eq_ignore_ascii_case(w);
    if let Some(v) = ONES.iter().position(eq) {
        return Some(Num::Ones(v as u64));
    }
    if let Some(v) = TEENS.iter().position(eq) {
        return Some(Num::Teens(10 + v as u64));
    }
    if let Some(v) = TENS.iter().position(eq) {
        return Some(Num::Tens(20 + 10 * v as u64));
    }
    if w.eq_ignore_ascii_case("hundred") {
        return Some(Num::Hundred);
    }
    SCALES
        .iter()
        .find(|(s, _)| s.eq_ignore_ascii_case(w))
        .map(|&(_, v)| Num::Scale(v))
}

/// A word token as number parts: `five` → [5], `twenty-five` → [20, 5].
fn parts(w: &str) -> Option<([Num; 2], usize)> {
    if let Some((a, b)) = w.split_once('-') {
        return match (classify(a), classify(b)) {
            (Some(t @ Num::Tens(_)), Some(o @ Num::Ones(v))) if v > 0 => Some(([t, o], 2)),
            _ => None,
        };
    }
    classify(w).map(|n| ([n, n], 1))
}

/// Whether `w` is a spoken number word (or a `twenty-five` compound).
pub(crate) fn is_number_word(w: &str) -> bool {
    parts(w).is_some()
}

struct IntParse {
    value: u64,
    /// `twenty five million` → (25, "million"): rendered `25 million`.
    big: Option<(u64, &'static str)>,
}

fn parse_int(items: &[Item]) -> Option<IntParse> {
    if items.is_empty() {
        return None;
    }
    let mut total: u64 = 0;
    let mut current: u64 = 0;
    let mut last_scale = u64::MAX;
    let mut prev: Option<Item> = None;
    let mut scales = 0;
    for it in items {
        match *it {
            Item::N(Num::Ones(v)) => {
                if !matches!(
                    prev,
                    None | Some(Item::N(Num::Tens(_) | Num::Hundred | Num::Scale(_)) | Item::And)
                ) {
                    return None;
                }
                if v == 0 && items.len() > 1 {
                    return None;
                }
                current += v;
            }
            Item::N(Num::Teens(v) | Num::Tens(v)) => {
                if !matches!(
                    prev,
                    None | Some(Item::N(Num::Hundred | Num::Scale(_)) | Item::And)
                ) {
                    return None;
                }
                current += v;
            }
            Item::N(Num::Hundred) => {
                if !matches!(prev, Some(Item::N(Num::Ones(_) | Num::Teens(_))))
                    || current == 0
                    || current >= 100
                {
                    return None;
                }
                current *= 100;
            }
            Item::N(Num::Scale(s)) => {
                if !matches!(prev, Some(Item::N(_))) || current == 0 || s >= last_scale {
                    return None;
                }
                total = total.checked_add(current.checked_mul(s)?)?;
                last_scale = s;
                current = 0;
                scales += 1;
            }
            Item::And => {
                if !matches!(prev, Some(Item::N(Num::Hundred | Num::Scale(_)))) {
                    return None;
                }
            }
            Item::Point => return None,
        }
        prev = Some(*it);
    }
    if prev == Some(Item::And) {
        return None;
    }
    let value = total.checked_add(current)?;
    let big = match items.last() {
        Some(Item::N(Num::Scale(s))) if *s >= 1_000_000 && scales == 1 => {
            let name = SCALES.iter().find(|(_, v)| v == s).map(|(n, _)| *n)?;
            Some((value / s, name))
        }
        _ => None,
    };
    Some(IntParse { value, big })
}

/// Digits after a "point": each a single digit ("one four" → `14`), or one
/// compound ("fourteen" → `14`). A trailing million/billion is allowed on the
/// last group only.
fn parse_fraction(items: &[Item], allow_big: bool) -> Option<(String, Option<&'static str>)> {
    let (items, big) = match items.split_last() {
        Some((Item::N(Num::Scale(s)), rest))
            if allow_big && *s >= 1_000_000 && !rest.is_empty() =>
        {
            let name = SCALES.iter().find(|(_, v)| v == s).map(|(n, _)| *n)?;
            (rest, Some(name))
        }
        _ => (items, None),
    };
    if items.is_empty() {
        return None;
    }
    if items.iter().all(|i| matches!(i, Item::N(Num::Ones(_)))) {
        let digits = items
            .iter()
            .map(|i| match i {
                Item::N(Num::Ones(v)) => char::from(b'0' + *v as u8),
                _ => unreachable!(),
            })
            .collect();
        return Some((digits, big));
    }
    if items
        .iter()
        .any(|i| matches!(i, Item::N(Num::Hundred | Num::Scale(_)) | Item::And))
    {
        return None;
    }
    parse_int(items).map(|p| (p.value.to_string(), big))
}

/// Plain digits below `from`, comma-grouped from `from` up.
fn group_digits(v: u64, from: u64) -> String {
    let digits = v.to_string();
    if v < from {
        return digits;
    }
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

/// Counts group from 10,000 so a spoken port or size (`8000`) stays pasteable.
fn group_thousands(v: u64) -> String {
    group_digits(v, 10_000)
}

/// A parsed run, rendered.
struct Rendered {
    text: String,
    /// A lone number below ten, which stays a word unless something marks it
    /// as a quantity.
    small_single: bool,
    /// The value, when it rendered as a plain integer (no scale word, no
    /// decimals). Money is regrouped from 1,000 (`$5,000`) and may take cents.
    plain_int: Option<u64>,
}

fn render(items: &[Item]) -> Option<Rendered> {
    let groups: Vec<&[Item]> = items.split(|i| *i == Item::Point).collect();
    match groups.len() {
        1 => {
            let p = parse_int(items)?;
            let text = match p.big {
                Some((mult, name)) => format!("{mult} {name}"),
                None => group_thousands(p.value),
            };
            Some(Rendered {
                small_single: items.len() == 1 && p.value < 10,
                plain_int: p.big.is_none().then_some(p.value),
                text,
            })
        }
        2 => {
            let int = parse_int(groups[0])?;
            if int.big.is_some()
                || groups[0]
                    .iter()
                    .any(|i| matches!(i, Item::N(Num::Scale(_))))
            {
                return None;
            }
            let (frac, big) = parse_fraction(groups[1], true)?;
            let mut text = format!("{}.{frac}", group_thousands(int.value));
            if let Some(name) = big {
                text.push(' ');
                text.push_str(name);
            }
            Some(Rendered {
                text,
                small_single: false,
                plain_int: None,
            })
        }
        _ => {
            let int = parse_int(groups[0])?;
            if int.big.is_some() || int.value >= 10_000 {
                return None;
            }
            let mut text = int.value.to_string();
            for g in &groups[1..] {
                let (component, _) = parse_fraction(g, false)?;
                text.push('.');
                text.push_str(&component);
            }
            Some(Rendered {
                text,
                small_single: false,
                plain_int: None,
            })
        }
    }
}

/// Words after a number that make it a quantity.
const UNITS: &[&str] = &[
    "ms",
    "millisecond",
    "milliseconds",
    "second",
    "seconds",
    "sec",
    "secs",
    "minute",
    "minutes",
    "min",
    "mins",
    "hour",
    "hours",
    "hr",
    "hrs",
    "byte",
    "bytes",
    "kb",
    "mb",
    "gb",
    "tb",
    "kilobyte",
    "kilobytes",
    "megabyte",
    "megabytes",
    "gigabyte",
    "gigabytes",
    "terabyte",
    "terabytes",
    "gig",
    "gigs",
    "px",
    "pixel",
    "pixels",
    "pt",
    "degree",
    "degrees",
    "mm",
    "cm",
    "km",
    "meter",
    "meters",
    "metre",
    "metres",
    "kilometer",
    "kilometers",
    "inch",
    "inches",
    "foot",
    "feet",
    "mile",
    "miles",
    "kg",
    "kilogram",
    "kilograms",
    "gram",
    "grams",
    "pound",
    "pounds",
    "lb",
    "lbs",
    "ounce",
    "ounces",
    "liter",
    "liters",
    "litre",
    "litres",
    "ml",
    "hz",
    "khz",
    "mhz",
    "ghz",
    "fps",
    "rpm",
    "mph",
    "kph",
    "volt",
    "volts",
    "watt",
    "watts",
    "amp",
    "amps",
    "cent",
    "cents",
    "core",
    "cores",
];

/// Ordinal words: a number run right before one is part of an ordinal.
/// `second` is here because "twenty second" is 22nd as often as 20 s.
const ORDINALS: &[&str] = &[
    "first",
    "second",
    "third",
    "fourth",
    "fifth",
    "sixth",
    "seventh",
    "eighth",
    "ninth",
    "tenth",
    "eleventh",
    "twelfth",
    "thirteenth",
    "fourteenth",
    "fifteenth",
    "sixteenth",
    "seventeenth",
    "eighteenth",
    "nineteenth",
    "twentieth",
    "thirtieth",
    "fortieth",
    "fiftieth",
    "sixtieth",
    "seventieth",
    "eightieth",
    "ninetieth",
    "hundredth",
    "thousandth",
    "millionth",
];

/// "one second" / "one minute" are idioms for "a moment", not measurements.
const ONE_IDIOMS: &[&str] = &["second", "sec", "minute", "min"];

fn eq_any(w: &str, list: &[&str]) -> bool {
    list.iter().any(|x| x.eq_ignore_ascii_case(w))
}

fn is_digits(w: &str) -> bool {
    w.chars().next().is_some_and(|c| c.is_ascii_digit())
}

struct Run {
    /// Word tokens and the single spaces between them, in order.
    tokens: Vec<usize>,
    items: Vec<Item>,
    last: usize,
}

fn collect_run(ed: &Editor<'_>, start: usize) -> Option<Run> {
    let (p, n) = parts(ed.text(start))?;
    let mut items: Vec<Item> = p[..n].iter().map(|x| Item::N(*x)).collect();
    let mut tokens = vec![start];
    let mut last = start;
    loop {
        let Some((sp, j)) = ed.next_word_after_space(last) else {
            break;
        };
        let w = ed.text(j);
        if let Some((p, n)) = parts(w) {
            items.extend(p[..n].iter().map(|x| Item::N(*x)));
            tokens.extend([sp, j]);
            last = j;
            continue;
        }
        let joiner = if w.eq_ignore_ascii_case("and")
            && matches!(items.last(), Some(Item::N(Num::Hundred | Num::Scale(_))))
        {
            Some(Item::And)
        } else if w.eq_ignore_ascii_case("point") && matches!(items.last(), Some(Item::N(_))) {
            Some(Item::Point)
        } else {
            None
        };
        let Some(joiner) = joiner else { break };
        // Only if a number word follows the joiner.
        let Some((sp2, k)) = ed.next_word_after_space(j) else {
            break;
        };
        let Some((p, n)) = parts(ed.text(k)) else {
            break;
        };
        if joiner == Item::And && !matches!(p[0], Num::Ones(_) | Num::Teens(_) | Num::Tens(_)) {
            break;
        }
        items.push(joiner);
        items.extend(p[..n].iter().map(|x| Item::N(*x)));
        tokens.extend([sp, j, sp2, k]);
        last = k;
    }
    Some(Run {
        tokens,
        items,
        last,
    })
}

/// Punctuation glued to a number word that makes it part of something else
/// (`twenty/twenty`, `v.two`).
fn glued(ed: &Editor<'_>, first: usize, last: usize) -> bool {
    let before = ed.touching_prev(first).is_some_and(|p| match ed.kind(p) {
        Kind::Newline => false,
        Kind::Punct => matches!(ed.text(p), "/" | "-" | "." | ":" | "$" | "#" | "_"),
        _ => true,
    });
    let after = ed.touching_next(last).is_some_and(|n| match ed.kind(n) {
        Kind::Newline => false,
        Kind::Punct => matches!(ed.text(n), "/" | "-" | "_" | "%"),
        _ => true,
    });
    before || after
}

fn digits_adjacent(ed: &Editor<'_>, first: usize, last: usize) -> bool {
    let before = ed
        .prev_alive(first)
        .filter(|&s| ed.kind(s) == Kind::Space)
        .and_then(|s| ed.prev_alive(s))
        .is_some_and(|w| ed.kind(w) == Kind::Word && is_digits(ed.text(w)));
    let after = ed
        .next_word_after_space(last)
        .is_some_and(|(_, w)| is_digits(ed.text(w)));
    before || after
}

fn convert(ed: &mut Editor<'_>) {
    let n = ed.len();
    let mut i = 0;
    while i < n {
        if !ed.is_word(i) || parts(ed.text(i)).is_none() {
            i += 1;
            continue;
        }
        if let Some(next) = try_time(ed, i) {
            i = next;
            continue;
        }
        let Some(run) = collect_run(ed, i) else {
            i += 1;
            continue;
        };
        let resume = run.last + 1;
        if !glued(ed, i, run.last) && !digits_adjacent(ed, i, run.last) {
            apply_run(ed, &run);
        }
        i = resume;
    }
}

fn apply_run(ed: &mut Editor<'_>, run: &Run) {
    let Some(rendered) = render(&run.items) else {
        return;
    };
    let first = run.tokens[0];
    let suffix = ed.next_word_after_space(run.last);
    let suffix_word = suffix.map(|(_, w)| ed.text(w).to_string());
    let suffix_word = suffix_word.as_deref().unwrap_or("");

    // "twenty third", "one hundred and first", "twenty second": ordinals (or
    // ambiguous with one) stay words.
    let ordinal_after_and = suffix_word.eq_ignore_ascii_case("and")
        && suffix.is_some_and(|(_, w)| {
            ed.next_word_after_space(w)
                .is_some_and(|(_, o)| eq_any(ed.text(o), ORDINALS))
        });
    if eq_any(suffix_word, ORDINALS) || ordinal_after_and {
        return;
    }

    // Percent: "five percent" / "five per cent".
    let percent = if suffix_word.eq_ignore_ascii_case("percent") {
        suffix.map(|(sp, w)| vec![sp, w])
    } else if suffix_word.eq_ignore_ascii_case("per") {
        suffix.and_then(|(sp, w)| {
            ed.next_word_after_space(w)
                .filter(|&(_, c)| ed.text(c).eq_ignore_ascii_case("cent"))
                .map(|(sp2, c)| vec![sp, w, sp2, c])
        })
    } else {
        None
    };
    if let Some(consumed) = percent {
        finish(ed, run, &consumed, format!("{}%", rendered.text));
        return;
    }

    // Currency: "$10", "$2 million", "$20.50", "€5".
    let symbol = if eq_any(suffix_word, &["dollars", "dollar"]) {
        Some('$')
    } else if eq_any(suffix_word, &["euros", "euro"]) {
        Some('\u{20AC}')
    } else {
        None
    };
    if let (Some(symbol), Some((sp, w))) = (symbol, suffix) {
        let mut consumed = vec![sp, w];
        let amount = match rendered.plain_int {
            Some(v) => group_digits(v, 1_000),
            None => rendered.text.clone(),
        };
        let mut text = format!("{symbol}{amount}");
        if symbol == '$' && rendered.plain_int.is_some() {
            if let Some((cents, extra)) = cents_after(ed, w) {
                text = format!("{text}.{cents:02}");
                consumed.extend(extra);
            }
        }
        finish(ed, run, &consumed, text);
        return;
    }

    let quantity =
        eq_any(suffix_word, UNITS) && !(rendered.text == "1" && eq_any(suffix_word, ONE_IDIOMS));
    let versioned = ed
        .prev_alive(first)
        .filter(|&s| ed.kind(s) == Kind::Space)
        .and_then(|s| ed.prev_alive(s))
        .is_some_and(|w| ed.kind(w) == Kind::Word && ed.text(w).eq_ignore_ascii_case("version"));
    if rendered.small_single && !quantity && !versioned {
        return;
    }
    finish(ed, run, &[], rendered.text);
}

/// "and fifty cents" after "dollars": the cents value and the tokens used.
fn cents_after(ed: &Editor<'_>, dollars: usize) -> Option<(u64, Vec<usize>)> {
    let (sp1, and) = ed.next_word_after_space(dollars)?;
    if !ed.text(and).eq_ignore_ascii_case("and") {
        return None;
    }
    let (sp2, start) = ed.next_word_after_space(and)?;
    let run = collect_run(ed, start)?;
    let (sp3, unit) = ed.next_word_after_space(run.last)?;
    if !eq_any(ed.text(unit), &["cents", "cent"]) {
        return None;
    }
    let value = parse_int(&run.items)?.value;
    if value >= 100 {
        return None;
    }
    let mut used = vec![sp1, and, sp2];
    used.extend(&run.tokens);
    used.extend([sp3, unit]);
    Some((value, used))
}

fn finish(ed: &mut Editor<'_>, run: &Run, extra: &[usize], text: String) {
    for &t in run.tokens[1..].iter().chain(extra) {
        ed.delete(t);
    }
    ed.replace(run.tokens[0], text);
}

/// Clock times: an hour word, optional minutes, and an am/pm marker.
fn try_time(ed: &mut Editor<'_>, i: usize) -> Option<usize> {
    let (p, n) = parts(ed.text(i))?;
    let hour = match (n, p[0]) {
        (1, Num::Ones(v)) if v >= 1 => v,
        (1, Num::Teens(v)) if v <= 12 => v,
        _ => return None,
    };
    let mut used: Vec<usize> = Vec::new();
    let mut cursor = i;
    let mut minutes: Option<u64> = None;
    let (sp, j) = ed.next_word_after_space(cursor)?;
    if marker(ed, j).is_none() {
        let w = ed.text(j);
        if w.eq_ignore_ascii_case("oh") || w.eq_ignore_ascii_case("o") {
            let (sp2, k) = ed.next_word_after_space(j)?;
            match parts(ed.text(k)) {
                Some((p, 1)) if matches!(p[0], Num::Ones(v) if v >= 1) => {
                    if let Num::Ones(v) = p[0] {
                        minutes = Some(v);
                    }
                    used.extend([sp, j, sp2, k]);
                    cursor = k;
                }
                _ => return None,
            }
        } else {
            let (p, n) = parts(w)?;
            let mut m = match (n, p[0], p[1]) {
                (2, Num::Tens(t), Num::Ones(o)) => t + o,
                (1, Num::Teens(t), _) => t,
                (1, Num::Tens(t), _) => t,
                _ => return None,
            };
            used.extend([sp, j]);
            cursor = j;
            if n == 1 && matches!(p[0], Num::Tens(_)) {
                if let Some((sp2, k)) = ed.next_word_after_space(j) {
                    if let Some((q, 1)) = parts(ed.text(k)) {
                        if let Num::Ones(o) = q[0] {
                            if o >= 1 {
                                m += o;
                                used.extend([sp2, k]);
                                cursor = k;
                            }
                        }
                    }
                }
            }
            if m >= 60 {
                return None;
            }
            minutes = Some(m);
        }
    }
    let (msp, m) = ed.next_word_after_space(cursor)?;
    let (label, marker_tokens, sentence_end) = marker(ed, m)?;
    used.push(msp);
    used.extend(marker_tokens.iter().copied());
    let mut text = match minutes {
        Some(mm) => format!("{hour}:{mm:02} {label}"),
        None => format!("{hour} {label}"),
    };
    if sentence_end {
        text.push('.');
    }
    let last = *used.last().unwrap_or(&i);
    for t in used {
        ed.delete(t);
    }
    ed.replace(i, text);
    Some(last + 1)
}

/// `am`/`pm`, or `a.m.`/`p.m.`. Returns the label, the tokens it spans, and
/// whether a consumed final period was also ending the sentence.
fn marker(ed: &Editor<'_>, w: usize) -> Option<(&'static str, Vec<usize>, bool)> {
    let t = ed.text(w);
    if t.eq_ignore_ascii_case("am") || t.eq_ignore_ascii_case("pm") {
        let label = if t.eq_ignore_ascii_case("am") {
            "AM"
        } else {
            "PM"
        };
        return Some((label, vec![w], false));
    }
    let label = if t.eq_ignore_ascii_case("a") {
        "AM"
    } else if t.eq_ignore_ascii_case("p") {
        "PM"
    } else {
        return None;
    };
    let d1 = ed.touching_next(w).filter(|&d| ed.text(d) == ".")?;
    let m = ed
        .touching_next(d1)
        .filter(|&m| ed.kind(m) == Kind::Word && ed.text(m).eq_ignore_ascii_case("m"))?;
    let mut tokens = vec![w, d1, m];
    let mut sentence_end = false;
    if let Some(d2) = ed.touching_next(m).filter(|&d| ed.text(d) == ".") {
        tokens.push(d2);
        sentence_end = match ed.next_alive(d2) {
            None => true,
            Some(s) if ed.kind(s) == Kind::Newline => true,
            Some(s) if ed.kind(s) == Kind::Space => ed.next_alive(s).is_some_and(|x| {
                ed.kind(x) == Kind::Word && ed.text(x).starts_with(char::is_uppercase)
            }),
            Some(_) => false,
        };
    }
    Some((label, tokens, sentence_end))
}

#[cfg(test)]
mod tests {
    use crate::text::rules::test_support::stage;

    use super::*;

    fn num(input: &str) -> String {
        stage(&Numbers, input)
    }

    #[test]
    fn cardinals() {
        let cases: &[(&str, &str)] = &[
            ("twenty five", "25"),
            ("twenty-five tests", "25 tests"),
            ("one hundred and twenty", "120"),
            ("one hundred twenty three", "123"),
            ("four hundred four", "404"),
            ("ten minutes", "10 minutes"),
            ("I have eleven apples", "I have 11 apples"),
            ("port eight thousand", "port 8000"),
            ("twenty five thousand users", "25,000 users"),
            ("one thousand two hundred thirty four", "1234"),
            ("two thousand and five", "2005"),
            ("nineteen hundred", "1900"),
            ("twenty five hundred", "2500"),
            ("one million two hundred thousand", "1,200,000"),
            ("two million", "2 million"),
            ("three hundred million", "300 million"),
            ("Twenty people came", "20 people came"),
            ("it's twenty five or thirty", "it's 25 or 30"),
        ];
        for (input, want) in cases {
            assert_eq!(num(input), *want, "input: {input}");
        }
    }

    #[test]
    fn decimals_versions_percent_currency_times() {
        let cases: &[(&str, &str)] = &[
            ("three point five", "3.5"),
            ("version two point one", "version 2.1"),
            ("version two point one point three", "version 2.1.3"),
            ("Python three point twelve", "Python 3.12"),
            ("pi is three point one four", "pi is 3.14"),
            ("zero point five", "0.5"),
            ("two point five million", "2.5 million"),
            ("five percent", "5%"),
            ("twenty five percent off", "25% off"),
            ("three point five percent", "3.5%"),
            ("five per cent", "5%"),
            ("ten dollars", "$10"),
            ("one dollar", "$1"),
            ("a five dollar bill", "a $5 bill"),
            ("twenty dollars and fifty cents", "$20.50"),
            ("about five thousand dollars", "about $5,000"),
            ("one hundred and forty nine dollars", "$149"),
            ("two million dollars", "$2 million"),
            ("five euros", "\u{20AC}5"),
            ("five thirty pm", "5:30 PM"),
            ("at nine am", "at 9 AM"),
            ("at ten fifteen AM sharp", "at 10:15 AM sharp"),
            ("five oh five pm", "5:05 PM"),
            ("eleven forty five pm", "11:45 PM"),
            ("meet at five p.m. Then leave.", "meet at 5 PM. Then leave."),
            ("meet at five p.m. tomorrow", "meet at 5 PM tomorrow"),
            ("version two", "version 2"),
            ("five minutes", "5 minutes"),
            ("two gigabytes", "2 gigabytes"),
            ("wait three seconds", "wait 3 seconds"),
            ("one GB", "1 GB"),
        ];
        for (input, want) in cases {
            assert_eq!(num(input), *want, "input: {input}");
        }
    }

    #[test]
    fn leaves_prose_and_ambiguity_alone() {
        for input in [
            "one of the best",
            "two people",
            "the top five",
            "zero",
            "nine to five",
            "one on one",
            "twenty twenty four",
            "one two three",
            "nineteen eighty four",
            "a hundred and fifty",
            "a thousand things",
            "hundreds of files",
            "the three point guard",
            "a five point plan",
            "point five",
            "two and a half hours",
            "twenty 5",
            "5 hundred",
            "fifty-fifty",
            "a one-off",
            "five-minute break",
            "give me one second",
            "one minute please",
            "someone else",
            "5,000 and 3.14 stay",
            "twenty/twenty vision",
            "five thirty tomorrow",
            "I am five",
            "the twenty third of May",
            "the one hundred and first time",
            "a twenty second delay",
            "the twenty first century",
        ] {
            assert_eq!(num(input), input, "input: {input}");
        }
    }

    #[test]
    fn grouping() {
        assert_eq!(group_thousands(9_999), "9999");
        assert_eq!(group_thousands(10_000), "10,000");
        assert_eq!(group_thousands(1_234_567), "1,234,567");
    }
}
