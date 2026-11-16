//! The page shell's inline stylesheet stays inside Tailwind's `base` cascade
//! layer. Exercises `crates/roost-web/index.html`.
//!
//! An unlayered rule beats every layered rule regardless of specificity, so an
//! unlayered `* { padding: 0 }` there silently cancels every `p-*`/`m-*`
//! utility in the app, and an unlayered `* { overscroll-behavior-y: none }`
//! every `overscroll-y-auto`.

const INDEX_HTML: &str = include_str!("../index.html");

/// The text between the page's `<style>` and `</style>`.
fn inline_stylesheet(html: &str) -> &str {
    html.split_once("<style>")
        .and_then(|(_, rest)| rest.split_once("</style>"))
        .map_or("", |(sheet, _)| sheet)
}

/// `css` with every `/* … */` comment removed.
fn without_comments(css: &str) -> String {
    let mut uncommented = String::with_capacity(css.len());
    let mut rest = css;
    while let Some(start) = rest.find("/*") {
        uncommented.push_str(&rest[..start]);
        rest = rest[start..]
            .split_once("*/")
            .map_or("", |(_, after)| after);
    }
    uncommented.push_str(rest);
    uncommented
}

/// Every top-level block of `css` with comments removed: the text before each
/// `{` that opens at depth zero.
fn top_level_preludes(css: &str) -> Vec<String> {
    let uncommented = without_comments(css);
    let mut preludes = Vec::new();
    let mut depth = 0_usize;
    let mut prelude = String::new();
    for character in uncommented.chars() {
        match character {
            '{' => {
                if depth == 0 {
                    preludes.push(prelude.trim().to_string());
                }
                depth += 1;
            }
            '}' => {
                depth = depth.saturating_sub(1);
                prelude.clear();
            }
            other if depth == 0 => prelude.push(other),
            _ => {}
        }
    }
    preludes
}

#[test]
fn every_inline_page_rule_sits_in_the_base_layer() {
    let sheet = inline_stylesheet(INDEX_HTML);
    assert!(!sheet.trim().is_empty(), "index.html has no inline <style>");
    assert_eq!(
        top_level_preludes(sheet),
        vec!["@layer base".to_string()],
        "every rule in index.html's <style> must be inside `@layer base`"
    );
}

#[test]
fn an_unlayered_rule_is_reported() {
    let preludes = top_level_preludes("@layer base { * { margin: 0 } } /* x { */ * { padding: 0 }");
    assert_eq!(preludes, vec!["@layer base".to_string(), "*".to_string()]);
}

/// The page base declares the no-overscroll policy once: no scroller chains or
/// rubber-bands, and the document itself has no scroll range. Both halves are
/// needed — the first alone leaves the `height: 100%` vs `svh` page scroll.
#[test]
fn the_page_base_disables_vertical_overscroll() {
    let sheet = without_comments(inline_stylesheet(INDEX_HTML));
    let normalised = sheet.split_whitespace().collect::<Vec<_>>().join(" ");
    for declaration in [
        "* { overscroll-behavior-y: none; }",
        "html, body { overflow: hidden; }",
    ] {
        assert!(
            normalised.contains(declaration),
            "index.html's base style lost `{declaration}`"
        );
    }
}
