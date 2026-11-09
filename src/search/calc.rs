//! The calculator provider (SUPERSEARCH.md 3, "calc").
//!
//! A small recursive-descent parser: one function per precedence level,
//! each calling the next tighter one:
//!
//! ```text
//! input   := sum [("in" | "to" | "as") ("hex" | "bin" | "dec")]
//! sum     := product (("+" | "-") product)*
//! product := unary (("*" | "/" | "%" | "of" | <nothing>) unary)*
//! unary   := ("-" | "+") unary | power
//! power   := postfix ["^" unary]          (right to left: 2^3^2 = 2^9)
//! postfix := primary ["%"]                (15% = 0.15)
//! primary := number | name | name "(" sum ("," sum)* ")" | "(" sum ")"
//! ```
//!
//! It only knows numbers, the operators above, and a fixed list of
//! function and constant names. Anything else is "not math", so nothing
//! the user types can ever run code.

use super::{Hit, HitAction, HitIcon, Provider, ProviderId, Query, score};

/// The calculator glyph (Segoe Fluent Icons).
const GLYPH: char = '\u{E8EF}';
/// Deeper nesting than this is refused (no stack overflow on "((((...").
const MAX_DEPTH: usize = 64;
/// Longer input is not math anyone types (and keeps a keystroke fast).
const MAX_LEN: usize = 256;

pub struct CalcProvider;

impl Provider for CalcProvider {
    fn id(&self) -> ProviderId {
        ProviderId::Calc
    }

    fn prefix(&self) -> Option<&str> {
        Some("=")
    }

    fn query(&self, q: &Query, out: &mut Vec<Hit>) {
        let Some(answer) = evaluate(q.text, q.scoped) else {
            return;
        };
        out.push(Hit {
            provider: ProviderId::Calc,
            title: answer.clone(),
            subtitle: format!("{} =", q.text),
            icon: HitIcon::Glyph(GLYPH),
            action: HitAction::Copy(answer),
            score: score::ANSWER,
            confirm: None,
        });
    }
}

/// The answer to `text` as it should be shown and copied, or None if it
/// isn't math. Unless `always`, plain values ("12", "pi", "-3") give None:
/// only something that was actually calculated is worth a row.
pub fn evaluate(text: &str, always: bool) -> Option<String> {
    if text.len() > MAX_LEN {
        return None;
    }
    let tokens = tokenize(text)?;
    let mut p = Parser {
        tokens,
        pos: 0,
        depth: 0,
        worked: false,
    };
    let value = p.sum()?.value;
    let base = p.conversion()?;
    if p.pos != p.tokens.len() {
        return None;
    }
    if !(always || p.worked || base.is_some()) {
        return None;
    }
    if !value.is_finite() {
        return None;
    }
    match base {
        None | Some(Base::Dec) => Some(format_number(value)),
        Some(base) => format_base(value, base),
    }
}

#[derive(Debug, Clone, PartialEq)]
enum Token {
    Num(f64),
    /// A number written in hex or binary (counts as "worked": converting).
    Based(f64),
    Name(String),
    Op(char),
}

