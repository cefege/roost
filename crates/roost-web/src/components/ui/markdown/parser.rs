//! Semantic Markdown parsing and destination normalization for transcript content.
//! The parent renderer consumes typed nodes rather than HTML strings.
//! Raw markup remains text; fenced source and supported link schemes stay explicit.

use pulldown_cmark::{Alignment, CodeBlockKind, Event, HeadingLevel, Options, Parser, Tag, TagEnd};

#[derive(Clone, PartialEq)]
pub(super) enum Node {
    Text(String),
    Paragraph(Vec<Node>),
    Heading(u8, Vec<Node>),
    Strong(Vec<Node>),
    Emphasis(Vec<Node>),
    Strike(Vec<Node>),
    Link(String, String, Vec<Node>),
    Image(String, String, Vec<Node>),
    InlineCode(String),
    CodeBlock(String, String),
    Quote(Vec<Node>),
    List(Option<u64>, Vec<Node>),
    Item(Vec<Node>),
    Table(Vec<Alignment>, Vec<Node>),
    TableHead(Vec<Node>),
    TableRow(Vec<Node>),
    TableCell(Vec<Node>),
    SoftBreak,
    Task(bool),
    Break,
    Rule,
}

pub(super) fn parse_markdown(text: &str) -> Vec<Node> {
    let options =
        Options::ENABLE_TABLES | Options::ENABLE_STRIKETHROUGH | Options::ENABLE_TASKLISTS;
    let mut roots = Vec::new();
    let mut stack: Vec<Node> = Vec::new();
    let mut code: Option<(String, String)> = None;
    for event in Parser::new_ext(text, options) {
        match event {
            Event::End(TagEnd::CodeBlock) => {
                if let Some((language, source)) = code.take() {
                    append_node(&mut roots, &mut stack, Node::CodeBlock(language, source));
                }
            }
            Event::Start(Tag::CodeBlock(kind)) => {
                let language = match kind {
                    CodeBlockKind::Fenced(info) => safe_language(&info),
                    CodeBlockKind::Indented => String::new(),
                };
                code = Some((language, String::new()));
            }
            event if code.is_some() => {
                if let Some((_, source)) = code.as_mut() {
                    match event {
                        Event::Text(value)
                        | Event::Code(value)
                        | Event::Html(value)
                        | Event::InlineHtml(value) => source.push_str(&value),
                        Event::SoftBreak | Event::HardBreak => source.push('\n'),
                        _ => {}
                    }
                }
            }
            Event::Start(Tag::Paragraph) => stack.push(Node::Paragraph(Vec::new())),
            Event::End(TagEnd::Paragraph) => close_node(&mut roots, &mut stack),
            Event::Start(Tag::Heading { level, .. }) => {
                stack.push(Node::Heading(heading_level(level), Vec::new()))
            }
            Event::End(TagEnd::Heading(_)) => close_node(&mut roots, &mut stack),
            Event::Start(Tag::Emphasis) => stack.push(Node::Emphasis(Vec::new())),
            Event::End(TagEnd::Emphasis) => close_node(&mut roots, &mut stack),
            Event::Start(Tag::Strong) => stack.push(Node::Strong(Vec::new())),
            Event::End(TagEnd::Strong) => close_node(&mut roots, &mut stack),
            Event::Start(Tag::Strikethrough) => stack.push(Node::Strike(Vec::new())),
            Event::End(TagEnd::Strikethrough) => close_node(&mut roots, &mut stack),
            Event::Start(Tag::Link {
                dest_url, title, ..
            }) => stack.push(Node::Link(
                safe_link(&dest_url),
                title.into_string(),
                Vec::new(),
            )),
            Event::End(TagEnd::Link) => close_node(&mut roots, &mut stack),
            Event::Start(Tag::Image {
                dest_url, title, ..
            }) => stack.push(Node::Image(
                dest_url.into_string(),
                title.into_string(),
                Vec::new(),
            )),
            Event::End(TagEnd::Image) => close_node(&mut roots, &mut stack),
            Event::Start(Tag::BlockQuote(_)) => stack.push(Node::Quote(Vec::new())),
            Event::End(TagEnd::BlockQuote(_)) => close_node(&mut roots, &mut stack),
            Event::Start(Tag::List(start)) => stack.push(Node::List(start, Vec::new())),
            Event::End(TagEnd::List(_)) => close_node(&mut roots, &mut stack),
            Event::Start(Tag::Item) => stack.push(Node::Item(Vec::new())),
            Event::End(TagEnd::Item) => close_node(&mut roots, &mut stack),
            Event::Start(Tag::Table(alignment)) => stack.push(Node::Table(alignment, Vec::new())),
            Event::End(TagEnd::Table) => close_node(&mut roots, &mut stack),
            Event::Start(Tag::TableHead) => stack.push(Node::TableHead(Vec::new())),
            Event::End(TagEnd::TableHead) => close_node(&mut roots, &mut stack),
            Event::Start(Tag::TableRow) => stack.push(Node::TableRow(Vec::new())),
            Event::End(TagEnd::TableRow) => close_node(&mut roots, &mut stack),
            Event::Start(Tag::TableCell) => stack.push(Node::TableCell(Vec::new())),
            Event::End(TagEnd::TableCell) => close_node(&mut roots, &mut stack),
            Event::Text(value) | Event::Html(value) | Event::InlineHtml(value) => {
                append_node(&mut roots, &mut stack, Node::Text(value.into_string()))
            }
            Event::Code(value) => append_node(
                &mut roots,
                &mut stack,
                Node::InlineCode(value.into_string()),
            ),
            Event::SoftBreak => append_node(&mut roots, &mut stack, Node::SoftBreak),
            Event::HardBreak => append_node(&mut roots, &mut stack, Node::Break),
            Event::Rule => append_node(&mut roots, &mut stack, Node::Rule),
            Event::TaskListMarker(checked) => {
                append_node(&mut roots, &mut stack, Node::Task(checked))
            }
            _ => {}
        }
    }
    while !stack.is_empty() {
        close_node(&mut roots, &mut stack);
    }
    roots
}

