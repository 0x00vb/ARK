//! Spreadsheet formulas: enough to keep a workbook's values right after an edit (arithmetic,
//! comparisons, references across sheets, and the common functions of a summary). A formula
//! that uses anything else keeps the value the file had; the spreadsheet app recalculates it
//! when it opens the file.

use std::collections::{HashMap, HashSet};

use crate::model::{col_name, parse_number, parse_ref, plain_number, Value, Workbook};

// ---- tokens ------------------------------------------------------------------------------------

#[derive(Clone, Debug, PartialEq)]
enum T {
    Num(f64),
    Str(String),
    /// A name: a function, TRUE/FALSE, or a cell or column reference, with its sheet.
    Name(Option<String>, String),
    Op(&'static str),
    LParen,
    RParen,
    Comma,
    Colon,
}

/// Tokens with their byte spans in the source.
fn lex(src: &str) -> Option<Vec<(T, usize, usize)>> {
    let b = src.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < b.len() {
        let c = b[i] as char;
        let start = i;
        if c.is_ascii_whitespace() {
            i += 1;
            continue;
        }
        if c == '"' {
            let mut s = String::new();
            i += 1;
            loop {
                if i >= b.len() {
                    return None;
                }
                if b[i] == b'"' {
                    if b.get(i + 1) == Some(&b'"') {
                        s.push('"');
                        i += 2;
                        continue;
                    }
                    i += 1;
                    break;
                }
                let ch = src[i..].chars().next()?;
                s.push(ch);
                i += ch.len_utf8();
            }
            out.push((T::Str(s), start, i));
            continue;
        }
        if c.is_ascii_digit() || (c == '.' && b.get(i + 1).is_some_and(|d| d.is_ascii_digit())) {
            // A number, unless it is a row range like 1:3 (not supported, read as numbers).
            while i < b.len() && (b[i].is_ascii_digit() || b[i] == b'.') {
                i += 1;
            }
            if i < b.len() && (b[i] == b'e' || b[i] == b'E') {
                let mut j = i + 1;
                if j < b.len() && (b[j] == b'+' || b[j] == b'-') {
                    j += 1;
                }
                if j < b.len() && b[j].is_ascii_digit() {
                    i = j;
                    while i < b.len() && b[i].is_ascii_digit() {
                        i += 1;
                    }
                }
            }
            out.push((T::Num(src[start..i].parse().ok()?), start, i));
            continue;
        }
        let mut sheet = None;
        if c == '\'' {
            // 'My Sheet'!A1
            let mut s = String::new();
            i += 1;
            loop {
                if i >= b.len() {
                    return None;
                }
                if b[i] == b'\'' {
                    if b.get(i + 1) == Some(&b'\'') {
                        s.push('\'');
                        i += 2;
                        continue;
                    }
                    i += 1;
                    break;
                }
                let ch = src[i..].chars().next()?;
                s.push(ch);
                i += ch.len_utf8();
            }
            if b.get(i) != Some(&b'!') {
                return None;
            }
            i += 1;
            sheet = Some(s);
        }
        let name_start = i;
        while i < b.len() {
            let ch = src[i..].chars().next()?;
            if ch.is_alphanumeric() || matches!(ch, '_' | '$' | '.') {
                i += ch.len_utf8();
            } else {
                break;
            }
        }
        if i > name_start {
            let mut name = src[name_start..i].to_string();
            if sheet.is_none() && b.get(i) == Some(&b'!') {
                sheet = Some(name);
                i += 1;
                let s2 = i;
                while i < b.len() && ((b[i] as char).is_ascii_alphanumeric() || b[i] == b'$') {
                    i += 1;
                }
                name = src[s2..i].to_string();
            }
            out.push((T::Name(sheet, name), start, i));
            continue;
        }
        if sheet.is_some() {
            return None;
        }
        let two = src.get(i..i + 2).unwrap_or("");
        let op: Option<&'static str> = match two {
            "<=" => Some("<="),
            ">=" => Some(">="),
            "<>" => Some("<>"),
            _ => None,
        };
        if let Some(op) = op {
            out.push((T::Op(op), start, i + 2));
            i += 2;
            continue;
        }
        let tok = match c {
            '+' => T::Op("+"),
            '-' => T::Op("-"),
            '*' => T::Op("*"),
            '/' => T::Op("/"),
            '^' => T::Op("^"),
            '&' => T::Op("&"),
            '=' => T::Op("="),
            '<' => T::Op("<"),
            '>' => T::Op(">"),
            '%' => T::Op("%"),
            '(' => T::LParen,
            ')' => T::RParen,
            ',' | ';' => T::Comma,
            ':' => T::Colon,
            _ => return None,
        };
        i += 1;
        out.push((tok, start, i));
    }
    Some(out)
}

// ---- parse -------------------------------------------------------------------------------------

