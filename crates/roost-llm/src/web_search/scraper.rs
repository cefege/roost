//! Browser-profiled HTML search adapters ported from OMP search providers (MIT).
//! Fetches mirror OMP's browser-header request path and parse server-rendered organic results.
//! OMP's stealth Puppeteer fallback requires a browser registry unavailable in roost-llm.
use url::form_urlencoded;

use super::scraper_results::parse_results;

use reqwest::{
    Method,
    header::{HeaderMap, HeaderValue, REFERER, USER_AGENT},
};
use scraper::{Html, Selector};
use tokio_util::sync::CancellationToken;

use super::types::{Recency, SearchResponse};
use crate::LlmError;

const BROWSER_UA: &str = "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/149.0.0.0 Safari/537.36";

pub(super) fn browser_headers() -> HeaderMap {
    let mut headers = HeaderMap::new();
    headers.insert(USER_AGENT, HeaderValue::from_static(BROWSER_UA));
    headers.insert("accept", HeaderValue::from_static("text/html,application/xhtml+xml,application/xml;q=0.9,image/avif,image/webp,image/apng,*/*;q=0.8,application/signed-exchange;v=b3;q=0.7"));
    headers.insert(
        "accept-language",
        HeaderValue::from_static("en-US,en;q=0.9"),
    );
    headers.insert("cache-control", HeaderValue::from_static("max-age=0"));
    headers.insert("priority", HeaderValue::from_static("u=0, i"));
    headers.insert(
        "sec-ch-ua",
        HeaderValue::from_static(
            "\"Google Chrome\";v=\"149\", \"Chromium\";v=\"149\", \";Not A Brand\";v=\"99\"",
        ),
    );
    headers.insert("sec-ch-ua-mobile", HeaderValue::from_static("?0"));
    headers.insert("sec-ch-ua-platform", HeaderValue::from_static("\"macOS\""));
    headers.insert("sec-fetch-dest", HeaderValue::from_static("document"));
    headers.insert("sec-fetch-mode", HeaderValue::from_static("navigate"));
    headers.insert("sec-fetch-site", HeaderValue::from_static("none"));
    headers.insert("sec-fetch-user", HeaderValue::from_static("?1"));
    headers.insert("upgrade-insecure-requests", HeaderValue::from_static("1"));
    headers
}

pub(super) struct HtmlSearchRequest<'a> {
    pub base: &'a str,
    pub engine: &'a str,
    pub query: &'a str,
    pub count: u32,
    pub recency: Option<Recency>,
    pub headers: &'a HeaderMap,
}

pub(super) async fn search_html(
    http: &reqwest::Client,
    request: HtmlSearchRequest<'_>,
    cancel: CancellationToken,
) -> Result<SearchResponse, LlmError> {
    if request.engine == "startpage" {
        return search_startpage(http, request, cancel).await;
    }
    let url = build_search_url(&request)?;
    let mut headers = request.headers.clone();
    let builder = if request.engine == "duckduckgo" {
        headers.insert(
            reqwest::header::CONTENT_TYPE,
            HeaderValue::from_static("application/x-www-form-urlencoded"),
        );
        headers.insert(REFERER, HeaderValue::from_static("https://duckduckgo.com/"));
        http.request(Method::POST, url)
            .headers(headers)
            .body(encode_form(&[("q", request.query)]))
    } else {
        headers.insert(
            REFERER,
            HeaderValue::from_str(&format!("{}/", request.base.trim_end_matches('/')))
                .map_err(|error| LlmError::Decode(error.to_string()))?,
        );
        http.get(url).headers(headers)
    };
    let (status, html) = send_html(builder, &cancel).await?;
    check_html_response(request.engine, status, &html)?;
    Ok(parse_results(
        request.engine,
        request.base,
        &html,
        request.count,
    ))
}

async fn search_startpage(
    http: &reqwest::Client,
    request: HtmlSearchRequest<'_>,
    cancel: CancellationToken,
) -> Result<SearchResponse, LlmError> {
    let home = format!("{}/", request.base.trim_end_matches('/'));
    let mut home_headers = request.headers.clone();
    home_headers.insert(
        REFERER,
        HeaderValue::from_static("https://www.startpage.com/"),
    );
    let (home_status, home_html) =
        send_html(http.get(&home).headers(home_headers), &cancel).await?;
    let hidden_inputs = if (200..300).contains(&home_status) {
        parse_startpage_inputs(&home_html)
    } else {
        None
    };
    let search_url = format!("{}/sp/search", request.base.trim_end_matches('/'));
    let date = request.recency.map(startpage_recency);
    let (builder, referer) = if let Some(hidden) = hidden_inputs {
        let mut form = hidden;
        form.push(("query".to_owned(), request.query.to_owned()));
        if let Some(date) = date {
            form.push(("with_date".to_owned(), date.to_owned()));
        }
        let mut headers = request.headers.clone();
        headers.insert(
            reqwest::header::CONTENT_TYPE,
            HeaderValue::from_static("application/x-www-form-urlencoded"),
        );
        (
            http.post(&search_url).headers(headers).body(encode_form(
                &form
                    .iter()
                    .map(|(key, value)| (key.as_str(), value.as_str()))
                    .collect::<Vec<_>>(),
            )),
            home,
        )
    } else {
        let mut url = reqwest::Url::parse(&search_url)
            .map_err(|error| LlmError::Decode(error.to_string()))?;
        url.query_pairs_mut().append_pair("query", request.query);
        if let Some(date) = date {
            url.query_pairs_mut().append_pair("with_date", date);
        }
        (http.get(url).headers(request.headers.clone()), home)
    };
    let mut builder = builder;
    builder = builder.header(REFERER, referer);
    let (status, html) = send_html(builder, &cancel).await?;
    check_html_response("startpage", status, &html)?;
    Ok(parse_results(
        "startpage",
        request.base,
        &html,
        request.count,
    ))
}

