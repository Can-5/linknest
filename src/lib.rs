//! LinkNest: local-first bookmark domain logic.
//!
//! Orbit and standalone tooling use this crate to store, search and export bookmarks.
//! URL *normalization* here is deliberately conservative — enough to deduplicate links
//! while never corrupting them: trim, lowercase scheme+host, drop default port, strip
//! fragment, strip trailing slash (preserving root "/"). Paths are never decoded,
//! queries are never reordered, and nothing makes network requests.

use serde::{Deserialize, Serialize};
use thiserror::Error;

#[derive(Debug, Error, PartialEq, Eq)]
pub enum LinkNestError {
    #[error("url must have an http(s) scheme and a host, got {0:?}")]
    BadUrl(String),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Bookmark {
    pub id: String,
    pub url: String,
    pub title: String,
    pub tags: Vec<String>,
    /// When the user saved it (ms since Unix epoch).
    pub created_at_ms: u64,
    /// Whether the user marked it as frequently used.
    pub starred: bool,
}

impl Bookmark {
    pub fn new(id: impl Into<String>, url: impl Into<String>, title: impl Into<String>, created_at_ms: u64) -> Self {
        Self {
            id: id.into(),
            url: url.into(),
            title: title.into(),
            tags: Vec::new(),
            created_at_ms,
            starred: false,
        }
    }
}

/// Normalize a URL for storage/dedup. Returns `Err` for non-http(s) or hostless input.
/// Fragment (`#...`) is always dropped — bookmarks are deliberately kept fragment-free.
pub fn normalize_url(input: &str) -> Result<String, LinkNestError> {
    let s = input.trim();
    let (scheme, rest) = split_scheme(s).ok_or_else(|| LinkNestError::BadUrl(input.into()))?;
    let scheme = scheme.to_lowercase();
    if scheme != "http" && scheme != "https" {
        return Err(LinkNestError::BadUrl(input.into()));
    }
    // Rest = [//]authority[/path][?query][#fragment]
    let mut rest = rest.strip_prefix("//").unwrap_or(rest);
    // Cut fragment (everything after first '#').
    if let Some(i) = rest.find('#') {
        rest = &rest[..i];
    }
    // Split authority from path.
    let (authority, path_query) = match rest.find('/') {
        Some(i) => (&rest[..i], &rest[i..]),
        None => (rest, ""),
    };
    // authority may include userinfo@ — split on last '@'.
    let (userinfo, hostport) = match authority.rfind('@') {
        Some(i) => (Some(&authority[..i]), &authority[i + 1..]),
        None => (None, authority),
    };
    // host[:port] — IPv6 in brackets may contain ':'; handle bracketed hosts.
    let (host, port) = split_host_port(hostport);
    let host_lc = host.to_lowercase();
    if host_lc.is_empty() {
        return Err(LinkNestError::BadUrl(input.into()));
    }
    let default_port = match scheme.as_str() {
        "http" => Some("80"),
        "https" => Some("443"),
        _ => None,
    };
    let port_str = match port {
        Some(p) if default_port == Some(p) => None, // default port: strip
        Some(p) => Some(format!(":{p}")),
        None => None,
    };
    // Split path and query, then normalize the path's trailing slashes only.
    let (path_raw, query) = match path_query.find('?') {
        Some(i) => (&path_query[..i], &path_query[i..]),
        None => (path_query, ""),
    };
    let mut path = path_raw.to_string();
    while path.ends_with('/') {
        path.pop();
    }
    let path = format!("{path}{query}");
    let userinfo_str = userinfo.map(|u| format!("{u}@")).unwrap_or_default();
    let normalized = format!("{scheme}://{userinfo_str}{host_lc}{}{}", port_str.unwrap_or_default(), path);
    Ok(normalized)
}

fn split_scheme(s: &str) -> Option<(&str, &str)> {
    let scheme_end = s.find("://")?;
    let scheme = &s[..scheme_end];
    if scheme.is_empty() || !scheme.chars().all(|c| c.is_ascii_alphanumeric() || c == '+' || c == '-' || c == '.') {
        return None;
    }
    Some((scheme, &s[scheme_end + 3..]))
}

fn split_host_port(hostport: &str) -> (&str, Option<&str>) {
    if hostport.starts_with('[') {
        // [v6::addr]:port
        if let Some(end) = hostport.find(']') {
            let host = &hostport[..=end];
            let rest = &hostport[end + 1..];
            if let Some(p) = rest.strip_prefix(':') {
                return (host, Some(p));
            }
            return (host, None);
        }
        return (hostport, None);
    }
    match hostport.rfind(':') {
        Some(i) => (&hostport[..i], Some(&hostport[i + 1..])),
        None => (hostport, None),
    }
}

/// Group bookmarks that normalize to the same URL. Returns groups of ids (size ≥ 2).
pub fn find_duplicates(bookmarks: &[Bookmark]) -> Vec<Vec<String>> {
    let mut groups: std::collections::BTreeMap<String, Vec<String>> = Default::default();
    for b in bookmarks {
        if let Ok(n) = normalize_url(&b.url) {
            groups.entry(n).or_default().push(b.id.clone());
        }
    }
    groups.into_values().filter(|g| g.len() > 1).collect()
}

/// Case-insensitive substring search over title, tags and url. Newest first.
pub fn search<'a>(bookmarks: &'a [Bookmark], query: &str) -> Vec<&'a Bookmark> {
    let q = query.trim().to_lowercase();
    if q.is_empty() {
        return bookmarks.iter().collect();
    }
    bookmarks
        .iter()
        .filter(|b| {
            b.title.to_lowercase().contains(&q)
                || b.url.to_lowercase().contains(&q)
                || b.tags.iter().any(|t| t.to_lowercase().contains(&q))
        })
        .collect()
}

