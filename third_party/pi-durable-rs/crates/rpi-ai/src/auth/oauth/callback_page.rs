//! Port of `packages/ai/src/utils/oauth-page.ts` @ pi abe508e1b (v1.0.0,
//! moved from `auth/oauth/oauth-page.ts` by `4df157433`) — the shared browser
//! page rendered by the loopback OAuth callback servers.
//!
//! The generic callback server itself lives in [`super::callback_server`];
//! this module only renders the page (upstream `renderPage`,
//! `oauthSuccessHtml`, `oauthErrorHtml`). The built-in MCP OAuth flow
//! consumes the two HTML helpers through its `render_page` hook so the MCP
//! server and the provider flows show the same brand page.

/// `LOGO_SVG` (verbatim).
const LOGO_SVG: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 800 800" aria-hidden="true"><path fill="#F09082" d="M165.29 165.29H517.36V400H400V282.65H165.29Z"/><path fill="#4D9ABF" d="M165.29 282.65H282.65V400H400V517.36H282.65V634.72H165.29Z"/><path fill="#F1BE58" d="M517.36 400H634.72V634.72H517.36Z"/></svg>"##;

/// `escapeHtml` — the same five entity replacements, in the same order.
fn escape_html(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}

/// `renderPage` — verbatim markup; `__*__` tokens are substituted instead of
/// template interpolation so the CSS braces survive untouched.
const PAGE_TEMPLATE: &str = r##"<!doctype html>
<html lang="en">
<head>
  <meta charset="utf-8" />
  <meta name="viewport" content="width=device-width, initial-scale=1" />
  <title>__TITLE__</title>
  <style>
    :root {
      --text: #fafafa;
      --text-dim: #a1a1aa;
      --page-bg: #09090b;
      --font-sans: ui-sans-serif, system-ui, -apple-system, BlinkMacSystemFont, "Segoe UI", Roboto, "Helvetica Neue", Arial, "Noto Sans", sans-serif, "Apple Color Emoji", "Segoe UI Emoji", "Segoe UI Symbol", "Noto Color Emoji";
      --font-mono: ui-monospace, SFMono-Regular, Menlo, Monaco, Consolas, "Liberation Mono", "Courier New", monospace;
    }
    * { box-sizing: border-box; }
    html { color-scheme: dark; }
    body {
      margin: 0;
      min-height: 100vh;
      display: flex;
      align-items: center;
      justify-content: center;
      padding: 24px;
      background: var(--page-bg);
      color: var(--text);
      font-family: var(--font-sans);
      text-align: center;
    }
    main {
      width: 100%;
      max-width: 560px;
      display: flex;
      flex-direction: column;
      align-items: center;
      justify-content: center;
    }
    .logo {
      width: 72px;
      height: 72px;
      display: block;
      margin-bottom: 24px;
    }
    h1 {
      margin: 0 0 10px;
      font-size: 28px;
      line-height: 1.15;
      font-weight: 650;
      color: var(--text);
    }
    p {
      margin: 0;
      line-height: 1.7;
      color: var(--text-dim);
      font-size: 15px;
    }
    .details {
      margin-top: 16px;
      font-family: var(--font-mono);
      font-size: 13px;
      color: var(--text-dim);
      white-space: pre-wrap;
      word-break: break-word;
    }
  </style>
</head>
<body>
  <main>
    <div class="logo">__LOGO_SVG__</div>
    <h1>__HEADING__</h1>
    <p>__MESSAGE__</p>
    __DETAILS__
  </main>
</body>
</html>"##;

/// `renderPage(options)`.
fn render_page(title: &str, heading: &str, message: &str, details: Option<&str>) -> String {
    let details_block = match details {
        Some(details) => format!("<div class=\"details\">{}</div>", escape_html(details)),
        None => String::new(),
    };
    PAGE_TEMPLATE
        .replace("__TITLE__", &escape_html(title))
        .replace("__HEADING__", &escape_html(heading))
        .replace("__MESSAGE__", &escape_html(message))
        .replace("__DETAILS__", &details_block)
        .replace("__LOGO_SVG__", LOGO_SVG)
}

/// `oauthSuccessHtml`.
pub fn oauth_success_html(message: &str) -> String {
    render_page(
        "Authentication successful",
        "Authentication successful",
        message,
        None,
    )
}

/// `oauthErrorHtml`.
pub fn oauth_error_html(message: &str, details: Option<&str>) -> String {
    render_page(
        "Authentication failed",
        "Authentication failed",
        message,
        details,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `escapeHtml`: all five entities, `&` replaced first.
    #[test]
    fn escape_html_replaces_five_entities() {
        assert_eq!(escape_html(r#"&<>"'"#), "&amp;&lt;&gt;&quot;&#39;");
        assert_eq!(escape_html("&amp;"), "&amp;amp;");
    }

    #[test]
    fn success_page_renders_verbatim_structure() {
        let html = oauth_success_html("done <b>");
        assert!(html.starts_with("<!doctype html>\n<html lang=\"en\">"));
        assert!(html.ends_with("</html>"));
        assert!(html.contains("<title>Authentication successful</title>"));
        assert!(html.contains("<h1>Authentication successful</h1>"));
        assert!(html.contains("<p>done &lt;b&gt;</p>"));
        assert!(html.contains(LOGO_SVG));
        // v1.0.0 three-color badge (upstream oauth-callback-server.test.ts:74-76).
        assert!(html.contains("fill=\"#F09082\""));
        assert!(html.contains("fill=\"#4D9ABF\""));
        assert!(html.contains("fill=\"#F1BE58\""));
        // No details → the interpolation line keeps its 4-space indent.
        assert!(html.contains("\n    \n  </main>"));
    }

    #[test]
    fn error_page_renders_details_block() {
        let html = oauth_error_html("nope", Some("Error: access_denied"));
        assert!(html.contains("<title>Authentication failed</title>"));
        assert!(html.contains("<div class=\"details\">Error: access_denied</div>"));
    }
}