fn tokenize(text: &str) -> Option<Vec<Token>> {
    let chars: Vec<char> = text.chars().collect();
    let mut tokens = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c.is_whitespace() {
            i += 1;
        } else if c == '0' && matches!(chars.get(i + 1), Some('x' | 'X' | 'b' | 'B')) {
            let radix = if matches!(chars[i + 1], 'x' | 'X') {
                16
            } else {
                2
            };
            let start = i + 2;
            let mut end = start;
            while end < chars.len() && chars[end].is_digit(radix) {
                end += 1;
            }
            if end == start {
                return None;
            }
            let digits: String = chars[start..end].iter().collect();
            let v = u64::from_str_radix(&digits, radix).ok()?;
            tokens.push(Token::Based(v as f64));
            i = end;
        } else if c.is_ascii_digit() || c == '.' {
            let start = i;
            while i < chars.len() && (chars[i].is_ascii_digit() || chars[i] == '.') {
                i += 1;
            }
            // An exponent only if digits follow ("2e3"; "2e" is 2 × e).
            if i < chars.len() && matches!(chars[i], 'e' | 'E') {
                let mut j = i + 1;
                if matches!(chars.get(j), Some('+' | '-')) {
                    j += 1;
                }
                if chars.get(j).is_some_and(char::is_ascii_digit) {
                    i = j;
                    while i < chars.len() && chars[i].is_ascii_digit() {
                        i += 1;
                    }
                }
            }
            let s: String = chars[start..i].iter().collect();
            tokens.push(Token::Num(s.parse().ok()?));
        } else if c.is_alphabetic() {
            let start = i;
            while i < chars.len() && chars[i].is_alphabetic() {
                i += 1;
            }
            let s: String = chars[start..i].iter().collect::<String>().to_lowercase();
            tokens.push(Token::Name(s));
        } else {
            let op = match c {
                '×' => '*',
                '÷' => '/',
                '−' => '-',
                '+' | '-' | '*' | '/' | '^' | '%' | '(' | ')' | ',' => c,
                _ => return None,
            };
            // "**" is a power too.
            if op == '*' && chars.get(i + 1) == Some(&'*') {
                tokens.push(Token::Op('^'));
                i += 2;
                continue;
            }
            tokens.push(Token::Op(op));
            i += 1;
        }
    }
    Some(tokens)
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum Base {
    Hex,
    Bin,
    Dec,
}

/// A value, and whether it was written as a percentage ("15%"), which
/// changes what + and - do: 200 + 15% = 230.
#[derive(Debug, Clone, Copy)]
struct Value {
    value: f64,
    percent: bool,
}

impl Value {
    fn plain(value: f64) -> Self {
        Self {
            value,
            percent: false,
        }
    }
}

struct Parser {
    tokens: Vec<Token>,
    pos: usize,
    depth: usize,
    /// An operator or function was used (so this is a calculation, not
    /// just a number).
    worked: bool,
}

impl Parser {
    fn peek(&self) -> Option<&Token> {
        self.tokens.get(self.pos)
    }

    fn eat_op(&mut self, op: char) -> bool {
        if self.peek() == Some(&Token::Op(op)) {
            self.pos += 1;
            true
        } else {
            false
        }
    }

    fn peek_name(&self, name: &str) -> bool {
        matches!(self.peek(), Some(Token::Name(n)) if n == name)
    }

    /// `[("in" | "to" | "as") base]` at the end.
    fn conversion(&mut self) -> Option<Option<Base>> {
        if !["in", "to", "as"].iter().any(|w| self.peek_name(w)) {
            return Some(None);
        }
        self.pos += 1;
        let base = match self.peek() {
            Some(Token::Name(n)) => match n.as_str() {
                "hex" | "hexadecimal" => Base::Hex,
                "bin" | "binary" => Base::Bin,
                "dec" | "decimal" => Base::Dec,
                _ => return None,
            },
            _ => return None,
        };
        self.pos += 1;
        Some(Some(base))
    }

    fn sum(&mut self) -> Option<Value> {
        self.depth += 1;
        if self.depth > MAX_DEPTH {
            return None;
        }
        let mut left = self.product()?;
        loop {
            let sign = if self.eat_op('+') {
                1.0
            } else if self.eat_op('-') {
                -1.0
            } else {
                break;
            };
            let right = self.product()?;
            self.worked = true;
            // 200 + 15% = 200 × 1.15 (like a pocket calculator).
            let value = if right.percent && !left.percent {
                left.value * (1.0 + sign * right.value)
            } else {
                left.value + sign * right.value
            };
            left = Value::plain(value);
        }
        self.depth -= 1;
        Some(left)
    }

