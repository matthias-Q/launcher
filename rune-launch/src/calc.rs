//! A small calculator: `+ - * / % ^`, parentheses, implicit multiplication (`2pi`),
//! constants (`pi`, `e`, `tau`) and common functions (`sqrt`, `sin`, `ln`, ...).

use std::f64::consts;

use crate::entries::{Action, Entry, EntryIcon};

/// The result of `query` as a single entry, or nothing when it is not a calculation.
pub fn entries(query: &str) -> Vec<Entry> {
    let expression = query.trim();
    // Plain words ("e", "pi") and plain numbers are searches, not calculations.
    if !expression.chars().any(|c| c.is_ascii_digit()) {
        return Vec::new();
    }
    let Some(result) = evaluate(expression).and_then(format) else {
        return Vec::new();
    };
    if result == expression {
        return Vec::new();
    }
    vec![Entry {
        id: "result".to_string(),
        name: result.clone(),
        comment: Some(format!("= {expression}  ·  Enter to copy")),
        icon: Some(EntryIcon::Name("accessories-calculator".to_string())),
        keywords: String::new(),
        action: Action::Copy(result),
        confirm: false,
        expires: None,
    }]
}

pub fn evaluate(expression: &str) -> Option<f64> {
    let tokens = tokenize(expression)?;
    let mut parser = Parser { tokens, pos: 0 };
    let value = parser.expression()?;
    (parser.pos == parser.tokens.len()).then_some(value)
}

/// Integers without a fraction, everything else with up to 10 decimals.
fn format(value: f64) -> Option<String> {
    if !value.is_finite() {
        return None;
    }
    let value = if value == 0. { 0. } else { value };
    if value.abs() >= 1e15 || (value != 0. && value.abs() < 1e-6) {
        return Some(format!("{value:e}"));
    }
    let text = format!("{value:.10}");
    Some(text.trim_end_matches('0').trim_end_matches('.').to_string())
}

#[derive(Clone, Debug, PartialEq)]
enum Token {
    Number(f64),
    Ident(String),
    Op(char),
}

fn tokenize(input: &str) -> Option<Vec<Token>> {
    let mut tokens = Vec::new();
    let mut chars = input.chars().peekable();
    while let Some(&c) = chars.peek() {
        if c.is_whitespace() || c == '_' {
            chars.next();
        } else if c.is_ascii_digit() || c == '.' {
            let mut number = String::new();
            while let Some(&c) = chars.peek().filter(|c| c.is_ascii_digit() || **c == '.') {
                number.push(c);
                chars.next();
            }
            tokens.push(Token::Number(number.parse().ok()?));
        } else if c.is_alphabetic() {
            let mut ident = String::new();
            while let Some(&c) = chars.peek().filter(|c| c.is_alphanumeric()) {
                ident.push(c);
                chars.next();
            }
            tokens.push(Token::Ident(ident.to_lowercase()));
        } else {
            let op = match c {
                '+' | '-' | '*' | '/' | '%' | '^' | '(' | ')' => c,
                '×' | '·' => '*',
                '÷' => '/',
                '−' => '-',
                _ => return None,
            };
            tokens.push(Token::Op(op));
            chars.next();
        }
    }
    Some(tokens)
}

struct Parser {
    tokens: Vec<Token>,
    pos: usize,
}

impl Parser {
    fn peek(&self) -> Option<&Token> {
        self.tokens.get(self.pos)
    }

    fn eat(&mut self, op: char) -> bool {
        let matched = self.peek() == Some(&Token::Op(op));
        if matched {
            self.pos += 1;
        }
        matched
    }

    fn expression(&mut self) -> Option<f64> {
        let mut value = self.term()?;
        loop {
            if self.eat('+') {
                value += self.term()?;
            } else if self.eat('-') {
                value -= self.term()?;
            } else {
                return Some(value);
            }
        }
    }