#[derive(Clone, Debug)]
enum E {
    Num(f64),
    Str(String),
    Bool(bool),
    Ref(Option<String>, u32, u32),
    /// Rows `None` is a whole column (`A:A`).
    Range(Option<String>, Option<(u32, u32)>, u32, u32),
    Neg(Box<E>),
    Pct(Box<E>),
    Bin(&'static str, Box<E>, Box<E>),
    Call(String, Vec<E>),
}

struct P {
    t: Vec<T>,
    i: usize,
}

impl P {
    fn peek(&self) -> Option<&T> {
        self.t.get(self.i)
    }
    fn next(&mut self) -> Option<T> {
        let t = self.t.get(self.i).cloned();
        self.i += 1;
        t
    }
    fn binary(&mut self, level: usize) -> Option<E> {
        const LEVELS: [&[&str]; 4] = [
            &["=", "<>", "<", ">", "<=", ">="],
            &["&"],
            &["+", "-"],
            &["*", "/"],
        ];
        if level == LEVELS.len() {
            return self.power();
        }
        let mut left = self.binary(level + 1)?;
        while let Some(T::Op(op)) = self.peek() {
            let op = *op;
            if !LEVELS[level].contains(&op) {
                break;
            }
            self.i += 1;
            let right = self.binary(level + 1)?;
            left = E::Bin(op, Box::new(left), Box::new(right));
        }
        Some(left)
    }
    fn power(&mut self) -> Option<E> {
        let mut left = self.unary()?;
        while let Some(T::Op("^")) = self.peek() {
            self.i += 1;
            let right = self.unary()?;
            left = E::Bin("^", Box::new(left), Box::new(right));
        }
        Some(left)
    }
    fn unary(&mut self) -> Option<E> {
        match self.peek() {
            Some(T::Op("-")) => {
                self.i += 1;
                Some(E::Neg(Box::new(self.unary()?)))
            }
            Some(T::Op("+")) => {
                self.i += 1;
                self.unary()
            }
            _ => {
                let mut e = self.atom()?;
                while let Some(T::Op("%")) = self.peek() {
                    self.i += 1;
                    e = E::Pct(Box::new(e));
                }
                Some(e)
            }
        }
    }
    fn atom(&mut self) -> Option<E> {
        match self.next()? {
            T::Num(n) => Some(E::Num(n)),
            T::Str(s) => Some(E::Str(s)),
            T::LParen => {
                let e = self.binary(0)?;
                (self.next()? == T::RParen).then_some(e)
            }
            T::Name(sheet, name) => {
                if self.peek() == Some(&T::LParen) && sheet.is_none() {
                    self.i += 1;
                    let mut args = Vec::new();
                    if self.peek() == Some(&T::RParen) {
                        self.i += 1;
                    } else {
                        loop {
                            args.push(self.binary(0)?);
                            match self.next()? {
                                T::Comma => continue,
                                T::RParen => break,
                                _ => return None,
                            }
                        }
                    }
                    let name = name.to_ascii_uppercase();
                    let name = name.strip_prefix("_XLFN.").unwrap_or(&name).to_string();
                    return Some(E::Call(name, args));
                }
                let upper = name.to_ascii_uppercase();
                if sheet.is_none() && (upper == "TRUE" || upper == "FALSE") {
                    return Some(E::Bool(upper == "TRUE"));
                }
                if self.peek() == Some(&T::Colon) {
                    self.i += 1;
                    let Some(T::Name(_, end)) = self.next() else {
                        return None;
                    };
                    if let (Some(a), Some(b)) = (parse_ref(&name), parse_ref(&end)) {
                        return Some(E::Range(
                            sheet,
                            Some((a.0.min(b.0), b.0.max(a.0))),
                            a.1.min(b.1),
                            a.1.max(b.1),
                        ));
                    }
                    let (a, b) = (col_index(&name)?, col_index(&end)?);
                    return Some(E::Range(sheet, None, a.min(b), a.max(b)));
                }
                let (r, c) = parse_ref(&name)?;
                Some(E::Ref(sheet, r, c))
            }
            _ => None,
        }
    }
}

fn col_index(s: &str) -> Option<u32> {
    let s = s.replace('$', "").to_ascii_uppercase();
    if s.is_empty() || s.len() > 3 || !s.chars().all(|c| c.is_ascii_uppercase()) {
        return None;
    }
    Some(s.bytes().fold(0u32, |a, c| a * 26 + (c - b'A' + 1) as u32) - 1)
}

fn parse(src: &str) -> Option<E> {
    let toks = lex(src)?;
    let mut p = P {
        t: toks.into_iter().map(|t| t.0).collect(),
        i: 0,
    };
    let e = p.binary(0)?;
    (p.i == p.t.len()).then_some(e)
}

// ---- values ------------------------------------------------------------------------------------

#[derive(Clone, Debug, PartialEq)]
enum V {
    Num(f64),
    Str(String),
    Bool(bool),
    Err(&'static str),
    Empty,
    List(Vec<V>),
}

/// A formula uses something this evaluator does not know; keep the file's value.
struct Unknown;

type R = Result<V, Unknown>;

impl V {
    fn num(&self) -> Result<f64, &'static str> {
        match self {
            V::Num(n) => Ok(*n),
            V::Bool(b) => Ok(if *b { 1.0 } else { 0.0 }),
            V::Empty => Ok(0.0),
            V::Str(s) => parse_number(s).ok_or("#VALUE!"),
            V::Err(e) => Err(e),
            V::List(l) => match l.as_slice() {
                [one] => one.num(),
                _ => Err("#VALUE!"),
            },
        }
    }
    fn text(&self) -> Result<String, &'static str> {
        match self {
            V::Num(n) => Ok(plain_number(*n)),
            V::Str(s) => Ok(s.clone()),
            V::Bool(b) => Ok(if *b { "TRUE" } else { "FALSE" }.into()),
            V::Empty => Ok(String::new()),
            V::Err(e) => Err(e),
            V::List(l) => match l.as_slice() {
                [one] => one.text(),
                _ => Err("#VALUE!"),
            },
        }
    }
    fn truth(&self) -> Result<bool, &'static str> {
        match self {
            V::Bool(b) => Ok(*b),
            V::Str(s) if s.eq_ignore_ascii_case("true") => Ok(true),
            V::Str(s) if s.eq_ignore_ascii_case("false") => Ok(false),
            V::Str(_) => Err("#VALUE!"),
            other => other.num().map(|n| n != 0.0),
        }
    }
    fn scalar(self) -> V {
        match self {
            V::List(mut l) if l.len() == 1 => l.pop().unwrap_or(V::Empty),
            V::List(_) => V::Err("#VALUE!"),
            v => v,
        }
    }
    fn flat(self, out: &mut Vec<V>) {
        match self {
            V::List(l) => l.into_iter().for_each(|v| v.flat(out)),
            v => out.push(v),
        }
    }
}

