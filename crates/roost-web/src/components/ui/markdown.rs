//! Typed Markdown rendering for untrusted transcript text.
//! Pulldown-cmark events become a small tree; raw HTML stays text and unsafe
//! link destinations are blanked before rendering.
mod parser;

use parser::{Node, parse_markdown};

use dioxus::prelude::*;
use libero::{
    components::{ActionIcon, Blockquote, Code, Flex, Paper, Text, Title},
    sx::sx,
};
use pulldown_cmark::Alignment;

use crate::components::hooks::use_copy_clipboard::use_copy_clipboard;

#[component]
pub fn Markdown(
    text: String,
    #[props(default)] sx: libero::sx::Sx,
    #[props(extends = GlobalAttributes)] attributes: Vec<Attribute>,
) -> Element {
    let nodes = parse_markdown(&text);
    let root_sx = use_hook(|| {
        libero::sx::sx()
            .width("100%")
            .gap("sm")
            .font_size("sm")
            .line_height("1.6")
            .selector("& p", libero::sx::sx().margin("0").line_height("1.6"))
            .selector(
                "& a",
                libero::sx::sx()
                    .color("primary")
                    .text_decoration("underline")
                    .with("text-underline-offset", "0.15em"),
            )
            .selector(
                "& ol, & ul",
                libero::sx::sx()
                    .margin_top("xs")
                    .margin_bottom("xs")
                    .padding_left("md"),
            )
            .selector("& ol", libero::sx::sx().with("list-style-type", "decimal"))
            .selector("& ul", libero::sx::sx().with("list-style-type", "disc"))
            .selector(
                "& hr",
                libero::sx::sx()
                    .width("100%")
                    .border("0")
                    .border_top("1px solid")
                    .border_color("muted.4"),
            )
            .selector(
                "& table",
                libero::sx::sx().width("100%").border_collapse("collapse"),
            )
            .selector(
                "& th, & td",
                libero::sx::sx()
                    .padding("sm")
                    .text_align("start")
                    .border_bottom("1px solid")
                    .border_color("muted.4"),
            )
            .selector("& [data-align='left']", libero::sx::sx().text_align("left"))
            .selector(
                "& [data-align='center']",
                libero::sx::sx().text_align("center"),
            )
            .selector(
                "& [data-align='right']",
                libero::sx::sx().text_align("right"),
            )
            .selector("& img", libero::sx::sx().max_width("100%").height("auto"))
    });
    let root_sx = root_sx.and(sx);
    rsx! {
        Flex {
            direction: "column",
            sx: root_sx,
            "data-name": "Markdown",
            attributes,
            {render_nodes(nodes)}
        }
    }
}

fn render_nodes(nodes: Vec<Node>) -> Element {
    rsx! { for node in nodes { {render_node(node)} } }
}

fn render_node(node: Node) -> Element {
    match node {
        Node::Text(value) => rsx! { {value} },
        Node::Paragraph(children) => {
            rsx! { Text { size: "sm", component: "p", {render_nodes(children)} } }
        }
        Node::Heading(level, children) => {
            rsx! { Title { component: heading_tag(level), size: heading_size(level), {render_nodes(children)} } }
        }
        Node::Strong(children) => rsx! { strong { {render_nodes(children)} } },
        Node::Emphasis(children) => rsx! { em { {render_nodes(children)} } },
        Node::Strike(children) => rsx! { del { {render_nodes(children)} } },
        Node::Link(href, title, children) => rsx! { a { href, title, {render_nodes(children)} } },
        Node::Image(source, title, children) => {
            let mut alt = String::new();
            append_image_alt(&children, &mut alt);
            rsx! { img { src: source, title, alt } }
        }
        Node::InlineCode(source) => rsx! { Code { source } },
        Node::CodeBlock(language, source) => rsx! { MarkdownCodeBlock { language, source } },
        Node::Quote(children) => rsx! { Blockquote { {render_nodes(children)} } },
        Node::List(start, children) => {
            if let Some(start) = start {
                rsx! { ol { start: start.to_string(), {render_nodes(children)} } }
            } else {
                rsx! { ul { {render_nodes(children)} } }
            }
        }
        Node::Item(children) => rsx! { li { {render_nodes(children)} } },
        Node::Table(alignment, children) => render_table(alignment, children),
        Node::TableHead(children) => {
            rsx! { thead { tr { {render_table_cells(children, true, &[])} } } }
        }
        Node::TableRow(children) => rsx! { tr { {render_table_cells(children, false, &[])} } },
        Node::TableCell(children) => rsx! { td { {render_nodes(children)} } },
        Node::SoftBreak => rsx! { "\n" },
        Node::Task(checked) => rsx! { input { r#type: "checkbox", disabled: true, checked } },
        Node::Break => rsx! { br {} },
        Node::Rule => rsx! { hr {} },
    }
}

fn render_table(alignment: Vec<Alignment>, children: Vec<Node>) -> Element {
    let mut header = None;
    let mut rows = Vec::new();
    for child in children {
        match child {
            Node::TableHead(cells) => header = Some(cells),
            Node::TableRow(cells) => rows.push(cells),
            _ => {}
        }
    }
    rsx! {
        table {
            if let Some(cells) = header {
                thead { tr { {render_table_cells(cells, true, &alignment)} } }
            }
            tbody {
                for cells in rows {
                    tr { {render_table_cells(cells, false, &alignment)} }
                }
            }
        }
    }
}

fn render_table_cells(cells: Vec<Node>, header: bool, alignment: &[Alignment]) -> Element {
    rsx! {
        for (index, cell) in cells.into_iter().enumerate() {
            if let Node::TableCell(children) = cell {
                {
                    let align = match alignment.get(index) {
                        Some(Alignment::Left) => "left",
                        Some(Alignment::Center) => "center",
                        Some(Alignment::Right) => "right",
                        _ => "start",
                    };
                    rsx! {
                        if header { th { scope: "col", "data-align": align, {render_nodes(children)} } }
                        else { td { "data-align": align, {render_nodes(children)} } }
                    }
                }
            }
        }
    }
}

fn append_image_alt(nodes: &[Node], alt: &mut String) {
    for node in nodes {
        match node {
            Node::Text(text) | Node::InlineCode(text) | Node::CodeBlock(_, text) => {
                alt.push_str(text)
            }
            Node::Paragraph(children)
            | Node::Heading(_, children)
            | Node::Strong(children)
            | Node::Emphasis(children)
            | Node::Strike(children)
            | Node::Link(_, _, children)
            | Node::Image(_, _, children)
            | Node::Quote(children)
            | Node::List(_, children)
            | Node::Item(children)
            | Node::Table(_, children)
            | Node::TableHead(children)
            | Node::TableRow(children)
            | Node::TableCell(children) => append_image_alt(children, alt),
            Node::SoftBreak | Node::Break => alt.push(' '),
            Node::Task(_) | Node::Rule => {}
        }
    }
}

fn heading_tag(level: u8) -> &'static str {
    match level {
        1 => "h1",
        2 => "h2",
        3 => "h3",
        4 => "h4",
        5 => "h5",
        _ => "h6",
    }
}

fn heading_size(level: u8) -> &'static str {
    match level {
        1 => "xl",
        2 => "lg",
        3 => "md",
        _ => "sm",
    }
}