fn append_node(roots: &mut Vec<Node>, stack: &mut [Node], node: Node) {
    if let Some(parent) = stack.last_mut() {
        if let Some(children) = children_mut(parent) {
            children.push(node);
        } else {
            roots.push(node);
        }
    } else {
        roots.push(node);
    }
}

fn close_node(roots: &mut Vec<Node>, stack: &mut Vec<Node>) {
    if let Some(node) = stack.pop() {
        append_node(roots, stack, node);
    }
}

fn children_mut(node: &mut Node) -> Option<&mut Vec<Node>> {
    match node {
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
        | Node::TableCell(children) => Some(children),
        _ => None,
    }
}

fn heading_level(level: HeadingLevel) -> u8 {
    match level {
        HeadingLevel::H1 => 1,
        HeadingLevel::H2 => 2,
        HeadingLevel::H3 => 3,
        HeadingLevel::H4 => 4,
        HeadingLevel::H5 => 5,
        HeadingLevel::H6 => 6,
    }
}

fn safe_link(url: &str) -> String {
    let lower = url.trim_start().to_ascii_lowercase();
    if lower.starts_with("http://") || lower.starts_with("https://") || lower.starts_with("mailto:")
    {
        url.to_owned()
    } else {
        String::new()
    }
}

fn safe_language(info: &str) -> String {
    info.split_whitespace()
        .next()
        .unwrap_or_default()
        .chars()
        .filter(|character| character.is_ascii_alphanumeric() || "+#.-_".contains(*character))
        .take(24)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::{Node, parse_markdown, safe_link};

    #[test]
    fn raw_html_is_preserved_as_text_nodes() {
        let nodes = parse_markdown("<script>alert(1)</script>");
        let text = nodes
            .iter()
            .filter_map(|node| match node {
                Node::Text(text) => Some(text.as_str()),
                _ => None,
            })
            .collect::<String>();
        assert_eq!(text, "<script>alert(1)</script>");
    }

    #[test]
    fn unsafe_link_schemes_are_blank_and_web_links_survive() {
        assert_eq!(safe_link("javascript:alert(1)"), "");
        assert_eq!(safe_link("data:text/html,x"), "");
        assert_eq!(
            safe_link("https://example.test/path"),
            "https://example.test/path"
        );
        assert_eq!(
            safe_link("mailto:reader@example.test"),
            "mailto:reader@example.test"
        );
        assert!(
            matches!(parse_markdown("[x](javascript:alert(1))").as_slice(),
            [Node::Paragraph(children)] if matches!(children.as_slice(), [Node::Link(href, _, _)] if href.is_empty()))
        );
    }

    #[test]
    fn fenced_code_preserves_its_language_and_source() {
        assert!(
            matches!(parse_markdown("```rust\nlet x = 1;\n```").as_slice(),
            [Node::CodeBlock(language, source)] if language == "rust" && source == "let x = 1;\n")
        );
    }

    #[test]
    fn images_keep_their_destination_title_and_formatted_alt_text() {
        let nodes = parse_markdown("![an *image*](https://example.test/a.png \"Caption\")");
        assert!(matches!(nodes.as_slice(), [Node::Paragraph(children)]
            if matches!(children.as_slice(), [Node::Image(source, title, alt)]
                if source == "https://example.test/a.png" && title == "Caption"
                    && matches!(alt.as_slice(), [Node::Text(first), Node::Emphasis(_)] if first == "an "))));
    }

    #[test]
    fn ordered_starts_column_alignment_and_task_state_survive_parsing() {
        assert!(matches!(
            parse_markdown("3. third\n4. fourth").as_slice(),
            [Node::List(Some(3), _)]
        ));
        assert!(
            matches!(parse_markdown("| left | right |\n| :--- | ---: |\n| a | b |").as_slice(),
            [Node::Table(alignment, _)] if alignment == &[pulldown_cmark::Alignment::Left, pulldown_cmark::Alignment::Right])
        );
        assert!(
            matches!(parse_markdown("- [x] complete").as_slice(), [Node::List(None, items)]
            if matches!(items.as_slice(), [Node::Item(children)] if matches!(children.first(), Some(Node::Task(true)))))
        );
    }

    #[test]
    fn soft_wrapping_does_not_become_an_explicit_line_break() {
        assert!(
            matches!(parse_markdown("soft\nwrap  \nhard").as_slice(), [Node::Paragraph(children)]
            if matches!(children.as_slice(), [Node::Text(_), Node::SoftBreak, Node::Text(_), Node::Break, Node::Text(_)]))
        );
    }
}