struct Calc<'a> {
    book: &'a Workbook,
    done: HashMap<(usize, u32, u32), Option<V>>,
    busy: HashSet<(usize, u32, u32)>,
    /// Shared-formula and edited cells: their formula text, per (sheet, row, col).
    formulas: HashMap<(usize, u32, u32), E>,
}

impl<'a> Calc<'a> {
    fn sheet(&self, name: &Option<String>, cur: usize) -> Option<usize> {
        match name {
            None => Some(cur),
            Some(n) => self
                .book
                .sheets
                .iter()
                .position(|s| s.name.eq_ignore_ascii_case(n)),
        }
    }

    fn cell(&mut self, s: usize, r: u32, c: u32) -> R {
        let key = (s, r, c);
        if let Some(v) = self.done.get(&key) {
            return v.clone().ok_or(Unknown);
        }
        let Some(cell) = self.book.sheets[s].get(r, c) else {
            return Ok(V::Empty);
        };
        if cell.formula.is_none() {
            return Ok(stored(&cell.value));
        }
        if !self.busy.insert(key) {
            return Ok(V::Err("#REF!"));
        }
        let v = match self.formulas.get(&key).cloned() {
            Some(e) => self.eval(&e, s).map(V::scalar),
            None => Err(Unknown),
        };
        self.busy.remove(&key);
        self.done.insert(key, v.as_ref().ok().cloned());
        v
    }

    fn range(&mut self, s: usize, rows: Option<(u32, u32)>, c0: u32, c1: u32) -> R {
        let (r0, r1) = match rows {
            Some(r) => r,
            None => {
                let (n, _) = self.book.sheets[s].size();
                if n == 0 {
                    return Ok(V::List(Vec::new()));
                }
                (0, n - 1)
            }
        };
        if (r1 - r0 + 1) as u64 * (c1 - c0 + 1) as u64 > 1_000_000 {
            return Ok(V::Err("#NUM!"));
        }
        let mut out = Vec::new();
        for r in r0..=r1 {
            for c in c0..=c1 {
                out.push(self.cell(s, r, c)?);
            }
        }
        Ok(V::List(out))
    }

    fn eval(&mut self, e: &E, cur: usize) -> R {
        Ok(match e {
            E::Num(n) => V::Num(*n),
            E::Str(s) => V::Str(s.clone()),
            E::Bool(b) => V::Bool(*b),
            E::Ref(sheet, r, c) => match self.sheet(sheet, cur) {
                Some(s) => self.cell(s, *r, *c)?,
                None => V::Err("#REF!"),
            },
            E::Range(sheet, rows, c0, c1) => match self.sheet(sheet, cur) {
                Some(s) => self.range(s, *rows, *c0, *c1)?,
                None => V::Err("#REF!"),
            },
            E::Neg(x) => match self.eval(x, cur)?.num() {
                Ok(n) => V::Num(-n),
                Err(e) => V::Err(e),
            },
            E::Pct(x) => match self.eval(x, cur)?.num() {
                Ok(n) => V::Num(n / 100.0),
                Err(e) => V::Err(e),
            },
            E::Bin(op, a, b) => {
                let (a, b) = (self.eval(a, cur)?.scalar(), self.eval(b, cur)?.scalar());
                binary(op, &a, &b)
            }
            E::Call(name, args) => self.call(name, args, cur)?,
        })
    }

