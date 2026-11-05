//! The TeX subset parser behind native `math` nodes. It never panics on
//! malformed input and caps both source length and recursion depth.

use super::symbols::symbol_command;
use super::{Expr, MAX_DEPTH, MAX_SOURCE_CHARS};

pub(super) struct Parser {
    chars: Vec<char>,
    at: usize,
    depth: usize,
}

impl Parser {
    pub(super) fn new(source: &str) -> Self {
        Self {
            chars: source.chars().take(MAX_SOURCE_CHARS).collect(),
            at: 0,
            depth: 0,
        }
    }

    pub(super) fn parse_row(&mut self, stop: Option<char>) -> Expr {
        let mut row = Vec::new();
        loop {
            self.skip_spaces();
            if self.at >= self.chars.len() || stop.is_some_and(|stop| self.peek() == Some(stop)) {
                break;
            }
            let before = self.at;
            row.push(self.parse_scripted());
            if self.at == before {
                self.at += 1;
            }
        }
        Expr::Row(row)
    }

    fn parse_scripted(&mut self) -> Expr {
        let base = self.parse_atom();
        let mut sub = None;
        let mut sup = None;
        loop {
            self.skip_spaces();
            match self.peek() {
                Some('_') => {
                    self.at += 1;
                    sub = Some(Box::new(self.parse_arg()));
                }
                Some('^') => {
                    self.at += 1;
                    sup = Some(Box::new(self.parse_arg()));
                }
                Some('\'') => {
                    self.at += 1;
                    sup = Some(Box::new(Expr::Op("′".into())));
                }
                _ => break,
            }
        }
        match (sub, sup) {
            (Some(sub), Some(sup)) => Expr::SubSup(Box::new(base), sub, sup),
            (Some(sub), None) => Expr::Sub(Box::new(base), sub),
            (None, Some(sup)) => Expr::Sup(Box::new(base), sup),
            (None, None) => base,
        }
    }

    fn parse_arg(&mut self) -> Expr {
        self.skip_spaces();
        if self.peek() == Some('{') {
            self.group()
        } else if self.at < self.chars.len() {
            self.parse_atom()
        } else {
            Expr::Row(Vec::new())
        }
    }

    fn group(&mut self) -> Expr {
        if self.depth >= MAX_DEPTH {
            self.skip_raw_group();
            return Expr::Text("…".into());
        }
        self.depth += 1;
        self.at += 1;
        let row = self.parse_row(Some('}'));
        if self.peek() == Some('}') {
            self.at += 1;
        }
        self.depth -= 1;
        row
    }

    fn parse_atom(&mut self) -> Expr {
        self.skip_spaces();
        let Some(character) = self.peek() else {
            return Expr::Row(Vec::new());
        };
        match character {
            '{' => self.group(),
            '\\' => self.command(),
            '0'..='9' => {
                let mut number = String::new();
                while let Some(character) =
                    self.peek().filter(|character| character.is_ascii_digit())
                {
                    number.push(character);
                    self.at += 1;
                }
                Expr::Number(number)
            }
            character if character.is_alphabetic() => {
                self.at += 1;
                Expr::Ident(character.to_string())
            }
            '+' | '-' | '=' | '<' | '>' | '(' | ')' | '[' | ']' | '|' | ',' | ';' | ':' | '/'
            | '!' | '?' | '.' | '*' => {
                self.at += 1;
                Expr::Op(match character {
                    '-' => "−".into(),
                    '*' => "∗".into(),
                    other => other.to_string(),
                })
            }
            '~' => {
                self.at += 1;
                Expr::Space("0.25em")
            }
            other => {
                self.at += 1;
                if other.is_control() {
                    Expr::Text(String::new())
                } else {
                    Expr::Text(other.to_string())
                }
            }
        }
    }

    fn command(&mut self) -> Expr {
        if self.depth >= MAX_DEPTH {
            self.at = self.at.saturating_add(1);
            return Expr::Text("…".into());
        }
        self.depth += 1;
        let expression = self.parse_command();
        self.depth -= 1;
        expression
    }