#[component]
fn MarkdownCodeBlock(language: String, source: String) -> Element {
    let (copy_to_clipboard, copied) = use_copy_clipboard(None);
    let mut copied_source = use_signal(|| None::<String>);
    let block_sx = use_hook(|| {
        sx().overflow("hidden")
            .padding("0")
            .background("var(--term-bg)")
            .color("var(--term-fg)")
    });
    let header_sx = use_hook(|| {
        sx().padding_left("sm")
            .padding_right("sm")
            .padding_top("xs")
    });
    let language_sx = use_hook(|| sx().font_family("monospace"));
    let source_sx = use_hook(|| {
        sx().overflow_x("auto")
            .with("overscroll-behavior-y", "auto")
            .padding("sm")
            .margin("0")
            .font_size("xs")
            .line_height("1.6")
            .font_family("monospace")
            .selector(
                "& [data-token='keyword']",
                sx().color("var(--syntax-keyword)"),
            )
            .selector(
                "& [data-token='string']",
                sx().color("var(--syntax-string)"),
            )
            .selector(
                "& [data-token='number']",
                sx().color("var(--syntax-number)"),
            )
            .selector(
                "& [data-token='comment']",
                sx().color("var(--syntax-comment)").font_style("italic"),
            )
    });
    let copy_confirmed = copied() && copied_source.read().as_deref() == Some(source.as_str());
    let highlight = highlighted_language(&language);
    let content = render_code(&source, highlight);
    rsx! {
        Paper { bordered: true, radius: "md", sx: block_sx,
            Flex { direction: "row", align: "center", justify: "space-between", gap: "sm", sx: header_sx,
                Text { size: "xs", color: "muted", sx: language_sx, if language.is_empty() { "code" } else { "{language}" } }
                ActionIcon {
                    icon: if copy_confirmed { pictogram_icons_lucide::check::outlined } else { pictogram_icons_lucide::copy::outlined },
                    aria_label: if copy_confirmed { "Copied code" } else { "Copy code" },
                    title: if copy_confirmed { "Copied" } else { "Copy" },
                    size: "sm",
                    onclick: move |_| {
                        copied_source.set(Some(source.clone()));
                        copy_to_clipboard(&source);
                    },
                }
            }
            libero::components::Box { component: "pre", sx: source_sx,
                code { {content} }
            }
        }
    }
}

fn highlighted_language(language: &str) -> bool {
    let lower = language.to_ascii_lowercase();
    let extension = match lower.as_str() {
        "rust" => "rs",
        "typescript" => "ts",
        "javascript" => "js",
        "python" => "py",
        "golang" => "go",
        "shell" | "console" => "sh",
        other => other,
    };
    crate::syntax_lite::should_highlight(extension)
}

fn render_code(source: &str, highlighted: bool) -> Element {
    if !highlighted {
        return rsx! { "{source}" };
    }
    let token_lines = crate::syntax_lite::tokenize_lines(source.split('\n'));
    rsx! {
        for (line_idx, tokens) in token_lines.into_iter().enumerate() {
            if line_idx > 0 { "\n" }
            for token in tokens {
                if token.kind == crate::syntax_lite::TokenKind::Plain { {token.text} }
                else { span { "data-token": token_name(token.kind), {token.text} } }
            }
        }
    }
}

fn token_name(kind: crate::syntax_lite::TokenKind) -> &'static str {
    use crate::syntax_lite::TokenKind;
    match kind {
        TokenKind::Keyword => "keyword",
        TokenKind::String => "string",
        TokenKind::Number => "number",
        TokenKind::Comment => "comment",
        TokenKind::Plain => "plain",
    }
}
