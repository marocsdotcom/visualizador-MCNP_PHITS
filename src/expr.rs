//! Avaliador das expressões numéricas aceitas pelo PHITS (`c1*2+sqrt(3)`).

use std::collections::HashMap;

pub fn eval(s: &str, vars: &HashMap<String, f64>) -> Option<f64> {
    if !s.is_ascii() {
        return None;
    }
    let mut p = Parser { s: s.as_bytes(), i: 0, vars };
    let v = p.expr()?;
    p.ws();
    (p.i == p.s.len()).then_some(v)
}

struct Parser<'a> {
    s: &'a [u8],
    i: usize,
    vars: &'a HashMap<String, f64>,
}

impl Parser<'_> {
    fn ws(&mut self) {
        while self.s.get(self.i).is_some_and(|c| c.is_ascii_whitespace()) {
            self.i += 1;
        }
    }

    fn eat(&mut self, tok: &[u8]) -> bool {
        self.ws();
        if self.s[self.i..].starts_with(tok) {
            self.i += tok.len();
            true
        } else {
            false
        }
    }

    fn expr(&mut self) -> Option<f64> {
        let mut v = self.term()?;
        loop {
            if self.eat(b"+") {
                v += self.term()?;
            } else if self.eat(b"-") {
                v -= self.term()?;
            } else {
                return Some(v);
            }
        }
    }

    fn term(&mut self) -> Option<f64> {
        let mut v = self.unary()?;
        loop {
            self.ws();
            if self.s[self.i..].starts_with(b"**") {
                return Some(v);
            }
            if self.eat(b"*") {
                v *= self.unary()?;
            } else if self.eat(b"/") {
                v /= self.unary()?;
            } else {
                return Some(v);
            }
        }
    }

    fn unary(&mut self) -> Option<f64> {
        if self.eat(b"-") {
            return Some(-self.unary()?);
        }
        if self.eat(b"+") {
            return self.unary();
        }
        let base = self.atom()?;
        if self.eat(b"**") {
            return Some(base.powf(self.unary()?));
        }
        Some(base)
    }

    fn atom(&mut self) -> Option<f64> {
        self.ws();
        let c = *self.s.get(self.i)?;
        if c == b'(' {
            self.i += 1;
            let v = self.expr()?;
            return self.eat(b")").then_some(v);
        }
        if c.is_ascii_digit() || c == b'.' {
            return self.number();
        }
        if !c.is_ascii_alphabetic() {
            return None;
        }
        let start = self.i;
        while self.s.get(self.i).is_some_and(|c| c.is_ascii_alphanumeric() || *c == b'_') {
            self.i += 1;
        }
        let name = std::str::from_utf8(&self.s[start..self.i]).ok()?;
        if !self.eat(b"(") {
            return match name {
                "pi" => Some(std::f64::consts::PI),
                _ => self.vars.get(name).copied(),
            };
        }
        let mut args = vec![self.expr()?];
        while self.eat(b",") {
            args.push(self.expr()?);
        }
        if !self.eat(b")") {
            return None;
        }
        let x = args[0];
        let y = args.get(1).copied();
        Some(match name {
            "float" => x,
            "int" => x.trunc(),
            "nint" => x.round(),
            "abs" => x.abs(),
            "exp" => x.exp(),
            "log" => x.ln(),
            "log10" => x.log10(),
            "sqrt" => x.sqrt(),
            "sin" => x.sin(),
            "cos" => x.cos(),
            "tan" => x.tan(),
            "asin" => x.asin(),
            "acos" => x.acos(),
            "atan" => x.atan(),
            "sinh" => x.sinh(),
            "cosh" => x.cosh(),
            "tanh" => x.tanh(),
            "atan2" => x.atan2(y?),
            "max" => args.iter().copied().fold(f64::NEG_INFINITY, f64::max),
            "min" => args.iter().copied().fold(f64::INFINITY, f64::min),
            "mod" => x % y?,
            "sign" => x.abs() * y?.signum(),
            _ => return None,
        })
    }

    fn number(&mut self) -> Option<f64> {
        let start = self.i;
        let digits = |p: &mut Self| {
            while p.s.get(p.i).is_some_and(|c| c.is_ascii_digit() || *c == b'.') {
                p.i += 1;
            }
        };
        digits(self);
        if matches!(self.s.get(self.i), Some(b'e') | Some(b'd')) {
            let save = self.i;
            self.i += 1;
            if matches!(self.s.get(self.i), Some(b'+') | Some(b'-')) {
                self.i += 1;
            }
            if self.s.get(self.i).is_some_and(|c| c.is_ascii_digit()) {
                digits(self);
            } else {
                self.i = save;
            }
        }
        std::str::from_utf8(&self.s[start..self.i]).ok()?.replace('d', "e").parse().ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn arithmetic_and_variables() {
        let vars = HashMap::from([("c1".to_string(), 2.5)]);
        assert_eq!(eval("1+2*3", &vars), Some(7.0));
        assert_eq!(eval("-c1*2", &vars), Some(-5.0));
        assert_eq!(eval("2**3", &vars), Some(8.0));
        assert_eq!(eval("(1+1)/4", &vars), Some(0.5));
        assert_eq!(eval("sqrt(16)+max(1,2,3)", &vars), Some(7.0));
        assert_eq!(eval("1.5d1", &vars), Some(15.0));
        assert_eq!(eval("abc", &vars), None);
        assert_eq!(eval("1 2", &vars), None);
    }
}
