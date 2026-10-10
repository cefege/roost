//! Parses organic result cards from server-rendered search pages.
//! This module owns provider-specific card selectors and redirect unwrapping.
//! Request construction and CAPTCHA handling remain in `scraper`.
use scraper::{ElementRef, Html, Selector};

use super::types::{SearchResponse, SearchSource};

pub(super) fn parse_results(engine: &str, base: &str, html: &str, limit: u32) -> SearchResponse {
    let document = Html::parse_document(html);
    let container_selector = match engine {
        "duckduckgo" => ".result",
        "startpage" => "div.result",
        "google" => "div.MjjYud",
        "ecosia" => "article[data-test-id='organic-result']",
        _ => "ul.results-standard > li",
    };
    let Ok(container_selector) = Selector::parse(container_selector) else {
        return SearchResponse::empty(engine);
    };
    let link_selector = match engine {
        "startpage" => Selector::parse("a.result-link").ok(),
        "mojeek" => Selector::parse("a.title").ok(),
        _ => Selector::parse("a").ok(),
    };
    let title_selector = match engine {
        "google" => Selector::parse("h3").ok(),
        "startpage" => Selector::parse("h2, h3").ok(),
        "ecosia" => Selector::parse("[data-test-id='result-title']").ok(),
        _ => None,
    };
    let snippet_selector = match engine {
        "duckduckgo" => Selector::parse(".result__snippet").ok(),
        "google" => {
            Selector::parse("[data-sncf='1'] .VwiC3b, .VwiC3b, .IsZvec, .BNeawe.s3v9rd").ok()
        }
        "startpage" => Selector::parse("p.description").ok(),
        "ecosia" => Selector::parse(
            "[data-test-id='web-result-description'], [data-test-id='result-description']",
        )
        .ok(),
        _ => Selector::parse("p.s").ok(),
    };
    let mut response = SearchResponse::empty(engine);
    for container in document.select(&container_selector).take(limit as usize) {
        if engine == "duckduckgo"
            && container
                .value()
                .classes()
                .any(|class| class == "result--ad")
        {
            continue;
        }
        if let Some((title, url, snippet)) = parse_result(
            engine,
            base,
            container,
            &link_selector,
            &title_selector,
            &snippet_selector,
        ) && !response.sources.iter().any(|source| source.url == url)
        {
            response.sources.push(SearchSource {
                title,
                url,
                snippet,
                published: None,
                age_seconds: None,
            });
        }
    }
    response
}

fn parse_result(
    engine: &str,
    base: &str,
    container: ElementRef<'_>,
    links: &Option<Selector>,
    titles: &Option<Selector>,
    snippets: &Option<Selector>,
) -> Option<(String, String, Option<String>)> {
    let link = links.as_ref().and_then(|selector| {
        container
            .select(selector)
            .find(|link| link.value().attr("href").is_some())
    })?;
    let raw_url = link.value().attr("href")?;
    let mut url = resolve_result_url(engine, raw_url, base)?;
    if engine == "duckduckgo"
        && reqwest::Url::parse(&url).ok().is_some_and(|target| {
            target
                .host_str()
                .is_some_and(|host| host == "duckduckgo.com" || host.ends_with(".duckduckgo.com"))
                && target.path() == "/y.js"
        })
    {
        return None;
    }
    if engine == "google" {
        url = unwrap_google_url(&url)?;
    }
    if engine == "startpage" && is_startpage_url(&url)
        || engine == "ecosia" && is_ecosia_url(&url)
        || engine == "mojeek" && is_mojeek_url(&url)
    {
        return None;
    }
    let title = titles
        .as_ref()
        .and_then(|selector| container.select(selector).next())
        .map(|node| node.text().collect::<String>())
        .unwrap_or_else(|| link.text().collect::<String>())
        .trim()
        .to_owned();
    if title.is_empty() {
        return None;
    }
    let snippet = snippets
        .as_ref()
        .and_then(|selector| container.select(selector).next())
        .map(|node| node.text().collect::<String>().trim().to_owned())
        .filter(|value| !value.is_empty());
    Some((title, url, snippet))
}

fn resolve_result_url(engine: &str, href: &str, base: &str) -> Option<String> {
    let mut url = reqwest::Url::parse(href)
        .or_else(|_| reqwest::Url::parse(base).and_then(|base| base.join(href)))
        .ok()?;
    if engine == "duckduckgo"
        && let Some((_, target)) = url.query_pairs().find(|(key, _)| key == "uddg")
    {
        url = reqwest::Url::parse(&target).ok()?;
    }
    if !matches!(url.scheme(), "http" | "https") {
        return None;
    }
    Some(url.to_string())
}

fn unwrap_google_url(href: &str) -> Option<String> {
    let url = reqwest::Url::parse(href).ok()?;
    if (url.host_str()? == "google.com" || url.host_str()? == "www.google.com")
        && url.path() == "/url"
    {
        let target = url
            .query_pairs()
            .find(|(key, _)| key == "q" || key == "url")?
            .1
            .to_string();
        let target_url = reqwest::Url::parse(&target).ok()?;
        return matches!(target_url.scheme(), "http" | "https").then(|| target_url.to_string());
    }
    matches!(url.scheme(), "http" | "https").then(|| url.to_string())
}

fn is_startpage_url(url: &str) -> bool {
    reqwest::Url::parse(url)
        .ok()
        .and_then(|url| {
            url.host_str()
                .map(|host| host == "startpage.com" || host.ends_with(".startpage.com"))
        })
        .unwrap_or(false)
}

fn is_ecosia_url(url: &str) -> bool {
    reqwest::Url::parse(url)
        .ok()
        .and_then(|url| {
            url.host_str()
                .map(|host| host == "ecosia.org" || host == "www.ecosia.org")
        })
        .unwrap_or(false)
}

fn is_mojeek_url(url: &str) -> bool {
    reqwest::Url::parse(url)
        .ok()
        .and_then(|parsed| {
            parsed.host_str().map(|host| {
                ["mojeek.com", "mojeek.co.uk", "mojeek.fr", "mojeek.de"]
                    .iter()
                    .any(|suffix| host == *suffix || host.ends_with(&format!(".{suffix}")))
            })
        })
        .unwrap_or(false)
}
