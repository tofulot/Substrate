use anyhow::{Context, Result, bail, ensure};
use chrono::{SecondsFormat, Utc};
use ego_tree::iter::Edge;
use reqwest::{
    Client, Response, StatusCode,
    dns::{Addrs, Name, Resolve, Resolving},
    header::{ACCEPT, CACHE_CONTROL, CONTENT_TYPE, HeaderValue, LOCATION},
    redirect::Policy,
};
use scraper::{Html, Node, Selector};
use serde_json::{Value, json};
use std::{collections::HashSet, net::IpAddr, sync::Arc, time::Duration};
use tokio_util::sync::CancellationToken;
use url::{Host, Url};

const SEARCH_ENDPOINT: &str = "https://api.search.brave.com/res/v1/web/search";
const MAX_RESULTS: usize = 8;
const MAX_BODY_BYTES: usize = 1024 * 1024;
const MAX_TEXT_BYTES: usize = 24 * 1024;
const MAX_URL_BYTES: usize = 4096;
const MAX_REDIRECTS: usize = 5;
const DEADLINE: Duration = Duration::from_secs(20);

pub struct WebTools {
    client: Client,
    search_key: String,
}

impl WebTools {
    pub fn search_available(&self) -> bool {
        !self.search_key.is_empty()
    }

    pub fn new(search_key: &str) -> Result<Self> {
        let client = Client::builder()
            .no_proxy()
            .redirect(Policy::none())
            .dns_resolver(Arc::new(PublicDns))
            .connect_timeout(Duration::from_secs(6))
            .user_agent(concat!("Substrate/", env!("CARGO_PKG_VERSION")))
            .build()?;
        Ok(Self {
            client,
            search_key: search_key.trim().into(),
        })
    }

    pub async fn search(&self, query: &str, cancel: &CancellationToken) -> Result<Value> {
        within_deadline(cancel, self.search_at(query, SEARCH_ENDPOINT)).await
    }

    async fn search_at(&self, query: &str, endpoint: &str) -> Result<Value> {
        let query = query.trim();
        ensure!(
            !query.is_empty()
                && query.chars().count() <= 600
                && query.split_whitespace().count() <= 75,
            "Search query must contain 1–600 characters and at most 75 words"
        );
        ensure!(
            !self.search_key.is_empty(),
            "Web search requires a Brave Search API key in Settings or BRAVE_SEARCH_API_KEY. You can still fetch a public URL directly.",
        );
        let mut key =
            HeaderValue::from_str(&self.search_key).context("Invalid Brave Search API key")?;
        key.set_sensitive(true);
        let response = self
            .client
            .get(endpoint)
            .query(&[
                ("q", query),
                ("count", &MAX_RESULTS.to_string()),
                ("result_filter", "web"),
                ("text_decorations", "false"),
            ])
            .header(ACCEPT, "application/json")
            .header("X-Subscription-Token", key)
            .send()
            .await
            .map_err(reqwest::Error::without_url)
            .context("Could not contact Brave Search")?;
        match response.status() {
            StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN => {
                bail!("Brave Search rejected the API key or the key lacks search access")
            }
            StatusCode::TOO_MANY_REQUESTS => bail!("Brave Search rate or usage limit reached"),
            status if !status.is_success() => bail!("Brave Search returned HTTP {status}"),
            _ => {}
        }
        let (body, truncated) = read_body(response).await?;
        ensure!(!truncated, "Brave Search response exceeded 1 MiB");
        let payload: Value =
            serde_json::from_slice(&body).context("Invalid Brave Search response")?;
        Ok(json!({
            "query": query,
            "provider": "Brave Search",
            "retrieved_at": retrieved_at(),
            "results": search_results(&payload),
        }))
    }

    pub async fn fetch(&self, url: &str, cancel: &CancellationToken) -> Result<Value> {
        within_deadline(cancel, self.fetch_page(url)).await
    }

