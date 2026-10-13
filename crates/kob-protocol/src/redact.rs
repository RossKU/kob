//! Node and indexer URLs as they may be shown or logged (the executor's logs, health and metrics routes, the `kob` CLI's
//! messages and reports): a URL can carry a password or an API key, which never leaves the process.

/// A node URL as it may be shown or logged: `scheme://host[:port]`. User name and password, path, query and fragment are
/// left out (they can hold credentials or API keys: `wss://user:pass@host/key?apikey=...`); a removed path or query is
/// marked with `/...`. A value without a scheme keeps its host part only. A password written with an unencoded `?`, `#` or `/`
/// does not leak: the user information ends at the last `@` before the first `/`, and a "host" that is not `host[:port]` (the
/// part of a credential before an unencoded `/`) is shown as `<redacted>`.
pub fn redact_url(url: &str) -> String {
    let (scheme, rest) = match url.split_once("://") {
        Some((s, r)) => (Some(s), r),
        None => (None, url),
    };
    let (host, tail) = url_host(rest);
    let host = if host_like(host) { host } else { "<redacted>" };
    let marker = if tail.is_empty() || tail == "/" { "" } else { "/..." };
    match scheme {
        Some(s) => format!("{s}://{host}{marker}"),
        None => format!("{host}{marker}"),
    }
}

/// The `host[:port]` of a URL without its scheme, and what follows it: the user information ends at the last `@` before the first
/// `/` (a `?` or `#` before that `/` can belong to an unencoded password), the host at the first `?` or `#` after it.
pub fn url_host(rest: &str) -> (&str, &str) {
    let seg_end = rest.find('/').unwrap_or(rest.len());
    let start = rest[..seg_end].rfind('@').map_or(0, |i| i + 1);
    let after = &rest[start..];
    let end = after.find(['/', '?', '#']).unwrap_or(after.len());
    (&after[..end], &after[end..])
}

/// `name[:port]` or `[v6][:port]` with a numeric port: anything else is not a host.
fn host_like(h: &str) -> bool {
    let (name, port) = if let Some(v6) = h.strip_prefix('[') {
        match v6.split_once(']') {
            Some((addr, p)) if !addr.is_empty() && addr.chars().all(|c| c.is_ascii_hexdigit() || c == ':' || c == '.') => ("v6", p),
            _ => return false,
        }
    } else {
        match h.rfind(':') {
            Some(i) => (&h[..i], &h[i..]),
            None => (h, ""),
        }
    };
    let port_ok = port.is_empty() || port.strip_prefix(':').is_some_and(|p| !p.is_empty() && p.chars().all(|c| c.is_ascii_digit()));
    !name.is_empty() && name.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '.' | '_')) && port_ok
}

/// `text` with every occurrence of `url` replaced by its [`redact_url`] form (an error message of a library that echoes the
/// URL it was given).
pub fn scrub_url(text: &str, url: &str) -> String {
    if url.is_empty() {
        return text.to_string();
    }
    text.replace(url, &redact_url(url))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scrubbing_replaces_the_url_wherever_it_is_echoed() {
        let url = "ws://alice:s3cret@127.0.0.1:1/key/K?apikey=A";
        let msg = format!("URL error: Unable to connect to {url}");
        let s = scrub_url(&msg, url);
        assert_eq!(s, "URL error: Unable to connect to ws://127.0.0.1:1/...");
        assert!(!s.contains("s3cret") && !s.contains("apikey"));
        assert_eq!(scrub_url("no url here", url), "no url here");
        assert_eq!(scrub_url("x", ""), "x");
    }
}
