//! The drawings an OMP `md` node carries beside its source: the terminal's
//! ASCII rendering of each closed mermaid fence and the themed document of each
//! closed svg fence. `markdown.rs` swaps a matching fence for them; a fence
//! without an entry (still streaming, or a client that sent none) stays code.

use super::markdown::escape_html;
use serde_json::Value;

/// Drawings keyed by the trimmed fence source they were made from.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct FenceFigures {
    mermaid: Vec<(String, String)>,
    svg: Vec<(String, String)>,
}

impl FenceFigures {
    /// Reads an `md` node's `mermaid` (`[{src, ascii}]`) and `svg` (`[{src, doc}]`) props.
    pub fn from_props(mermaid: Option<&Value>, svg: Option<&Value>) -> Self {
        Self {
            mermaid: entries(mermaid, "ascii"),
            svg: entries(svg, "doc"),
        }
    }

    pub(super) fn is_empty(&self) -> bool {
        self.mermaid.is_empty() && self.svg.is_empty()
    }

    /// The drawn text of a closed mermaid fence with this source.
    pub(super) fn mermaid_art(&self, fence_source: &str) -> Option<&str> {
        lookup(&self.mermaid, fence_source)
    }

    /// The `<img>` markup of a closed svg fence with this source.
    pub(super) fn svg_image(&self, fence_source: &str) -> Option<String> {
        let document = lookup(&self.svg, fence_source)?;
        let alt = root_label(document).map_or_else(|| "SVG figure".to_owned(), escape_html);
        Some(format!(
            "<div data-svg-figure class=\"my-2 overflow-x-auto\">\
             <img alt=\"{alt}\" src=\"data:image/svg+xml;charset=utf-8,{}\"></div>",
            percent_encode(document)
        ))
    }
}

fn entries(value: Option<&Value>, drawing_key: &str) -> Vec<(String, String)> {
    value
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(|item| {
                    let source = item.get("src")?.as_str()?;
                    let drawing = item.get(drawing_key)?.as_str()?;
                    Some((source.trim().to_owned(), drawing.to_owned()))
                })
                .collect()
        })
        .unwrap_or_default()
}

fn lookup<'a>(entries: &'a [(String, String)], fence_source: &str) -> Option<&'a str> {
    let wanted = fence_source.trim();
    entries
        .iter()
        .find(|(source, _)| source == wanted)
        .map(|(_, drawing)| drawing.as_str())
}

/// The root element's `aria-label`, the figure's own description for the image's alt text.
fn root_label(document: &str) -> Option<&str> {
    let root = document.get(document.find("<svg")?..)?;
    let mut quote = None;
    let mut tag_end = root.len();
    for (index, character) in root.char_indices() {
        match (quote, character) {
            (None, '"' | '\'') => quote = Some(character),
            (Some(open), _) if open == character => quote = None,
            (None, '>') => {
                tag_end = index;
                break;
            }
            _ => {}
        }
    }
    let tag = root.get(..tag_end)?;
    let label = tag.get(tag.find("aria-label=\"")? + "aria-label=\"".len()..)?;
    label
        .get(..label.find('"')?)
        .filter(|text| !text.is_empty())
}

/// Everything but RFC 3986 unreserved bytes, so the document cannot end the attribute or the URL.
fn percent_encode(text: &str) -> String {
    let mut encoded = String::with_capacity(text.len() * 2);
    for byte in text.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~') {
            encoded.push(char::from(byte));
        } else {
            encoded.push_str(&format!("%{byte:02X}"));
        }
    }
    encoded
}

#[cfg(test)]
mod tests {
    use super::FenceFigures;
    use serde_json::{Value, json};

    fn from_json(props: &Value) -> FenceFigures {
        FenceFigures::from_props(props.get("mermaid"), props.get("svg"))
    }

    #[test]
    fn entries_match_by_trimmed_source_and_ignore_malformed_items() {
        let figures = from_json(&json!({
            "mermaid": [{"src": "flowchart LR\nA-->B", "ascii": "[A]-->[B]"}, {"src": 3}],
            "svg": [{"src": "<svg/>", "doc": "<svg xmlns=\"x\"/>"}],
        }));
        assert_eq!(
            figures.mermaid_art("flowchart LR\nA-->B\n"),
            Some("[A]-->[B]")
        );
        assert_eq!(figures.mermaid_art("graph TD"), None);
        assert!(!figures.is_empty());
        assert!(from_json(&json!({})).is_empty());
    }

    #[test]
    fn an_svg_document_is_percent_encoded_into_a_data_image() {
        let figures = from_json(&json!({
            "svg": [{"src": "<svg/>", "doc": "<svg a=\"b\" c='d'>#</svg>"}],
        }));
        let image = figures.svg_image("<svg/>\n").unwrap_or_default();
        assert!(image.contains("data:image/svg+xml;charset=utf-8,%3Csvg%20a%3D%22b%22"));
        assert!(!image.contains("<svg"), "{image}");
        assert!(!image.contains("'"), "{image}");
        assert!(image.contains("%23"), "{image}");
        assert!(image.contains("alt=\"SVG figure\""), "{image}");
    }

    #[test]
    fn the_roots_aria_label_describes_the_image() {
        let figures = from_json(&json!({
            "svg": [{"src": "<svg/>", "doc": "<svg aria-label=\"Latency <p95> & p99\" width=\"4\"><g aria-label=\"inner\"/></svg>"}],
        }));
        let image = figures.svg_image("<svg/>").unwrap_or_default();
        assert!(
            image.contains("alt=\"Latency &lt;p95&gt; &amp; p99\""),
            "{image}"
        );
    }
}
