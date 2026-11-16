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

/// Import bookmarks from a Netscape Bookmark HTML export (Firefox/Chromium flavor).
/// Each `<DT><A ...>title</A>` line is parsed: `HREF` is required (lines without it are
/// skipped), `ADD_DATE` is read as seconds and converted to `created_at_ms`, and `TAGS`
/// (comma-separated) becomes the tag list. Ids are `import-{i}` with `i` the 0-based
/// index of the parsed bookmark. Unknown closing tags are tolerated; unknown entities
/// pass through untouched. Nothing here makes network requests.
pub fn import_netscape(export: &str) -> Vec<Bookmark> {
    let mut out = Vec::new();
    for raw_line in export.lines() {
        let line = raw_line.trim();
        let lower = line.to_ascii_lowercase();
        let Some(a_start) = lower.find("<a ") else { continue };
        let rest = &line[a_start + 3..];
        let Some(gt) = rest.find('>') else { continue };
        let attrs = &rest[..gt];
        let after = &rest[gt + 1..];

        let Some(url) = html_attr(attrs, "href") else { continue };
        if url.trim().is_empty() {
            continue;
        }
        let created_at_ms = html_attr(attrs, "add_date")
            .and_then(|v| v.trim().parse::<u64>().ok())
            .map(|secs| secs.saturating_mul(1000))
            .unwrap_or(0);
        let tags: Vec<String> = html_attr(attrs, "tags")
            .map(|v| v.split(',').map(|t| t.trim().to_string()).filter(|t| !t.is_empty()).collect())
            .unwrap_or_default();

        let lower_after = after.to_ascii_lowercase();
        let title_raw = match lower_after.find("</") {
            Some(end) => &after[..end],
            None => after,
        };
        let title = html_unescape(title_raw.trim());

        let i = out.len();
        let mut b = Bookmark::new(format!("import-{i}"), html_unescape(&url), title, created_at_ms);
        b.tags = tags;
        out.push(b);
    }
    out
}

/// Count tag occurrences across bookmarks.
pub fn tag_counts(bookmarks: &[Bookmark]) -> std::collections::BTreeMap<String, usize> {
    let mut counts = std::collections::BTreeMap::new();
    for b in bookmarks {
        for t in &b.tags {
            *counts.entry(t.clone()).or_default() += 1;
        }
    }
    counts
}

/// Case-insensitive attribute lookup in a tag's attribute string. Values may be
/// double- or single-quoted. Returns the entity-unescaped value.
fn html_attr(attrs: &str, key: &str) -> Option<String> {
    let lower = attrs.to_ascii_lowercase();
    let key = key.to_ascii_lowercase();
    let mut search_from = 0usize;
    loop {
        let rel = lower[search_from..].find(&key)?;
        let pos = search_from + rel;
        let boundary_ok = pos == 0 || matches!(attrs.as_bytes()[pos - 1], b' ' | b'\t' | b'\n' | b'\r');
        let after_key = lower[pos + key.len()..].trim_start();
        if boundary_ok && after_key.starts_with('=') {
            let value_src = &attrs[pos + key.len()..];
            let value = value_src.trim_start();
            let value = value.strip_prefix('=').unwrap_or(value).trim_start();
            let mut chars = value.chars();
            match chars.next() {
                Some(q @ ('"' | '\'')) => {
                    let body = &value[q.len_utf8()..];
                    let end = body.find(q)?;
                    return Some(html_unescape(&body[..end]));
                }
                _ => return None, // unquoted values are not accepted in bookmark exports
            }
        }
        search_from = pos + key.len();
    }
}

/// Single-pass entity unescape for the five named entities used in bookmark exports.
/// Unknown entities are left untouched (no double-unescaping).
pub fn html_unescape(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut rest = input;
    while let Some(amp) = rest.find('&') {
        out.push_str(&rest[..amp]);
        let after = &rest[amp..];
        let end = match after.find(';') {
            Some(e) => e,
            None => {
                out.push_str(after);
                return out;
            }
        };
        let entity = &after[..=end];
        match entity {
            "&amp;" => out.push('&'),
            "&lt;" => out.push('<'),
            "&gt;" => out.push('>'),
            "&quot;" => out.push('"'),
            "&#39;" => out.push('\''),
            _ => out.push_str(entity),
        }
        rest = &after[end + 1..];
    }
    out.push_str(rest);
    out
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
    fn import_netscape_parses_full_line() {
        let export = r#"<!DOCTYPE NETSCAPE-Bookmark-file-1>
<DL><p>
    <DT><A HREF="https://example.com/rust" ADD_DATE="1700000000" TAGS="rust,docs">Rust &amp; Cargo</A>
    <DT><A HREF="https://example.com/a?b=1&amp;c=2" ADD_DATE="1650000001">Tom &lt;b&gt; &quot;quoted&quot; &#39;x&#39;</A>
    <DT><A HREF="https://no-date.example.com/">No date</A>
    <DT><A ADD_DATE="1">No href — skipped</A>
</DL><p>
"#;
        let bms = import_netscape(export);
        assert_eq!(bms.len(), 3);
        assert_eq!(bms[0].id, "import-0");
        assert_eq!(bms[0].url, "https://example.com/rust");
        assert_eq!(bms[0].title, "Rust & Cargo");
        assert_eq!(bms[0].tags, vec!["rust".to_string(), "docs".to_string()]);
        assert_eq!(bms[0].created_at_ms, 1_700_000_000_000);
        assert_eq!(bms[1].title, "Tom <b> \"quoted\" 'x'");
        assert_eq!(bms[1].url, "https://example.com/a?b=1&c=2");
        assert_eq!(bms[1].created_at_ms, 1_650_000_001_000);
        assert_eq!(bms[2].created_at_ms, 0); // missing ADD_DATE → 0
        assert_eq!(bms[2].id, "import-2");
    }

    #[test]
    fn import_netscape_handles_empty_and_garbage() {
        assert!(import_netscape("").is_empty());
        assert!(import_netscape("<html>not a bookmark file</html>").is_empty());
    }

    #[test]
    fn html_unescape_single_pass() {
        assert_eq!(html_unescape("&amp;lt;"), "&lt;"); // no double unescape
        assert_eq!(html_unescape("plain"), "plain");
        assert_eq!(html_unescape("&unknown; &amp;"), "&unknown; &");
        assert_eq!(html_unescape("trailing &"), "trailing &");
    }

    #[test]
    fn tag_counts_aggregates() {
        let mut a = bm("1", "https://a.test/", "A");
        a.tags = vec!["rust".into(), "docs".into()];
        let mut b = bm("2", "https://b.test/", "B");
        b.tags = vec!["rust".into()];
        let counts = tag_counts(&[a, b, bm("3", "https://c.test/", "C")]);
        assert_eq!(counts.get("rust"), Some(&2));
        assert_eq!(counts.get("docs"), Some(&1));
        assert_eq!(counts.len(), 2);
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