    async fn fetch_page(&self, raw_url: &str) -> Result<Value> {
        let mut url = public_url(raw_url.trim())?;
        let mut redirects_left = MAX_REDIRECTS;
        loop {
            let response = self
                .client
                .get(url.clone())
                .header(
                    ACCEPT,
                    "text/html, text/plain, application/json, application/xml, text/*;q=0.8",
                )
                .header(CACHE_CONTROL, "no-cache")
                .send()
                .await
                .map_err(reqwest::Error::without_url)
                .context("Could not reach the public URL")?;
            if matches!(
                response.status(),
                StatusCode::MOVED_PERMANENTLY
                    | StatusCode::FOUND
                    | StatusCode::SEE_OTHER
                    | StatusCode::TEMPORARY_REDIRECT
                    | StatusCode::PERMANENT_REDIRECT
            ) {
                ensure!(redirects_left > 0, "Too many URL redirects");
                redirects_left -= 1;
                let location = response
                    .headers()
                    .get(LOCATION)
                    .context("Redirect is missing a location")?
                    .to_str()?;
                url = public_url(url.join(location).context("Invalid redirect URL")?.as_str())?;
                continue;
            }
            ensure!(
                response.status().is_success(),
                "Page returned HTTP {}",
                response.status()
            );
            let raw_type = response
                .headers()
                .get(CONTENT_TYPE)
                .and_then(|value| value.to_str().ok())
                .unwrap_or("text/plain");
            let content_type = raw_type
                .split(';')
                .next()
                .unwrap_or("")
                .trim()
                .to_ascii_lowercase();
            ensure!(
                content_type.starts_with("text/")
                    || matches!(
                        content_type.as_str(),
                        "application/json" | "application/xml"
                    )
                    || content_type.ends_with("+json")
                    || content_type.ends_with("+xml"),
                "Unsupported page content type"
            );
            let (body, body_truncated) = read_body(response).await?;
            let retrieved_at = retrieved_at();
            let (title, text) = tokio::task::spawn_blocking(move || {
                let decoded = String::from_utf8_lossy(&body);
                if matches!(content_type.as_str(), "text/html" | "application/xhtml+xml") {
                    extract_html(&decoded)
                } else {
                    (None, decoded.trim().to_owned())
                }
            })
            .await?;
            ensure!(!text.is_empty(), "Page returned no readable text");
            return Ok(json!({
                "url": url.as_str(),
                "title": title,
                "retrieved_at": retrieved_at,
                "text": excerpt(&text, MAX_TEXT_BYTES),
                "truncated": body_truncated || text.len() > MAX_TEXT_BYTES,
            }));
        }
    }
}

async fn within_deadline<T>(
    cancel: &CancellationToken,
    work: impl Future<Output = Result<T>>,
) -> Result<T> {
    tokio::select! {
        biased;
        _ = cancel.cancelled() => bail!("Web request cancelled"),
        result = tokio::time::timeout(DEADLINE, work) => result.context("Web request timed out")?,
    }
}

async fn read_body(mut response: Response) -> Result<(Vec<u8>, bool)> {
    let mut body = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(reqwest::Error::without_url)?
    {
        let remaining = MAX_BODY_BYTES - body.len();
        if chunk.len() > remaining {
            body.extend_from_slice(&chunk[..remaining]);
            return Ok((body, true));
        }
        body.extend_from_slice(&chunk);
    }
    Ok((body, false))
}

fn retrieved_at() -> String {
    Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true)
}

fn excerpt(text: &str, bytes: usize) -> &str {
    &text[..text.floor_char_boundary(bytes)]
}

fn search_results(payload: &Value) -> Vec<Value> {
    let mut seen = HashSet::new();
    payload["web"]["results"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|entry| {
            let url = public_url(entry["url"].as_str()?).ok()?;
            if !seen.insert(url.clone()) {
                return None;
            }
            Some(json!({
                "url": url.as_str(),
                "title": excerpt(entry["title"].as_str().unwrap_or(url.as_str()), 400),
                "snippet": excerpt(entry["description"].as_str().unwrap_or(""), 1500),
            }))
        })
        .take(MAX_RESULTS)
        .collect()
}

fn public_url(raw_url: &str) -> Result<Url> {
    ensure!(raw_url.len() <= MAX_URL_BYTES, "URL exceeds 4096 bytes");
    let mut url = Url::parse(raw_url).context("Expected an absolute HTTP(S) URL")?;
    let port = match url.scheme() {
        "http" => 80,
        "https" => 443,
        _ => bail!("Only HTTP and HTTPS URLs are supported"),
    };
    ensure!(
        url.port_or_known_default() == Some(port),
        "URL must use the standard HTTP(S) port"
    );
    ensure!(
        url.username().is_empty() && url.password().is_none(),
        "URL credentials are not supported"
    );
    let public = match url.host() {
        Some(Host::Domain(name)) => {
            let name = name.trim_end_matches('.');
            name.contains('.')
                && !["localhost", "local", "internal", "home.arpa", "onion"]
                    .iter()
                    .any(|suffix| name == *suffix || name.ends_with(&format!(".{suffix}")))
        }
        Some(Host::Ipv4(addr)) => public_ip(addr.into()),
        Some(Host::Ipv6(addr)) => public_ip(addr.into()),
        None => false,
    };
    ensure!(public, "URL must point to a public website");
    url.set_fragment(None);
    ensure!(
        url.as_str().len() <= MAX_URL_BYTES,
        "URL exceeds 4096 bytes"
    );
    Ok(url)
}