    fn parse_command(&mut self) -> Expr {
        self.at += 1;
        let Some(character) = self.peek() else {
            return Expr::Row(Vec::new());
        };
        if !character.is_alphabetic() {
            self.at += 1;
            return match character {
                ',' | ':' => Expr::Space("0.17em"),
                ';' => Expr::Space("0.28em"),
                '!' => Expr::Space("-0.17em"),
                ' ' => Expr::Space("0.25em"),
                '\\' => Expr::Space("0.5em"),
                '{' | '}' | '%' | '$' | '&' | '#' | '_' => Expr::Text(character.to_string()),
                other => Expr::Text(other.to_string()),
            };
        }
        let mut name = String::new();
        while let Some(character) = self.peek().filter(|character| character.is_alphabetic()) {
            name.push(character);
            self.at += 1;
        }
        match name.as_str() {
            "frac" | "dfrac" | "tfrac" => {
                let numerator = self.parse_arg();
                let denominator = self.parse_arg();
                Expr::Frac(Box::new(numerator), Box::new(denominator))
            }
            "binom" => {
                let top = self.parse_arg();
                let bottom = self.parse_arg();
                Expr::Binom(Box::new(top), Box::new(bottom))
            }
            "sqrt" => {
                self.skip_spaces();
                let index = if self.peek() == Some('[') {
                    Some(Box::new(self.bracket_group()))
                } else {
                    None
                };
                Expr::Sqrt(index, Box::new(self.parse_arg()))
            }
            "text" | "textrm" | "mbox" | "operatorname" => Expr::Text(self.raw_group_text()),
            "mathrm" => self.styled("normal"),
            "mathbf" | "boldsymbol" | "bm" => self.styled("bold"),
            "mathit" => self.styled("italic"),
            "mathbb" => self.styled("double-struck"),
            "mathcal" => self.styled("script"),
            "mathsf" => self.styled("sans-serif"),
            "mathtt" => self.styled("monospace"),
            "textcolor" => {
                let _ = self.raw_group_text();
                self.parse_arg()
            }
            "color" => {
                let _ = self.raw_group_text();
                Expr::Row(Vec::new())
            }
            "colorbox" => {
                let _ = self.raw_group_text();
                self.parse_arg()
            }
            "fcolorbox" => {
                let _ = self.raw_group_text();
                let _ = self.raw_group_text();
                self.parse_arg()
            }
            "overline" | "bar" => {
                Expr::Over(Box::new(Expr::Op("¯".into())), Box::new(self.parse_arg()))
            }
            "hat" => Expr::Over(Box::new(Expr::Op("^".into())), Box::new(self.parse_arg())),
            "vec" => Expr::Over(Box::new(Expr::Op("→".into())), Box::new(self.parse_arg())),
            "tilde" => Expr::Over(Box::new(Expr::Op("~".into())), Box::new(self.parse_arg())),
            "dot" => Expr::Over(Box::new(Expr::Op("˙".into())), Box::new(self.parse_arg())),
            "ddot" => Expr::Over(Box::new(Expr::Op("¨".into())), Box::new(self.parse_arg())),
            "left" => Expr::Fence(self.delimiter()),
            "right" => Expr::Fence(self.delimiter()),
            "begin" => self.environment(),
            "quad" => Expr::Space("1em"),
            "qquad" => Expr::Space("2em"),
            "displaystyle" | "textstyle" | "scriptstyle" | "limits" | "nolimits" => {
                Expr::Row(Vec::new())
            }
            "sin" | "cos" | "tan" | "cot" | "sec" | "csc" | "log" | "ln" | "exp" | "lim"
            | "max" | "min" | "sup" | "inf" | "det" | "gcd" => {
                Expr::Styled("normal", Box::new(Expr::Ident(name)))
            }
            other => symbol_command(other).map_or_else(
                || Expr::Text(other.to_owned()),
                |symbol| {
                    if symbol.len() > 2 {
                        Expr::Op(symbol.into())
                    } else {
                        Expr::Ident(symbol.into())
                    }
                },
            ),
        }
    }