    /// Every argument, ranges flattened.
    fn values(&mut self, args: &[E], cur: usize) -> Result<Vec<(V, bool)>, Unknown> {
        let mut out = Vec::new();
        for a in args {
            let direct = !matches!(a, E::Range(..));
            let mut flat = Vec::new();
            self.eval(a, cur)?.flat(&mut flat);
            out.extend(flat.into_iter().map(|v| (v, direct)));
        }
        Ok(out)
    }

    /// The numbers of the arguments: a range counts only its numbers; a typed argument counts
    /// as a number when it is one.
    fn numbers(
        &mut self,
        args: &[E],
        cur: usize,
    ) -> Result<Result<Vec<f64>, &'static str>, Unknown> {
        let mut out = Vec::new();
        for (v, direct) in self.values(args, cur)? {
            match v {
                V::Num(n) => out.push(n),
                V::Err(e) => return Ok(Err(e)),
                V::Bool(b) if direct => out.push(if b { 1.0 } else { 0.0 }),
                V::Str(s) if direct => match parse_number(&s) {
                    Some(n) => out.push(n),
                    None => return Ok(Err("#VALUE!")),
                },
                _ => {}
            }
        }
        Ok(Ok(out))
    }

    fn call(&mut self, name: &str, args: &[E], cur: usize) -> R {
        let arg = |i: usize| args.get(i);
        macro_rules! nums {
            () => {
                match self.numbers(args, cur)? {
                    Ok(n) => n,
                    Err(e) => return Ok(V::Err(e)),
                }
            };
        }
        macro_rules! one {
            ($i:expr) => {
                match arg($i) {
                    Some(a) => self.eval(a, cur)?.scalar(),
                    None => return Ok(V::Err("#VALUE!")),
                }
            };
        }
        macro_rules! num {
            ($i:expr) => {
                match one!($i).num() {
                    Ok(n) => n,
                    Err(e) => return Ok(V::Err(e)),
                }
            };
        }
        macro_rules! text {
            ($i:expr) => {
                match one!($i).text() {
                    Ok(t) => t,
                    Err(e) => return Ok(V::Err(e)),
                }
            };
        }
        Ok(match name {
            "SUM" => V::Num(nums!().iter().sum()),
            "PRODUCT" => V::Num(nums!().iter().product()),
            "AVERAGE" => {
                let n = nums!();
                if n.is_empty() {
                    V::Err("#DIV/0!")
                } else {
                    V::Num(n.iter().sum::<f64>() / n.len() as f64)
                }
            }
            "MIN" => V::Num(nums!().into_iter().reduce(f64::min).unwrap_or(0.0)),
            "MAX" => V::Num(nums!().into_iter().reduce(f64::max).unwrap_or(0.0)),
            "COUNT" => V::Num(
                self.values(args, cur)?
                    .iter()
                    .filter(|(v, d)| {
                        matches!(v, V::Num(_)) || (*d && v.num().is_ok() && !matches!(v, V::Empty))
                    })
                    .count() as f64,
            ),
            "COUNTA" => V::Num(
                self.values(args, cur)?
                    .iter()
                    .filter(|(v, _)| !matches!(v, V::Empty))
                    .count() as f64,
            ),
            "COUNTBLANK" => V::Num(
                self.values(args, cur)?
                    .iter()
                    .filter(|(v, _)| matches!(v, V::Empty) || *v == V::Str(String::new()))
                    .count() as f64,
            ),
            "ROUND" | "ROUNDUP" | "ROUNDDOWN" => {
                let x = num!(0);
                let d = if args.len() > 1 { num!(1) } else { 0.0 };
                let m = 10f64.powi(d as i32);
                let y = x * m;
                let r = match name {
                    // Half away from zero, as spreadsheets round.
                    "ROUND" => y.round(),
                    "ROUNDUP" => y.abs().ceil().copysign(y),
                    _ => y.abs().floor().copysign(y),
                };
                V::Num(r / m)
            }
            "INT" => V::Num(num!(0).floor()),
            "ABS" => V::Num(num!(0).abs()),
            "SQRT" => {
                let x = num!(0);
                if x < 0.0 {
                    V::Err("#NUM!")
                } else {
                    V::Num(x.sqrt())
                }
            }
            "POWER" => V::Num(num!(0).powf(num!(1))),
            "MOD" => {
                let (a, b) = (num!(0), num!(1));
                if b == 0.0 {
                    V::Err("#DIV/0!")
                } else {
                    V::Num(a - b * (a / b).floor())
                }
            }
            "IF" => {
                let c = match one!(0).truth() {
                    Ok(c) => c,
                    Err(e) => return Ok(V::Err(e)),
                };
                match (c, arg(1), arg(2)) {
                    (true, Some(a), _) | (false, _, Some(a)) => self.eval(a, cur)?.scalar(),
                    (true, None, _) => V::Bool(true),
                    (false, _, None) => V::Bool(false),
                }
            }
            "IFERROR" => match one!(0) {
                V::Err(_) => match arg(1) {
                    Some(a) => self.eval(a, cur)?.scalar(),
                    None => V::Empty,
                },
                v => v,
            },
            "AND" | "OR" => {
                let mut acc = name == "AND";
                for (v, _) in self.values(args, cur)? {
                    if matches!(v, V::Empty) {
                        continue;
                    }
                    match v.truth() {
                        Ok(t) if name == "AND" => acc &= t,
                        Ok(t) => acc |= t,
                        Err(e) => return Ok(V::Err(e)),
                    }
                }
                V::Bool(acc)
            }
            "NOT" => match one!(0).truth() {
                Ok(t) => V::Bool(!t),
                Err(e) => V::Err(e),
            },
            "CONCAT" | "CONCATENATE" => {
                let mut s = String::new();
                for (v, _) in self.values(args, cur)? {
                    match v.text() {
                        Ok(t) => s.push_str(&t),
                        Err(e) => return Ok(V::Err(e)),
                    }
                }
                V::Str(s)
            }
            "LEN" => V::Num(text!(0).chars().count() as f64),
            "UPPER" => V::Str(text!(0).to_uppercase()),
            "LOWER" => V::Str(text!(0).to_lowercase()),
            "TRIM" => V::Str(text!(0).split_whitespace().collect::<Vec<_>>().join(" ")),
            "LEFT" | "RIGHT" => {
                let t = text!(0);
                let n = if args.len() > 1 {
                    num!(1).max(0.0) as usize
                } else {
                    1
                };
                let chars: Vec<char> = t.chars().collect();
                let n = n.min(chars.len());
                V::Str(if name == "LEFT" {
                    chars[..n].iter().collect()
                } else {
                    chars[chars.len() - n..].iter().collect()
                })
            }
            "MID" => {
                let t: Vec<char> = text!(0).chars().collect();
                let start = (num!(1).max(1.0) as usize - 1).min(t.len());
                let n = (num!(2).max(0.0) as usize).min(t.len() - start);
                V::Str(t[start..start + n].iter().collect())
            }
            "SUMIF" | "COUNTIF" | "AVERAGEIF" => {
                let (Some(range), Some(crit)) = (arg(0), arg(1)) else {
                    return Ok(V::Err("#VALUE!"));
                };
                let mut cells = Vec::new();
                self.eval(range, cur)?.flat(&mut cells);
                let crit = self.eval(crit, cur)?.scalar();
                let sum_cells = match arg(2) {
                    Some(r) if name != "COUNTIF" => {
                        let mut v = Vec::new();
                        self.eval(r, cur)?.flat(&mut v);
                        v
                    }
                    _ => cells.clone(),
                };
                let mut total = 0.0;
                let mut count = 0usize;
                for (i, v) in cells.iter().enumerate() {
                    if matches_criterion(v, &crit) {
                        count += 1;
                        if let Some(V::Num(n)) = sum_cells.get(i) {
                            total += n;
                        }
                    }
                }
                match name {
                    "COUNTIF" => V::Num(count as f64),
                    "SUMIF" => V::Num(total),
                    _ if count == 0 => V::Err("#DIV/0!"),
                    _ => V::Num(total / count as f64),
                }
            }
            "MATCH" => {
                let key = one!(0);
                let Some(range) = arg(1) else {
                    return Ok(V::Err("#N/A"));
                };
                let mut cells = Vec::new();
                self.eval(range, cur)?.flat(&mut cells);
                let kind = if args.len() > 2 { num!(2) } else { 1.0 };
                let mut found = None;
                for (i, v) in cells.iter().enumerate() {
                    if kind == 0.0 {
                        if equal(v, &key) {
                            found = Some(i);
                            break;
                        }
                    } else if compare(v, &key).is_some_and(|o| {
                        if kind > 0.0 {
                            o.is_le()
                        } else {
                            o.is_ge()
                        }
                    }) {
                        found = Some(i);
                    } else if found.is_some() {
                        break;
                    }
                }
                match found {
                    Some(i) => V::Num((i + 1) as f64),
                    None => V::Err("#N/A"),
                }
            }
            "INDEX" => {
                let Some(E::Range(sheet, rows, c0, c1)) = arg(0) else {
                    return Ok(V::Err("#REF!"));
                };
                let Some(s) = self.sheet(sheet, cur) else {
                    return Ok(V::Err("#REF!"));
                };
                let (r0, r1) = rows.unwrap_or((0, self.book.sheets[s].size().0.saturating_sub(1)));
                let (height, width) = (r1 - r0 + 1, c1 - c0 + 1);
                let a = num!(1).max(0.0) as u32;
                let b = if args.len() > 2 {
                    num!(2).max(0.0) as u32
                } else {
                    0
                };
                // One row or one column: the single number counts along it.
                let (r, c) = match (height, width, b) {
                    (1, _, 0) => (0, a.saturating_sub(1)),
                    (_, 1, 0) => (a.saturating_sub(1), 0),
                    _ => (a.saturating_sub(1), b.saturating_sub(1)),
                };
                if r >= height || c >= width || (a == 0 && b == 0) {
                    V::Err("#REF!")
                } else {
                    self.cell(s, r0 + r, c0 + c)?
                }
            }
            "VLOOKUP" => {
                let key = one!(0);
                let Some(E::Range(sheet, rows, c0, c1)) = arg(1) else {
                    return Ok(V::Err("#VALUE!"));
                };
                let col = num!(2) as u32;
                let exact = match arg(3) {
                    Some(a) => !self.eval(a, cur)?.scalar().truth().unwrap_or(true),
                    None => false,
                };
                let Some(s) = self.sheet(sheet, cur) else {
                    return Ok(V::Err("#REF!"));
                };
                if col == 0 || c0 + col - 1 > *c1 {
                    return Ok(V::Err("#REF!"));
                }
                let (r0, r1) = rows.unwrap_or((0, self.book.sheets[s].size().0.saturating_sub(1)));
                let mut found = None;
                for r in r0..=r1 {
                    let v = self.cell(s, r, *c0)?;
                    if equal(&v, &key) {
                        found = Some(r);
                        break;
                    }
                    if !exact && compare(&v, &key).is_some_and(|o| o.is_le()) {
                        found = Some(r);
                    }
                }
                match found {
                    Some(r) => self.cell(s, r, c0 + col - 1)?,
                    None => V::Err("#N/A"),
                }
            }
            _ => return Err(Unknown),
        })
    }
}