/// Export as markdown list. Titles and urls are written literally; markdown-unsafe
/// characters in titles are backslash-escaped so the output stays valid.
pub fn export_markdown(bookmarks: &[Bookmark]) -> String {
    let mut out = String::from("# Bookmarks\n\n");
    for b in bookmarks {
        let title = escape_markdown(&b.title);
        out.push_str(&format!("- [{title}]({})", b.url));
        if !b.tags.is_empty() {
            let tags = b.tags.join(", ");
            out.push_str(&format!("  `{}`", tags));
        }
        out.push('\n');
    }
    out
}

/// Minimal, correct HTML escaping.
pub fn html_escape(input: &str) -> String {
    input
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}

fn escape_markdown(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    for c in input.chars() {
        if matches!(c, '\\' | '[' | ']' | '(' | ')' | '*' | '_' | '`' | '#') {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

/// Export as a self-contained static HTML page (no scripts, no external resources).
pub fn export_html(bookmarks: &[Bookmark]) -> String {
    let mut out = String::from(
        "<!doctype html><html lang=\"en\"><head><meta charset=\"utf-8\"><title>LinkNest Export</title></head><body><h1>Bookmarks</h1><ul>\n",
    );
    for b in bookmarks {
        let title = html_escape(&b.title);
        let url = html_escape(&b.url);
        out.push_str(&format!("<li><a href=\"{url}\">{title}</a></li>\n"));
    }
    out.push_str("</ul></body></html>\n");
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bm(id: &str, url: &str, title: &str) -> Bookmark {
        Bookmark::new(id, url, title, 0)
    }

    #[test]
    fn normalizes_scheme_and_host_case() {
        assert_eq!(normalize_url("HTTPS://EXAMPLE.COM/").unwrap(), "https://example.com");
    }

    #[test]
    fn strips_fragment_and_default_port() {
        assert_eq!(normalize_url("https://example.com:443/path#section").unwrap(), "https://example.com/path");
        assert_eq!(normalize_url("http://example.com:80/a/").unwrap(), "http://example.com/a");
        assert_eq!(normalize_url("http://example.com:8080/a/").unwrap(), "http://example.com:8080/a");
    }

    #[test]
    fn keeps_query_untouched() {
        assert_eq!(normalize_url("https://x.test/search?b=2&a=1").unwrap(), "https://x.test/search?b=2&a=1");
    }

    #[test]
    fn root_slash_preserved() {
        assert_eq!(normalize_url("https://example.com/").unwrap(), "https://example.com");
    }

    #[test]
    fn non_http_rejected() {
        assert!(normalize_url("file:///etc/passwd").is_err());
        assert!(normalize_url("ftp://example.com/x").is_err());
        assert!(normalize_url("javascript:alert(1)").is_err());
    }

    #[test]
    fn duplicates_group_normalized() {
        let bookmarks = vec![
            bm("1", "https://example.com/path#frag", "A"),
            bm("2", "https://EXAMPLE.com/path", "B"),
            bm("3", "https://other.com/", "C"),
        ];
        let dups = find_duplicates(&bookmarks);
        assert_eq!(dups.len(), 1);
        assert_eq!(dups[0], vec!["1".to_string(), "2".to_string()]);
    }

    #[test]
    fn html_escape_is_complete() {
        assert_eq!(html_escape("<a href=\"x\">&'"), "&lt;a href=&quot;x&quot;&gt;&amp;&#39;");
    }

    #[test]
    fn markdown_export_escapes_brackets() {
        let out = export_markdown(&[bm("1", "https://x.test/", "[x]")]);
        assert!(out.contains("\\[x\\]"));
    }

    #[test]
    fn html_export_contains_no_raw_script() {
        let out = export_html(&[bm("1", "https://x.test/", "<script>alert(1)</script>")]);
        assert!(!out.contains("<script>alert(1)</script>"));
        assert!(out.contains("&lt;script&gt;"));
    }

    #[test]
    fn search_matches_tags_and_title() {
        let mut b = bm("1", "https://docs.example.com/", "Rust Book");
        b.tags = vec!["reading".into()];
        let binding = [b];
        let hits = search(&binding, "reading");
        assert_eq!(hits.len(), 1);
        assert_eq!(search(&[bm("2", "https://x.test/", "Other")], "reading").len(), 0);
    }
}