fn public_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(addr) => !matches!(
            addr.octets(),
            [0 | 10 | 127 | 224..=255, ..]
                | [100, 64..=127, ..]
                | [169, 254, ..]
                | [172, 16..=31, ..]
                | [192, 0, 0 | 2, _]
                | [192, 88, 99, _]
                | [192, 168, ..]
                | [198, 18 | 19, ..]
                | [198, 51, 100, _]
                | [203, 0, 113, _]
        ),
        IpAddr::V6(addr) => {
            matches!(addr.segments(), [0x2000..=0x3fff, ..])
                && !matches!(
                    addr.segments(),
                    [0x2001, 0..=0x01ff | 0x0db8, ..] | [0x2002, ..] | [0x3fff, 0..=0x0fff, ..]
                )
        }
    }
}

struct PublicDns;

impl Resolve for PublicDns {
    fn resolve(&self, name: Name) -> Resolving {
        Box::pin(async move {
            // Filter the socket addresses actually used for the connection.
            let host = format!("{}.", name.as_str().trim_end_matches('.'));
            let addresses: Vec<_> = tokio::net::lookup_host((host.as_str(), 0))
                .await?
                .filter(|addr| public_ip(addr.ip()))
                .collect();
            if addresses.is_empty() {
                return Err(std::io::Error::other("Host has no public address").into());
            }
            Ok(Box::new(addresses.into_iter()) as Addrs)
        })
    }
}