fn stored(v: &Value) -> V {
    match v {
        Value::Empty => V::Empty,
        Value::Number(n) => V::Num(*n),
        Value::Text(s) => V::Str(s.clone()),
        Value::Bool(b) => V::Bool(*b),
        Value::Error(e) => V::Err(match e.as_str() {
            "#DIV/0!" => "#DIV/0!",
            "#N/A" => "#N/A",
            "#NAME?" => "#NAME?",
            "#NUM!" => "#NUM!",
            "#REF!" => "#REF!",
            _ => "#VALUE!",
        }),
    }
}

fn binary(op: &str, a: &V, b: &V) -> V {
    if let V::Err(e) = a {
        return V::Err(e);
    }
    if let V::Err(e) = b {
        return V::Err(e);
    }
    match op {
        "&" => match (a.text(), b.text()) {
            (Ok(x), Ok(y)) => V::Str(x + &y),
            (Err(e), _) | (_, Err(e)) => V::Err(e),
        },
        "=" | "<>" | "<" | ">" | "<=" | ">=" => {
            let ord = compare(a, b);
            V::Bool(match op {
                "=" => equal(a, b),
                "<>" => !equal(a, b),
                "<" => ord.is_some_and(|o| o.is_lt()),
                ">" => ord.is_some_and(|o| o.is_gt()),
                "<=" => ord.is_some_and(|o| o.is_le()),
                _ => ord.is_some_and(|o| o.is_ge()),
            })
        }
        _ => {
            let (x, y) = match (a.num(), b.num()) {
                (Ok(x), Ok(y)) => (x, y),
                (Err(e), _) | (_, Err(e)) => return V::Err(e),
            };
            match op {
                "+" => V::Num(x + y),
                "-" => V::Num(x - y),
                "*" => V::Num(x * y),
                "/" if y == 0.0 => V::Err("#DIV/0!"),
                "/" => V::Num(x / y),
                _ => {
                    let r = x.powf(y);
                    if r.is_finite() {
                        V::Num(r)
                    } else {
                        V::Err("#NUM!")
                    }
                }
            }
        }
    }
}