    fn product(&mut self) -> Option<Value> {
        let mut left = self.unary()?;
        loop {
            let op = match self.peek() {
                Some(Token::Op(c @ ('*' | '/' | '%'))) => *c,
                Some(Token::Name(n)) if n == "of" || n == "x" => '*',
                Some(Token::Name(n)) if n == "mod" => '%',
                // Implicit multiplication: "2pi", "3(1+1)", "(1)(2)"...
                // but not "2 3", and not before "in hex".
                Some(Token::Op('(')) => 'i',
                Some(Token::Name(n)) if !matches!(n.as_str(), "in" | "to" | "as") => 'i',
                _ => break,
            };
            if op != 'i' {
                self.pos += 1;
            }
            let right = self.unary()?;
            self.worked = true;
            let value = match op {
                '/' => left.value / right.value,
                '%' => left.value % right.value,
                _ => left.value * right.value,
            };
            left = Value::plain(value);
        }
        Some(left)
    }

    fn unary(&mut self) -> Option<Value> {
        if self.eat_op('-') {
            let v = self.unary()?;
            return Some(Value::plain(-v.value));
        }
        if self.eat_op('+') {
            return self.unary();
        }
        self.power()
    }

    fn power(&mut self) -> Option<Value> {
        let base = self.postfix()?;
        if self.eat_op('^') {
            let exponent = self.unary()?;
            self.worked = true;
            return Some(Value::plain(base.value.powf(exponent.value)));
        }
        Some(base)
    }

    fn postfix(&mut self) -> Option<Value> {
        let v = self.primary()?;
        // "15%" is a percentage when nothing that could be its right-hand
        // side follows; "10 % 3" is the remainder (handled in `product`).
        if self.peek() == Some(&Token::Op('%')) {
            let next = self.tokens.get(self.pos + 1);
            let operand_follows = match next {
                Some(Token::Num(_) | Token::Based(_) | Token::Op('(')) => true,
                Some(Token::Name(n)) => {
                    !matches!(n.as_str(), "of" | "x" | "mod" | "in" | "to" | "as")
                }
                _ => false,
            };
            if !operand_follows {
                self.pos += 1;
                self.worked = true;
                return Some(Value {
                    value: v.value / 100.0,
                    percent: true,
                });
            }
        }
        Some(v)
    }

    fn primary(&mut self) -> Option<Value> {
        let token = self.peek()?.clone();
        self.pos += 1;
        match token {
            Token::Num(v) => Some(Value::plain(v)),
            Token::Based(v) => {
                self.worked = true;
                Some(Value::plain(v))
            }
            Token::Op('(') => {
                let v = self.sum()?;
                self.eat_op(')').then_some(v)
            }
            Token::Name(name) => self.name(&name),
            Token::Op(_) => None,
        }
    }

    /// A constant, or a function with its arguments.
    fn name(&mut self, name: &str) -> Option<Value> {
        match name {
            "pi" | "π" => return Some(Value::plain(std::f64::consts::PI)),
            "e" => return Some(Value::plain(std::f64::consts::E)),
            "tau" => return Some(Value::plain(std::f64::consts::TAU)),
            _ => {}
        }
        let f: fn(&[f64]) -> Option<f64> = match name {
            "sqrt" => |a| one(a).map(f64::sqrt),
            "sin" => |a| one(a).map(f64::sin),
            "cos" => |a| one(a).map(f64::cos),
            "tan" => |a| one(a).map(f64::tan),
            "ln" => |a| one(a).map(f64::ln),
            "log" => |a| match a {
                [x] => Some(x.log10()),
                [x, base] => Some(x.log(*base)),
                _ => None,
            },
            "abs" => |a| one(a).map(f64::abs),
            "round" => |a| one(a).map(f64::round),
            "floor" => |a| one(a).map(f64::floor),
            "ceil" => |a| one(a).map(f64::ceil),
            "min" => |a| a.iter().copied().reduce(f64::min),
            "max" => |a| a.iter().copied().reduce(f64::max),
            _ => return None,
        };
        let mut args = Vec::new();
        if self.eat_op('(') {
            args.push(self.sum()?.value);
            while self.eat_op(',') {
                args.push(self.sum()?.value);
            }
            if !self.eat_op(')') {
                return None;
            }
        } else {
            // "sqrt 16" works too.
            args.push(self.unary()?.value);
        }
        self.worked = true;
        f(&args).map(Value::plain)
    }
}