    fn term(&mut self) -> Option<f64> {
        let mut value = self.unary()?;
        loop {
            if self.eat('*') {
                value *= self.unary()?;
            } else if self.eat('/') {
                value /= self.unary()?;
            } else if self.eat('%') {
                value %= self.unary()?;
            } else if matches!(
                self.peek(),
                Some(Token::Number(_) | Token::Ident(_) | Token::Op('('))
            ) {
                value *= self.power()?;
            } else {
                return Some(value);
            }
        }
    }

    fn unary(&mut self) -> Option<f64> {
        if self.eat('-') {
            Some(-self.unary()?)
        } else if self.eat('+') {
            self.unary()
        } else {
            self.power()
        }
    }

    fn power(&mut self) -> Option<f64> {
        let base = self.primary()?;
        if self.eat('^') {
            Some(base.powf(self.unary()?))
        } else {
            Some(base)
        }
    }

    fn primary(&mut self) -> Option<f64> {
        match self.tokens.get(self.pos)?.clone() {
            Token::Number(value) => {
                self.pos += 1;
                Some(value)
            }
            Token::Op('(') => {
                self.pos += 1;
                let value = self.expression()?;
                self.eat(')').then_some(value)
            }
            Token::Ident(name) => {
                self.pos += 1;
                if let Some(value) = constant(&name) {
                    return Some(value);
                }
                let function = function(&name)?;
                let argument = if self.eat('(') {
                    let value = self.expression()?;
                    self.eat(')').then_some(value)?
                } else {
                    self.power()?
                };
                Some(function(argument))
            }
            Token::Op(_) => None,
        }
    }
}

fn constant(name: &str) -> Option<f64> {
    match name {
        "pi" | "π" => Some(consts::PI),
        "tau" | "τ" => Some(consts::TAU),
        "e" => Some(consts::E),
        _ => None,
    }
}

fn function(name: &str) -> Option<fn(f64) -> f64> {
    Some(match name {
        "sqrt" => f64::sqrt,
        "cbrt" => f64::cbrt,
        "abs" => f64::abs,
        "sin" => f64::sin,
        "cos" => f64::cos,
        "tan" => f64::tan,
        "asin" => f64::asin,
        "acos" => f64::acos,
        "atan" => f64::atan,
        "ln" => f64::ln,
        "log" => f64::log10,
        "log2" => f64::log2,
        "exp" => f64::exp,
        "floor" => f64::floor,
        "ceil" => f64::ceil,
        "round" => f64::round,
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn calc(expression: &str) -> Option<String> {
        evaluate(expression).and_then(format)
    }

    #[test]
    fn evaluates_with_precedence() {
        assert_eq!(calc("1 + 2 * 3").as_deref(), Some("7"));
        assert_eq!(calc("(1 + 2) * 3").as_deref(), Some("9"));
        assert_eq!(calc("2 ^ 3 ^ 2").as_deref(), Some("512"));
        assert_eq!(calc("-2 ^ 2").as_deref(), Some("-4"));
        assert_eq!(calc("2 ^ -1").as_deref(), Some("0.5"));
        assert_eq!(calc("10 % 4").as_deref(), Some("2"));
        assert_eq!(calc("7 ÷ 2 × 4").as_deref(), Some("14"));
        assert_eq!(calc("1/3").as_deref(), Some("0.3333333333"));
    }

    #[test]
    fn supports_functions_and_constants() {
        assert_eq!(calc("sqrt(16)").as_deref(), Some("4"));
        assert_eq!(calc("sqrt 16 + 1").as_deref(), Some("5"));
        assert_eq!(calc("2pi").as_deref(), Some("6.2831853072"));
        assert_eq!(calc("2(3 + 1)").as_deref(), Some("8"));
        assert_eq!(calc("log(1000)").as_deref(), Some("3"));
        assert_eq!(calc("sin(0)").as_deref(), Some("0"));
    }

    #[test]
    fn rejects_non_calculations() {
        assert_eq!(calc("firefox"), None);
        assert_eq!(calc("1 +"), None);
        assert_eq!(calc("(1"), None);
        assert_eq!(calc("1 / 0"), None);
        assert!(entries("e").is_empty());
        assert!(entries("2048").is_empty());
        assert!(entries("7zip").is_empty());
        assert_eq!(entries("2+2")[0].name, "4");
    }
}
