//! Native `math` nodes typeset TeX as MathML. The parser covers the notation
//! OMP emits: fractions, scripts, roots, text, styled letters, accents,
//! binomials and matrix environments. Input is never echoed as TeX, and all
//! text is escaped while only the fixed MathML tags below reach the DOM.

use super::super::prop_text;
use dioxus::prelude::*;
use roost_protocol::wire::agent_chat::semantic_ui::OmpUiNode;
use serde_json::Value;

mod parse;
mod symbols;

const MAX_SOURCE_CHARS: usize = 8_000;
const MAX_DEPTH: usize = 48;

pub(super) fn render(node: &OmpUiNode) -> Element {
    let source = prop_text(node, "text");
    let display = node
        .p
        .get("display")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let markup = typeset(&source, display);
    if display {
        rsx! {
            div {
                class: "min-w-0 max-w-full overflow-x-auto rounded-md bg-muted p-3 text-center text-sm text-foreground",
                dangerous_inner_html: markup,
            }
        }
    } else {
        rsx! {
            span {
                class: "inline-block max-w-full align-middle text-sm",
                dangerous_inner_html: markup,
            }
        }
    }
}

#[derive(Clone, PartialEq)]
enum Expr {
    Row(Vec<Expr>),
    Ident(String),
    Number(String),
    Op(String),
    Frac(Box<Expr>, Box<Expr>),
    Binom(Box<Expr>, Box<Expr>),
    Sqrt(Option<Box<Expr>>, Box<Expr>),
    Sup(Box<Expr>, Box<Expr>),
    Sub(Box<Expr>, Box<Expr>),
    SubSup(Box<Expr>, Box<Expr>, Box<Expr>),
    Over(Box<Expr>, Box<Expr>),
    Styled(&'static str, Box<Expr>),
    Text(String),
    Space(&'static str),
    Table(Vec<Vec<Expr>>),
    Fence(String),
}

fn typeset(source: &str, display: bool) -> String {
    let mut parser = parse::Parser::new(source);
    let expression = parser.parse_row(None);
    let mut out = String::new();
    out.push_str("<math xmlns=\"http://www.w3.org/1998/Math/MathML\" display=\"");
    out.push_str(if display { "block" } else { "inline" });
    out.push_str("\">");
    write_expr(&mut out, &expression);
    out.push_str("</math>");
    out
}

fn write_expr(out: &mut String, expression: &Expr) {
    match expression {
        Expr::Row(rows) => {
            out.push_str("<mrow>");
            for row in rows {
                write_expr(out, row);
            }
            out.push_str("</mrow>");
        }
        Expr::Ident(text) => write_tag(out, "mi", text),
        Expr::Number(text) => write_tag(out, "mn", text),
        Expr::Op(text) => {
            let large = matches!(text.as_str(), "∑" | "∏" | "∫" | "∮");
            if large {
                out.push_str("<mo largeop=\"true\">");
            } else {
                out.push_str("<mo>");
            }
            out.push_str(&escape_math(text));
            out.push_str("</mo>");
        }
        Expr::Frac(top, bottom) => {
            out.push_str("<mfrac>");
            write_expr(out, top);
            write_expr(out, bottom);
            out.push_str("</mfrac>");
        }
        Expr::Binom(top, bottom) => {
            out.push_str("<mrow><mo>(</mo><mfrac linethickness=\"0\">");
            write_expr(out, top);
            write_expr(out, bottom);
            out.push_str("</mfrac><mo>)</mo></mrow>");
        }
        Expr::Sqrt(index, body) => {
            if let Some(index) = index {
                out.push_str("<mroot>");
                write_expr(out, body);
                write_expr(out, index);
                out.push_str("</mroot>");
            } else {
                out.push_str("<msqrt>");
                write_expr(out, body);
                out.push_str("</msqrt>");
            }
        }
        Expr::Sup(base, sup) => script(out, "msup", base, Some(sup), None),
        Expr::Sub(base, sub) => script(out, "msub", base, None, Some(sub)),
        Expr::SubSup(base, sub, sup) => script(out, "msubsup", base, Some(sup), Some(sub)),
        Expr::Over(accent, body) => {
            out.push_str("<mover accent=\"true\">");
            write_expr(out, body);
            write_expr(out, accent);
            out.push_str("</mover>");
        }
        Expr::Styled(variant, body) => {
            out.push_str("<mstyle mathvariant=\"");
            out.push_str(variant);
            out.push_str("\">");
            write_expr(out, body);
            out.push_str("</mstyle>");
        }
        Expr::Text(text) => write_tag(out, "mtext", &text.replace(['\n', '\r', '\t'], " ")),
        Expr::Space(width) => {
            out.push_str("<mspace width=\"");
            out.push_str(width);
            out.push_str("\"/>");
        }
        Expr::Table(rows) => {
            out.push_str("<mtable>");
            for row in rows {
                out.push_str("<mtr>");
                for cell in row {
                    out.push_str("<mtd>");
                    write_expr(out, cell);
                    out.push_str("</mtd>");
                }
                out.push_str("</mtr>");
            }
            out.push_str("</mtable>");
        }
        Expr::Fence(text) => {
            out.push_str("<mo fence=\"true\" stretchy=\"true\">");
            out.push_str(&escape_math(text));
            out.push_str("</mo>");
        }
    }
}

fn script(out: &mut String, tag: &str, base: &Expr, sup: Option<&Expr>, sub: Option<&Expr>) {
    out.push('<');
    out.push_str(tag);
    out.push('>');
    write_expr(out, base);
    if tag == "msubsup" {
        write_expr(out, sub.unwrap_or(&Expr::Row(Vec::new())));
        write_expr(out, sup.unwrap_or(&Expr::Row(Vec::new())));
    } else if let Some(sup) = sup {
        write_expr(out, sup);
    } else if let Some(sub) = sub {
        write_expr(out, sub);
    }
    out.push_str("</");
    out.push_str(tag);
    out.push('>');
}

fn write_tag(out: &mut String, tag: &str, text: &str) {
    out.push('<');
    out.push_str(tag);
    out.push('>');
    out.push_str(&escape_math(text));
    out.push_str("</");
    out.push_str(tag);
    out.push('>');
}

fn escape_math(text: &str) -> String {
    let mut escaped = String::with_capacity(text.len());
    for character in text.chars() {
        match character {
            '&' => escaped.push_str("&amp;"),
            '<' => escaped.push_str("&lt;"),
            '>' => escaped.push_str("&gt;"),
            '"' => escaped.push_str("&quot;"),
            '\'' => escaped.push_str("&#39;"),
            other => escaped.push(other),
        }
    }
    escaped
}

#[cfg(test)]
mod tests {
    use super::typeset;

    #[test]
    fn fractions_and_scripts_become_mathml() {
        let markup = typeset(r"\frac{a+b}{c_2}", false);
        assert!(markup.contains("<mfrac>"), "{markup}");
        assert!(markup.contains("<msub>"), "{markup}");
        assert!(!markup.contains("\\frac"), "{markup}");
        assert!(markup.contains("display=\"inline\""));
    }

    #[test]
    fn text_is_escaped_and_never_opens_markup() {
        let markup = typeset(r"\text{<script>alert(1)</script>}", true);
        assert!(!markup.contains("<script>"), "{markup}");
        assert!(markup.contains("&lt;script&gt;"), "{markup}");
        assert!(markup.contains("display=\"block\""));
    }

    #[test]
    fn unknown_commands_do_not_leak_tex_or_panic() {
        let markup = typeset(
            r"\unknown{x}\left(\begin{pmatrix}1&2\\3&4\end{pmatrix}\right)",
            false,
        );
        assert!(!markup.contains('\\'), "{markup}");
        assert!(markup.contains("<mtable>"), "{markup}");
    }

    #[test]
    fn unmatched_groups_and_deep_input_stay_bounded() {
        let markup = typeset(&format!("{}{{", "\\frac{1}{2}".repeat(2_000)), false);
        assert!(markup.starts_with("<math"));
        assert!(!markup.contains('\\'));
    }
}