fn one(args: &[f64]) -> Option<f64> {
    match args {
        [x] => Some(*x),
        _ => None,
    }
}

/// Up to 10 significant digits, no trailing zeros, no "-0".
/// Very big or very small numbers use an exponent ("1.5e20").
pub fn format_number(v: f64) -> String {
    format_sig(v, 10)
}

/// `v` with at most `digits` significant digits.
pub fn format_sig(v: f64, digits: i32) -> String {
    if v == 0.0 || !v.is_finite() {
        return "0".to_string();
    }
    let abs = v.abs();
    if !(1e-9..1e15).contains(&abs) {
        let s = format!("{:.*e}", (digits - 1) as usize, v);
        let (mantissa, exponent) = s.split_once('e').unwrap_or((&s, "0"));
        return format!("{}e{exponent}", trim_zeros(mantissa));
    }
    let magnitude = abs.log10().floor() as i32;
    let decimals = (digits - 1 - magnitude).clamp(0, 15) as usize;
    let s = format!("{v:.decimals$}");
    let s = trim_zeros(&s);
    if s == "-0" { "0".to_string() } else { s }
}

fn trim_zeros(s: &str) -> String {
    if s.contains('.') {
        s.trim_end_matches('0').trim_end_matches('.').to_string()
    } else {
        s.to_string()
    }
}