fn parse_startpage_inputs(html: &str) -> Option<Vec<(String, String)>> {
    let document = Html::parse_document(html);
    let form_selector = Selector::parse("form[action='/sp/search']").ok()?;
    let hidden_selector = Selector::parse("input[type='hidden']").ok()?;
    let form = document.select(&form_selector).next()?;
    let values: Vec<_> = form
        .select(&hidden_selector)
        .filter_map(|input| {
            let name = input.value().attr("name")?;
            Some((
                name.to_owned(),
                input.value().attr("value").unwrap_or_default().to_owned(),
            ))
        })
        .collect();
    values
        .iter()
        .any(|(name, value)| name == "sc" && !value.is_empty())
        .then_some(values)
}

fn build_search_url(request: &HtmlSearchRequest<'_>) -> Result<reqwest::Url, LlmError> {
    let path = match request.engine {
        "duckduckgo" => "/html/",
        "google" | "ecosia" => "/search",
        "mojeek" => "/search",
        _ => {
            return Err(LlmError::Decode(format!(
                "unsupported HTML engine {}",
                request.engine
            )));
        }
    };
    let mut url = reqwest::Url::parse(&format!("{}{}", request.base.trim_end_matches('/'), path))
        .map_err(|error| LlmError::Decode(error.to_string()))?;
    if request.engine == "duckduckgo" {
        return Ok(url);
    }
    url.query_pairs_mut().append_pair("q", request.query);
    match request.engine {
        "google" => {
            url.query_pairs_mut()
                .append_pair("num", &request.count.to_string())
                .append_pair("hl", "en")
                .append_pair("gl", "us")
                .append_pair("udm", "14")
                .append_pair("pws", "0");
            if let Some(date) = request.recency {
                url.query_pairs_mut()
                    .append_pair("tbs", google_recency(date));
            }
        }
        "mojeek" => {
            url.query_pairs_mut()
                .append_pair("t", &request.count.to_string())
                .append_pair("arc", "none")
                .append_pair("lang", "en")
                .append_pair("lb", "en")
                .append_pair("theme", "dark");
            if let Some(date) = request.recency {
                url.query_pairs_mut()
                    .append_pair("since", mojeek_recency(date));
            }
        }
        _ => {}
    }
    Ok(url)
}

async fn send_html(
    builder: reqwest::RequestBuilder,
    cancel: &CancellationToken,
) -> Result<(u16, String), LlmError> {
    let response = tokio::select! { _ = cancel.cancelled() => return Err(LlmError::Cancelled), response = builder.send() => response? };
    let status = response.status().as_u16();
    let text = tokio::select! { _ = cancel.cancelled() => return Err(LlmError::Cancelled), text = response.text() => text? };
    Ok((status, text))
}

fn check_html_response(engine: &str, status: u16, html: &str) -> Result<(), LlmError> {
    if (200..300).contains(&status) {
        if engine == "duckduckgo" && (html.contains("anomaly-modal") || html.contains("anomaly.js"))
        {
            return Err(LlmError::Http {
                status: 429,
                body: "DuckDuckGo challenge page".to_owned(),
            });
        }
        if engine == "startpage"
            && (html.contains("component---src-pages-captcha")
                || html.contains("/sp/captcha")
                || html.contains("<title data-react-helmet=\"true\">Startpage Blocked</title>")
                || html.contains("id=\"anubis_challenge\"")
                || html.contains("id=\"anubis-main\""))
        {
            return Err(LlmError::Http {
                status: 429,
                body: "Startpage bot challenge".to_owned(),
            });
        }
        return Ok(());
    }
    Err(LlmError::Http {
        status,
        body: html.chars().take(8192).collect(),
    })
}

fn startpage_recency(recency: Recency) -> &'static str {
    match recency {
        Recency::Day => "d",
        Recency::Week => "w",
        Recency::Month => "m",
        Recency::Year => "y",
    }
}
fn google_recency(recency: Recency) -> &'static str {
    match recency {
        Recency::Day => "qdr:d",
        Recency::Week => "qdr:w",
        Recency::Month => "qdr:m",
        Recency::Year => "qdr:y",
    }
}
fn mojeek_recency(recency: Recency) -> &'static str {
    match recency {
        Recency::Day => "day",
        Recency::Week => "week",
        Recency::Month => "month",
        Recency::Year => "year",
    }
}
fn encode_form(fields: &[(&str, &str)]) -> String {
    form_urlencoded::Serializer::new(String::new())
        .extend_pairs(fields.iter().copied())
        .finish()
}