fn extract_html(html: &str) -> (Option<String>, String) {
    let document = Html::parse_document(html);
    let title_selector = Selector::parse("head title").unwrap();
    let title = document
        .select(&title_selector)
        .next()
        .map(|el| {
            el.text()
                .collect::<String>()
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" ")
        })
        .filter(|text| !text.is_empty())
        .map(|text| excerpt(&text, 400).to_owned());
    let main_selector = Selector::parse("main, [role=main]").unwrap();
    let body_selector = Selector::parse("body").unwrap();
    let body = document
        .select(&main_selector)
        .next()
        .or_else(|| document.select(&body_selector).next())
        .unwrap_or_else(|| document.root_element());
    let mut raw = String::new();
    let mut hidden = None;
    for edge in body.traverse() {
        match edge {
            Edge::Open(node) if hidden.is_none() => match node.value() {
                Node::Element(el)
                    if matches!(
                        el.name(),
                        "script"
                            | "style"
                            | "template"
                            | "noscript"
                            | "svg"
                            | "head"
                            | "nav"
                            | "footer"
                            | "iframe"
                    ) || el.attr("hidden").is_some()
                        || el.attr("aria-hidden") == Some("true") =>
                {
                    hidden = Some(node.id());
                }
                Node::Element(el) if matches!(el.name(), "br" | "td" | "th") => raw.push(' '),
                Node::Text(text) => raw.push_str(text),
                _ => {}
            },
            Edge::Close(node) if hidden == Some(node.id()) => hidden = None,
            Edge::Close(node) if hidden.is_none() => {
                if let Node::Element(el) = node.value()
                    && matches!(
                        el.name(),
                        "p" | "div"
                            | "li"
                            | "section"
                            | "article"
                            | "tr"
                            | "pre"
                            | "blockquote"
                            | "h1"
                            | "h2"
                            | "h3"
                            | "h4"
                            | "h5"
                            | "h6"
                    )
                {
                    raw.push('\n');
                }
            }
            _ => {}
        }
    }
    let text = raw
        .lines()
        .map(|line| line.split_whitespace().collect::<Vec<_>>().join(" "))
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>()
        .join("\n");
    (title, text)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::{
        io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
        net::TcpListener,
        task::JoinHandle,
    };

    const FIXTURE: &str = "http://fixture.example.com";

    async fn server(responses: Vec<(&'static str, String)>) -> (WebTools, JoinHandle<Vec<String>>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let client = Client::builder()
            .no_proxy()
            .redirect(Policy::none())
            .resolve("fixture.example.com", listener.local_addr().unwrap())
            .build()
            .unwrap();
        let requests = tokio::spawn(async move {
            let mut requests = Vec::new();
            for (headers, body) in responses {
                let (socket, _) = listener.accept().await.unwrap();
                let mut socket = BufReader::new(socket);
                let mut request = String::new();
                loop {
                    let start = request.len();
                    assert_ne!(socket.read_line(&mut request).await.unwrap(), 0);
                    if &request[start..] == "\r\n" {
                        break;
                    }
                }
                requests.push(request);
                let response = format!(
                    "HTTP/1.1 {headers}\r\nConnection: close\r\nContent-Length: {}\r\n\r\n{body}",
                    body.len()
                );
                socket.write_all(response.as_bytes()).await.unwrap();
            }
            requests
        });
        (
            WebTools {
                client,
                search_key: "fixture-key".into(),
            },
            requests,
        )
    }

    #[tokio::test]
    async fn search_returns_ranked_citation_metadata() {
        let (web, requests) = server(vec![(
            "200 OK\r\nContent-Type: application/json",
            json!({"web":{"results":[
                {"url":"https://www.rust-lang.org/", "title":"Rust", "description":"A public source"},
                {"url":"https://www.rust-lang.org/#duplicate"},
                {"url":"http://127.0.0.1/"},
                {"url":"https://doc.rust-lang.org/book/"}
            ]}}).to_string(),
        )]).await;
        let result = web.search_at("  Rust & docs  ", FIXTURE).await.unwrap();
        assert_eq!(result["query"], "Rust & docs");
        assert_eq!(result["results"].as_array().unwrap().len(), 2);
        assert_eq!(
            result["results"][0],
            json!({
                "url": "https://www.rust-lang.org/", "title": "Rust", "snippet": "A public source"
            })
        );
        assert_eq!(
            result["results"][1]["url"],
            "https://doc.rust-lang.org/book/"
        );
        assert!(
            chrono::DateTime::parse_from_rfc3339(result["retrieved_at"].as_str().unwrap()).is_ok()
        );
        let requests = requests.await.unwrap();
        assert!(requests[0].starts_with("GET /?q=Rust+%26+docs&count=8&"));
        assert!(
            requests[0]
                .to_ascii_lowercase()
                .contains("x-subscription-token: fixture-key\r\n")
        );
        assert!(!result.to_string().contains("fixture-key"));
    }

    #[tokio::test]
    async fn fetch_follows_public_redirects_and_extracts_bounded_text() {
        let html = format!(
            "<title> Release &amp; notes </title><nav>menu</nav><main><h1>Version 2</h1><p>Live text.</p><script>injected()</script><p hidden>hidden</p><p>{}</p></main>",
            "é".repeat(MAX_TEXT_BYTES)
        );
        let (web, requests) = server(vec![
            ("302 Found\r\nLocation: /notes#version", String::new()),
            ("200 OK\r\nContent-Type: text/html; charset=utf-8", html),
        ])
        .await;
        let result = web.fetch(FIXTURE, &CancellationToken::new()).await.unwrap();
        assert_eq!(result["url"], format!("{FIXTURE}/notes"));
        assert_eq!(result["title"], "Release & notes");
        assert_eq!(result["truncated"], true);
        let text = result["text"].as_str().unwrap();
        assert!(text.starts_with("Version 2\nLive text.\n"));
        assert!(!text.contains("menu") && !text.contains("injected") && !text.contains("hidden"));
        assert!(text.len() <= MAX_TEXT_BYTES);
        assert!(
            chrono::DateTime::parse_from_rfc3339(result["retrieved_at"].as_str().unwrap()).is_ok()
        );
        let requests = requests.await.unwrap();
        assert!(requests[1].starts_with("GET /notes "));
        assert!(
            requests
                .iter()
                .all(|request| !request.contains("fixture-key"))
        );
    }

    #[tokio::test]
    async fn fetch_rejects_redirect_to_private_address() {
        let (web, requests) = server(vec![(
            "302 Found\r\nLocation: http://127.0.0.1/",
            String::new(),
        )])
        .await;
        let error = web
            .fetch(FIXTURE, &CancellationToken::new())
            .await
            .unwrap_err();
        assert!(error.to_string().contains("public website"));
        assert_eq!(requests.await.unwrap().len(), 1);
    }

    #[test]
    fn rejects_unsafe_url_targets() {
        for url in [
            "file:///etc/passwd",
            "https://user:pass@example.com",
            "https://localhost/",
            "http://127.1/",
            "http://2130706433/",
            "http://169.254.169.254/",
            "http://100.64.0.1/",
            "http://172.16.0.1/",
            "http://192.168.1.1/",
            "http://[::1]/",
            "http://[::ffff:127.0.0.1]/",
            "http://[fc00::1]/",
            "http://machine.local/",
            "https://example.com:8080/",
        ] {
            assert!(public_url(url).is_err(), "{url}");
        }
        assert!(public_url("https://example.com/").is_ok());
        assert!(public_url("https://[2606:4700:4700::1111]/").is_ok());
    }
}