/// "0xFF" / "0b1010" for whole numbers that fit; None otherwise.
fn format_base(v: f64, base: Base) -> Option<String> {
    if v.fract() != 0.0 || v.abs() >= 9_007_199_254_740_992.0 {
        return None;
    }
    let sign = if v < 0.0 { "-" } else { "" };
    let n = v.abs() as u64;
    Some(match base {
        Base::Hex => format!("{sign}0x{n:X}"),
        Base::Bin => format!("{sign}0b{n:b}"),
        Base::Dec => format!("{sign}{n}"),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn calc(s: &str) -> Option<String> {
        evaluate(s, false)
    }

    #[test]
    fn basic_math() {
        assert_eq!(calc("12*8").as_deref(), Some("96"));
        assert_eq!(calc("12 × 8").as_deref(), Some("96"));
        assert_eq!(calc("12 x 8").as_deref(), Some("96"));
        assert_eq!(calc("1+2*3").as_deref(), Some("7"));
        assert_eq!(calc("(1+2)*3").as_deref(), Some("9"));
        assert_eq!(calc("10/4").as_deref(), Some("2.5"));
        assert_eq!(calc("2^10").as_deref(), Some("1024"));
        assert_eq!(calc("2**10").as_deref(), Some("1024"));
        assert_eq!(calc("2^3^2").as_deref(), Some("512"));
        assert_eq!(calc("-2^2").as_deref(), Some("-4"));
        assert_eq!(calc("2^-1").as_deref(), Some("0.5"));
        assert_eq!(calc("10 % 3").as_deref(), Some("1"));
        assert_eq!(calc("10 mod 4").as_deref(), Some("2"));
        assert_eq!(calc("0.1+0.2").as_deref(), Some("0.3"));
        assert_eq!(calc("1/3").as_deref(), Some("0.3333333333"));
        assert_eq!(calc("1e3*2").as_deref(), Some("2000"));
        assert_eq!(calc("3 - 5").as_deref(), Some("-2"));
    }

    #[test]
    fn functions_and_constants() {
        assert_eq!(calc("sqrt(2)").as_deref(), Some("1.414213562"));
        assert_eq!(calc("sqrt 16").as_deref(), Some("4"));
        assert_eq!(calc("2pi").as_deref(), Some("6.283185307"));
        assert_eq!(calc("2(3+1)").as_deref(), Some("8"));
        assert_eq!(calc("max(3, 9, 4)").as_deref(), Some("9"));
        assert_eq!(calc("min(3,9)").as_deref(), Some("3"));
        assert_eq!(calc("log(1000)").as_deref(), Some("3"));
        assert_eq!(calc("log(8, 2)").as_deref(), Some("3"));
        assert_eq!(calc("ln(e)").as_deref(), Some("1"));
        assert_eq!(calc("abs(-5)").as_deref(), Some("5"));
        assert_eq!(calc("round(2.5)").as_deref(), Some("3"));
        assert_eq!(calc("floor(2.7)+ceil(2.1)").as_deref(), Some("5"));
        assert_eq!(calc("sin(0)").as_deref(), Some("0"));
        assert_eq!(calc("cos(pi)").as_deref(), Some("-1"));
    }

    #[test]
    fn percentages() {
        assert_eq!(calc("15% of 240").as_deref(), Some("36"));
        assert_eq!(calc("200 + 15%").as_deref(), Some("230"));
        assert_eq!(calc("200 - 10%").as_deref(), Some("180"));
        assert_eq!(calc("50%").as_deref(), Some("0.5"));
        assert_eq!(calc("50% * 8").as_deref(), Some("4"));
    }

    #[test]
    fn hex_and_binary() {
        assert_eq!(calc("0xff").as_deref(), Some("255"));
        assert_eq!(calc("0b1010").as_deref(), Some("10"));
        assert_eq!(calc("255 in hex").as_deref(), Some("0xFF"));
        assert_eq!(calc("10 to bin").as_deref(), Some("0b1010"));
        assert_eq!(calc("0xff + 1").as_deref(), Some("256"));
        assert_eq!(calc("0x10 in dec").as_deref(), Some("16"));
        assert_eq!(calc("2.5 in hex"), None);
        assert_eq!(calc("0x"), None);
    }

    #[test]
    fn plain_values_only_with_the_prefix() {
        for s in ["12", "pi", "e", "-3", "(4)", "3.5"] {
            assert_eq!(calc(s), None, "{s}");
        }
        assert_eq!(evaluate("12", true).as_deref(), Some("12"));
        assert_eq!(evaluate("pi", true).as_deref(), Some("3.141592654"));
    }

    #[test]
    fn not_math() {
        for s in [
            "",
            "notepad",
            "calc",
            "spotify",
            "e dge",
            "2 3",
            "1/0",
            "sqrt(-1)",
            "(1+2",
            "1+",
            "*2",
            "max()",
            "sqrt(1, 2)",
            "rm -rf",
            "2 + system(1)",
            "5 km to mi",
            "1 in cm",
            "=",
            "lock",
        ] {
            assert_eq!(calc(s), None, "{s}");
        }
    }

    #[test]
    fn deep_nesting_is_refused_not_a_crash() {
        let deep = format!("{}1{}", "(".repeat(100), ")".repeat(100));
        assert_eq!(evaluate(&deep, true), None);
        let ok = format!("{}1+1{}", "(".repeat(20), ")".repeat(20));
        assert_eq!(calc(&ok).as_deref(), Some("2"));
        assert_eq!(calc(&"1+".repeat(200)), None);
    }

    #[test]
    fn number_format() {
        assert_eq!(format_number(1e20), "1e20");
        assert_eq!(format_number(1.5e-12), "1.5e-12");
        assert_eq!(format_number(123456789012.0), "123456789012");
        assert_eq!(format_number(-0.0), "0");
        assert_eq!(format_number(2.0 / 3.0), "0.6666666667");
    }

    #[test]
    fn provider_row() {
        let mut out = Vec::new();
        CalcProvider.query(&Query::new("12*8", false), &mut out);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].title, "96");
        assert_eq!(out[0].subtitle, "12*8 =");
        assert_eq!(out[0].action, HitAction::Copy("96".into()));
        assert_eq!(out[0].score, score::ANSWER);
    }
}