fn equal(a: &V, b: &V) -> bool {
    match (a, b) {
        (V::Str(x), V::Str(y)) => x.eq_ignore_ascii_case(y),
        (V::Empty, V::Str(s)) | (V::Str(s), V::Empty) => s.is_empty(),
        _ => compare(a, b).is_some_and(|o| o.is_eq()),
    }
}

fn compare(a: &V, b: &V) -> Option<std::cmp::Ordering> {
    match (a, b) {
        (V::Str(x), V::Str(y)) => Some(x.to_lowercase().cmp(&y.to_lowercase())),
        (V::Num(_) | V::Empty | V::Bool(_), V::Num(_) | V::Empty | V::Bool(_)) => {
            a.num().ok()?.partial_cmp(&b.num().ok()?)
        }
        // Numbers sort before text.
        (V::Num(_), V::Str(_)) => Some(std::cmp::Ordering::Less),
        (V::Str(_), V::Num(_)) => Some(std::cmp::Ordering::Greater),
        _ => None,
    }
}

/// `">5"`, `"<>x"`, `"north"`, `5`.
fn matches_criterion(v: &V, crit: &V) -> bool {
    let V::Str(c) = crit else {
        return equal(v, crit);
    };
    for op in ["<=", ">=", "<>", "<", ">", "="] {
        if let Some(rest) = c.strip_prefix(op) {
            let target = match parse_number(rest) {
                Some(n) => V::Num(n),
                None => V::Str(rest.to_string()),
            };
            if matches!(v, V::Empty) && op != "<>" {
                return false;
            }
            return match binary(op, v, &target) {
                V::Bool(b) => b,
                _ => false,
            };
        }
    }
    match parse_number(c) {
        Some(n) => equal(v, &V::Num(n)),
        None => equal(v, crit),
    }
}