    fn styled(&mut self, variant: &'static str) -> Expr {
        Expr::Styled(variant, Box::new(self.parse_arg()))
    }

    fn bracket_group(&mut self) -> Expr {
        self.at += 1;
        let row = self.parse_row(Some(']'));
        if self.peek() == Some(']') {
            self.at += 1;
        }
        row
    }

    fn delimiter(&mut self) -> String {
        self.skip_spaces();
        if self.peek() == Some('\\') {
            let command = self.command();
            return match command {
                Expr::Ident(text) | Expr::Op(text) | Expr::Text(text) => text,
                _ => String::new(),
            };
        }
        let delimiter = self.peek().map(String::from).unwrap_or_default();
        if !delimiter.is_empty() {
            self.at += 1;
        }
        match delimiter.as_str() {
            "." => String::new(),
            other => other.to_owned(),
        }
    }

    fn environment(&mut self) -> Expr {
        let name = self.raw_group_text();
        let Some(name) = name.split_whitespace().next().map(str::to_owned) else {
            return Expr::Row(Vec::new());
        };
        let end = format!("\\end{{{name}}}");
        let closing = self.find_marker(&end);
        let body_end = closing.unwrap_or(self.chars.len());
        let body: String = self.chars[self.at..body_end].iter().collect();
        self.at = closing.map_or(self.chars.len(), |index| index + end.chars().count());
        let rows = body
            .split("\\\\")
            .map(|row| {
                row.split('&')
                    .map(|cell| {
                        let mut parser = Parser::new(cell);
                        parser.parse_row(None)
                    })
                    .collect()
            })
            .collect();
        let table = Expr::Table(rows);
        let (open, close) = match name.as_str() {
            "pmatrix" => ("(", ")"),
            "bmatrix" => ("[", "]"),
            "Bmatrix" => ("{", "}"),
            "vmatrix" => ("|", "|"),
            "Vmatrix" => ("‖", "‖"),
            "cases" => ("{", ""),
            _ => ("", ""),
        };
        if open.is_empty() && close.is_empty() {
            table
        } else {
            Expr::Row(vec![
                Expr::Fence(open.into()),
                table,
                Expr::Fence(close.into()),
            ])
        }
    }

    fn find_marker(&self, marker: &str) -> Option<usize> {
        let marker: Vec<char> = marker.chars().collect();
        (self.at..=self.chars.len().saturating_sub(marker.len()))
            .find(|start| self.chars[*start..].starts_with(&marker))
    }

    fn raw_group_text(&mut self) -> String {
        self.skip_spaces();
        if self.peek() != Some('{') {
            return self.peek().map(String::from).unwrap_or_default();
        }
        self.at += 1;
        let mut depth = 1;
        let mut text = String::new();
        while let Some(character) = self.peek() {
            self.at += 1;
            match character {
                '\\' => {
                    if let Some(next) = self.peek() {
                        text.push(next);
                        self.at += 1;
                    }
                }
                '{' => {
                    depth += 1;
                    text.push(character);
                }
                '}' => {
                    depth -= 1;
                    if depth == 0 {
                        break;
                    }
                    text.push(character);
                }
                other => text.push(other),
            }
        }
        text.replace(['\n', '\r', '\t'], " ")
    }

    fn skip_raw_group(&mut self) {
        if self.peek() == Some('{') {
            let _ = self.raw_group_text();
        } else if self.at < self.chars.len() {
            self.at += 1;
        }
    }

    fn skip_spaces(&mut self) {
        while self
            .peek()
            .is_some_and(|character| character.is_whitespace())
        {
            self.at += 1;
        }
    }

    fn peek(&self) -> Option<char> {
        self.chars.get(self.at).copied()
    }
}