/// Compute every formula the evaluator understands and store its value in the cell. A formula
/// it does not understand keeps its value.
pub fn recalc(book: &mut Workbook) {
    let mut formulas = HashMap::new();
    for (s, sheet) in book.sheets.iter().enumerate() {
        for (&(r, c), cell) in &sheet.cells {
            if let Some(f) = &cell.formula {
                if let Some(e) = parse(f) {
                    formulas.insert((s, r, c), e);
                }
            }
        }
    }
    if formulas.is_empty() {
        return;
    }
    let keys: Vec<(usize, u32, u32)> = book
        .sheets
        .iter()
        .enumerate()
        .flat_map(|(s, sh)| {
            sh.cells
                .iter()
                .filter(|(_, c)| c.formula.is_some())
                .map(move |(k, _)| (s, k.0, k.1))
        })
        .collect();
    let results: Vec<((usize, u32, u32), V)> = {
        let mut calc = Calc {
            book,
            done: HashMap::new(),
            busy: HashSet::new(),
            formulas,
        };
        keys.into_iter()
            .filter_map(|k| calc.cell(k.0, k.1, k.2).ok().map(|v| (k, v)))
            .collect()
    };
    for ((s, r, c), v) in results {
        if let Some(cell) = book.sheets[s].cells.get_mut(&(r, c)) {
            cell.value = match v {
                V::Num(n) => Value::Number(n),
                V::Str(t) => Value::Text(t),
                V::Bool(b) => Value::Bool(b),
                V::Err(e) => Value::Error(e.into()),
                V::Empty => Value::Number(0.0),
                V::List(_) => Value::Error("#VALUE!".into()),
            };
        }
    }
}

/// A formula moved by (rows, columns): references without `$` move with it. How a shared
/// formula's text reaches the cells that share it.
pub fn shift(src: &str, dr: i64, dc: i64) -> String {
    let Some(toks) = lex(src) else {
        return src.to_string();
    };
    let mut out = String::new();
    let mut last = 0;
    for (i, (t, _, end)) in toks.iter().enumerate() {
        let T::Name(_, name) = t else {
            continue;
        };
        let next = toks.get(i + 1).map(|t| &t.0);
        let prev = i.checked_sub(1).map(|p| &toks[p].0);
        // A function name is not a reference; a bare column (A in A:A) only is beside a colon.
        if next == Some(&T::LParen) {
            continue;
        }
        let column_only = !name.bytes().any(|b| b.is_ascii_digit());
        if column_only && next != Some(&T::Colon) && prev != Some(&T::Colon) {
            continue;
        }
        // The reference part is the name's tail (after any `Sheet!`).
        let at = end - name.len();
        let Some(moved) = shift_ref(name, dr, dc) else {
            continue;
        };
        out.push_str(&src[last..at]);
        out.push_str(&moved);
        last = *end;
    }
    out.push_str(&src[last..]);
    out
}

fn shift_ref(name: &str, dr: i64, dc: i64) -> Option<String> {
    let b = name.as_bytes();
    let mut i = 0;
    let col_abs = b.first() == Some(&b'$');
    if col_abs {
        i += 1;
    }
    let cs = i;
    while i < b.len() && b[i].is_ascii_alphabetic() {
        i += 1;
    }
    let letters = &name[cs..i];
    let row_abs = b.get(i) == Some(&b'$');
    if row_abs {
        i += 1;
    }
    let rs = i;
    while i < b.len() && b[i].is_ascii_digit() {
        i += 1;
    }
    if i != b.len() || letters.is_empty() || letters.len() > 3 {
        return None;
    }
    let digits = &name[rs..];
    let col = col_index(letters)? as i64;
    let new_col = if col_abs { col } else { col + dc };
    if new_col < 0 {
        return None;
    }
    let mut s = String::new();
    if col_abs {
        s.push('$');
    }
    s.push_str(&col_name(new_col as u32));
    if digits.is_empty() {
        // A column reference (A:A).
        return Some(s);
    }
    let row: i64 = digits.parse().ok()?;
    let new_row = if row_abs { row } else { row + dr };
    if new_row < 1 {
        return None;
    }
    if row_abs {
        s.push('$');
    }
    s.push_str(&new_row.to_string());
    Some(s)
}

/// A formula with sheet `from` renamed to `to` (quoted when the name needs it).
pub fn rename_sheet(src: &str, from: &str, to: &str) -> String {
    let Some(toks) = lex(src) else {
        return src.to_string();
    };
    let quoted = if to.chars().all(|c| c.is_alphanumeric() || c == '_') {
        to.to_string()
    } else {
        format!("'{}'", to.replace('\'', "''"))
    };
    let mut out = String::new();
    let mut last = 0;
    for (t, start, end) in toks {
        if let T::Name(Some(sheet), name) = &t {
            if sheet.eq_ignore_ascii_case(from) {
                out.push_str(&src[last..start]);
                out.push_str(&format!("{quoted}!{name}"));
                last = end;
            }
        }
    }
    out.push_str(&src[last..]);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Cell, Sheet};

    fn book(cells: &[(&str, &str)]) -> Workbook {
        let mut s = Sheet::new("Data");
        for (at, v) in cells {
            let (r, c) = parse_ref(at).unwrap();
            s.cells.insert(
                (r, c),
                crate::model::typed(&serde_json::json!(v), Default::default()),
            );
        }
        Workbook { sheets: vec![s] }
    }

    fn value(b: &Workbook, at: &str) -> Value {
        let (r, c) = parse_ref(at).unwrap();
        b.sheets[0]
            .get(r, c)
            .map(|c| c.value.clone())
            .unwrap_or_default()
    }

    #[test]
    fn evaluates_a_summary() {
        let mut b = book(&[
            ("A1", "North"),
            ("B1", "120"),
            ("A2", "South"),
            ("B2", "80"),
            ("A3", "North"),
            ("B3", "40"),
            ("C1", "=SUM(B1:B3)"),
            ("C2", "=AVERAGE(B:B)"),
            ("C3", "=SUMIF(A1:A3,\"North\",B1:B3)"),
            ("C4", "=IF(C1>200,\"big\",\"small\")&\"!\""),
            ("C5", "=ROUND(B1/7,2)"),
            ("C6", "=B1/0"),
            ("C7", "=COUNTIF(B1:B3,\">50\")"),
            ("C8", "=VLOOKUP(\"south\",A1:B3,2,FALSE)"),
            ("C9", "=-2^2+10%"),
            ("C10", "=INDEX(A1:A3,MATCH(MAX(B1:B3),B1:B3,0))"),
            ("D1", "=NOSUCH(1)"),
            ("E1", "=E2"),
            ("E2", "=E1"),
        ]);
        b.sheets[0].cells.get_mut(&(0, 3)).unwrap().value = Value::Number(7.0);
        recalc(&mut b);
        assert_eq!(value(&b, "C1"), Value::Number(240.0));
        assert_eq!(value(&b, "C2"), Value::Number(80.0));
        assert_eq!(value(&b, "C3"), Value::Number(160.0));
        assert_eq!(value(&b, "C4"), Value::Text("big!".into()));
        assert_eq!(value(&b, "C5"), Value::Number(17.14));
        assert_eq!(value(&b, "C6"), Value::Error("#DIV/0!".into()));
        assert_eq!(value(&b, "C7"), Value::Number(2.0));
        assert_eq!(value(&b, "C8"), Value::Number(80.0));
        assert_eq!(value(&b, "C9"), Value::Number(4.1));
        assert_eq!(value(&b, "C10"), Value::Text("North".into()));
        assert_eq!(
            value(&b, "D1"),
            Value::Number(7.0),
            "an unknown function keeps the file's value"
        );
        assert_eq!(value(&b, "E1"), Value::Error("#REF!".into()));
    }

    #[test]
    fn formulas_across_sheets() {
        let mut b = book(&[("A1", "5")]);
        let mut s = Sheet::new("My Summary");
        s.cells.insert(
            (0, 0),
            Cell {
                formula: Some("Data!A1*2".into()),
                ..Cell::default()
            },
        );
        b.sheets.push(s);
        b.sheets[0].cells.insert(
            (1, 0),
            Cell {
                formula: Some("'My Summary'!A1+1".into()),
                ..Cell::default()
            },
        );
        recalc(&mut b);
        assert_eq!(b.sheets[1].get(0, 0).unwrap().value, Value::Number(10.0));
        assert_eq!(value(&b, "A2"), Value::Number(11.0));
    }

    #[test]
    fn shifting_and_renaming() {
        assert_eq!(
            shift("B1*$C$1+SUM(A1:A3)+SUM(A:A)", 2, 1),
            "C3*$C$1+SUM(B3:B5)+SUM(B:B)"
        );
        assert_eq!(shift("Sheet2!A1&\"A1\"", 1, 0), "Sheet2!A2&\"A1\"");
        assert_eq!(
            rename_sheet("Data!A1+data!B2", "Data", "Q1 Data"),
            "'Q1 Data'!A1+'Q1 Data'!B2"
        );
    }
}
