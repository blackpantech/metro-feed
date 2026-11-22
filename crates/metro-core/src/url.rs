//! Feed URL normalization for Metro.
//!
//! Pure string functions (no I/O) shared by the store, which deduplicates
//! subscriptions by normalized URL, and HTTP autodiscovery, which
//! normalizes the feed URL it finds.
//!
//! # Normalization rules
//!
//! [`normalize_url`] applies the normalization pinned by the spec:
//!
//! 1. the scheme and host are lowercased (paths and user info are
//!    case-sensitive and preserved),
//! 2. fragments (`#...`) are dropped,
//! 3. tracking query parameters are dropped (any `utm_*` key plus the
//!    well-known click IDs listed in [`TRACKING_PARAM_KEYS`]),
//! 4. remaining query parameters keep their original order and encoding,
//! 5. trailing slashes are stripped from the path (`https://example.com/`
//!    becomes `https://example.com`).
//!
//! Only absolute `http://` and `https://` URLs are accepted; anything else is
//! rejected with [`MetroError::Usage`] so the caller can report a clear
//! message. Default ports are intentionally left in place: they are harmless
//! for deduplication and removing them would diverge from the specified
//! normalization rules.

use crate::MetroError;

/// Well-known query parameter keys that identify tracking clicks rather than
/// feed content.
///
/// Any key that starts with `utm_` is treated as tracking as well. Keys are
/// compared case-insensitively. The list is deliberately conservative:
/// parameters that may influence the served feed (for example `format` or
/// `lang`) are preserved.
const TRACKING_PARAM_KEYS: &[&str] = &[
    "fbclid", "gclid", "dclid", "msclkid", "yclid", "igshid", "mc_cid", "mc_eid", "_ga", "_gl",
];

/// Parsed components of an absolute `http(s)` URL.
///
/// Borrows slices of the input string so normalization can rebuild the URL
/// without allocations per component. IPv6 hosts are stored without their
/// surrounding brackets.
struct ParsedUrl<'a> {
    /// Original scheme spelling (`http` or `https`, any casing).
    scheme: &'a str,
    /// User info (everything before the last `@`), preserved verbatim.
    userinfo: Option<&'a str>,
    /// Host without brackets, original casing.
    host: &'a str,
    /// Explicit port, if present.
    port: Option<&'a str>,
    /// Path portion including its leading slash, or an empty string.
    path: &'a str,
    /// Query portion without `?`, or `None` when there is no query.
    query: Option<&'a str>,
}

impl<'a> ParsedUrl<'a> {
    /// Parse an absolute `http(s)` URL.
    ///
    /// Fragments are ignored. Returns [`MetroError::Usage`] for empty input,
    /// unsupported schemes, missing hosts, and malformed authorities.
    fn parse(raw: &'a str) -> Result<Self, MetroError> {
        let input = raw.trim();
        if input.is_empty() {
            return Err(MetroError::Usage("feed URL must not be empty".to_string()));
        }

        let (scheme, rest) = input.split_once("://").ok_or_else(|| {
            MetroError::Usage(format!(
                "invalid feed URL {input:?}: expected an absolute http:// or https:// URL"
            ))
        })?;
        if !scheme.eq_ignore_ascii_case("http") && !scheme.eq_ignore_ascii_case("https") {
            return Err(MetroError::Usage(format!(
                "unsupported URL scheme {scheme:?}: only http and https feed URLs are supported"
            )));
        }

        // A fragment never affects the resource that is fetched.
        let rest = rest.split('#').next().unwrap_or_default();

        let authority_end = rest.find(['/', '?']).unwrap_or(rest.len());
        let authority = &rest[..authority_end];
        if authority.is_empty() {
            return Err(MetroError::Usage(format!(
                "invalid feed URL {input:?}: the URL has no host"
            )));
        }

        let remainder = &rest[authority_end..];
        let (path, query) = match remainder.split_once('?') {
            Some((path, query)) => (path, Some(query)),
            None => (remainder, None),
        };

        let (userinfo, host_port) = match authority.rsplit_once('@') {
            Some((userinfo, host_port)) => (Some(userinfo), host_port),
            None => (None, authority),
        };
        let bracketed = host_port.starts_with('[');
        let (host, port) = split_host_port(host_port)?;
        if host.is_empty() {
            return Err(MetroError::Usage(format!(
                "invalid feed URL {input:?}: the URL has no host"
            )));
        }
        if host.contains(':') && !bracketed {
            return Err(MetroError::Usage(format!(
                "invalid feed URL {input:?}: IPv6 hosts must be enclosed in brackets"
            )));
        }

        Ok(Self {
            scheme,
            userinfo,
            host,
            port,
            path,
            query,
        })
    }

    /// Rebuild the origin (`scheme://host[:port]`) with a lowercased host.
    ///
    /// User info is omitted on purpose: it is not part of a site URL.
    fn origin(&self) -> String {
        let mut origin = format!(
            "{}://{}",
            self.scheme.to_ascii_lowercase(),
            self.host_for_url()
        );
        if let Some(port) = self.port {
            origin.push(':');
            origin.push_str(port);
        }
        origin
    }

    /// The host with ASCII casing folded to lowercase.
    fn host_lowercase(&self) -> String {
        self.host.to_ascii_lowercase()
    }

    /// The lowercased host formatted for inclusion in a URL.
    ///
    /// IPv6 literals are wrapped back in brackets; because
    /// [`ParsedUrl::parse`] stores them without brackets, their presence is
    /// detected by the embedded colons.
    fn host_for_url(&self) -> String {
        let host = self.host_lowercase();
        if self.host.contains(':') {
            format!("[{host}]")
        } else {
            host
        }
    }
}

/// Split an authority's `host[:port]` portion into host and port.
///
/// Supports bracketed IPv6 literals. Rejects empty or non-numeric ports and
/// unbracketed IPv6 literals so malformed URLs are reported early.
fn split_host_port(host_port: &str) -> Result<(&str, Option<&str>), MetroError> {
    if let Some(stripped) = host_port.strip_prefix('[') {
        let Some((host, rest)) = stripped.split_once(']') else {
            return Err(MetroError::Usage(format!(
                "invalid IPv6 host in URL: {host_port:?} is missing a closing bracket"
            )));
        };
        let port = match rest {
            "" => None,
            other => match other.strip_prefix(':') {
                Some(port) if is_valid_port(port) => Some(port),
                _ => {
                    return Err(MetroError::Usage(format!(
                        "invalid port in URL authority {host_port:?}"
                    )));
                }
            },
        };
        return Ok((host, port));
    }

    match host_port.rsplit_once(':') {
        Some((host, port)) if is_valid_port(port) => Ok((host, Some(port))),
        Some(_) => Err(MetroError::Usage(format!(
            "invalid port in URL authority {host_port:?}"
        ))),
        None => Ok((host_port, None)),
    }
}

/// Return `true` when `port` is a non-empty run of ASCII digits.
fn is_valid_port(port: &str) -> bool {
    !port.is_empty() && port.bytes().all(|byte| byte.is_ascii_digit())
}

/// Return `true` when a query parameter pair is a tracking parameter.
///
/// The key is compared case-insensitively against [`TRACKING_PARAM_KEYS`] and
/// the `utm_` prefix.
fn is_tracking_param(pair: &str) -> bool {
    let key = pair.split_once('=').map_or(pair, |(key, _)| key).trim();
    let key = key.to_ascii_lowercase();
    key.starts_with("utm_") || TRACKING_PARAM_KEYS.contains(&key.as_str())
}

/// Rebuild a query string without tracking parameters, preserving order.
///
/// Empty segments (`?`, `a=1&&b=2`) and parameters that become empty after
/// filtering are dropped. An empty result means the URL has no query.
fn normalized_query(query: &str) -> String {
    query
        .split('&')
        .filter(|pair| !pair.is_empty() && !is_tracking_param(pair))
        .collect::<Vec<_>>()
        .join("&")
}

/// Normalize a feed URL for storage and duplicate detection.
///
/// The normalization rules are documented at the module level. The returned
/// string is what [`crate::store::feeds::add_feed`] stores, so comparing stored URLs to
/// [`normalize_url`] output is sufficient for deduplication.
///
/// # Errors
///
/// Returns [`MetroError::Usage`] when the input is empty, is not an absolute
/// `http(s)` URL, has no host, or has a malformed port.
#[must_use = "normalization errors must be handled before using the URL"]
pub fn normalize_url(raw: &str) -> Result<String, MetroError> {
    let parsed = ParsedUrl::parse(raw)?;

    let mut normalized = format!("{}://", parsed.scheme.to_ascii_lowercase());
    if let Some(userinfo) = parsed.userinfo {
        normalized.push_str(userinfo);
        normalized.push('@');
    }
    normalized.push_str(&parsed.host_for_url());
    if let Some(port) = parsed.port {
        normalized.push(':');
        normalized.push_str(port);
    }
    normalized.push_str(parsed.path.trim_end_matches('/'));

    let query = parsed.query.map(normalized_query).unwrap_or_default();
    if !query.is_empty() {
        normalized.push('?');
        normalized.push_str(&query);
    }

    Ok(normalized)
}

/// Lowercased host and origin of an absolute `http(s)` URL.
///
/// Used to derive placeholder metadata for a feed that has not declared its
/// own title and site URL yet.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct UrlOrigin {
    /// The URL's host with ASCII casing folded to lowercase.
    pub(crate) host: String,
    /// The URL's origin (`scheme://host[:port]`), without user info.
    pub(crate) origin: String,
}

/// Split an absolute `http(s)` URL into its lowercased host and origin.
///
/// # Errors
///
/// Returns [`MetroError::Usage`] if `raw` is not an absolute `http(s)` URL.
pub(crate) fn url_origin(raw: &str) -> Result<UrlOrigin, MetroError> {
    let parsed = ParsedUrl::parse(raw)?;
    Ok(UrlOrigin {
        host: parsed.host_lowercase(),
        origin: parsed.origin(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_normalize_url_lowercases_scheme_and_host() {
        assert_eq!(
            normalize_url("HTTPS://Example.COM/Feed.xml").unwrap(),
            "https://example.com/Feed.xml"
        );
    }

    #[test]
    fn test_normalize_url_strips_fragment_and_trailing_slashes() {
        assert_eq!(
            normalize_url("https://example.com/#section").unwrap(),
            "https://example.com"
        );
        assert_eq!(
            normalize_url("https://example.com/feed///").unwrap(),
            "https://example.com/feed"
        );
    }

    #[test]
    fn test_normalize_url_drops_tracking_params() {
        assert_eq!(
            normalize_url(
                "https://example.com/feed.xml?utm_source=rss&format=rss&fbclid=abc&utm_medium=email"
            )
            .unwrap(),
            "https://example.com/feed.xml?format=rss"
        );
    }

    #[test]
    fn test_normalize_url_preserves_remaining_query_order() {
        assert_eq!(
            normalize_url("https://example.com/feed?b=2&a=1&gclid=x").unwrap(),
            "https://example.com/feed?b=2&a=1"
        );
    }

    #[test]
    fn test_normalize_url_drops_empty_query() {
        assert_eq!(
            normalize_url("https://example.com/feed.xml?utm_source=rss").unwrap(),
            "https://example.com/feed.xml"
        );
        assert_eq!(
            normalize_url("https://example.com/feed.xml?").unwrap(),
            "https://example.com/feed.xml"
        );
    }

    #[test]
    fn test_normalize_url_preserves_port_and_userinfo() {
        assert_eq!(
            normalize_url("https://Example.com:8443/feed/").unwrap(),
            "https://example.com:8443/feed"
        );
        assert_eq!(
            normalize_url("https://User:Pass@Example.com/feed").unwrap(),
            "https://User:Pass@example.com/feed"
        );
    }

    #[test]
    fn test_normalize_url_bracketed_ipv6() {
        assert_eq!(
            normalize_url("http://[2001:DB8::1]:8080/feed/").unwrap(),
            "http://[2001:db8::1]:8080/feed"
        );
    }

    #[test]
    fn test_normalize_url_rejects_invalid_input() {
        assert!(matches!(normalize_url(""), Err(MetroError::Usage(_))));
        assert!(matches!(normalize_url("   "), Err(MetroError::Usage(_))));
        assert!(matches!(
            normalize_url("example.com/feed"),
            Err(MetroError::Usage(_))
        ));
        assert!(matches!(
            normalize_url("ftp://example.com/feed"),
            Err(MetroError::Usage(_))
        ));
        assert!(matches!(
            normalize_url("https://"),
            Err(MetroError::Usage(_))
        ));
        assert!(matches!(
            normalize_url("https://example.com:abc/feed"),
            Err(MetroError::Usage(_))
        ));
        assert!(matches!(
            normalize_url("https://2001:db8::1/feed"),
            Err(MetroError::Usage(_))
        ));
    }

    #[test]
    fn test_url_origin_lowercases_host_and_drops_userinfo() {
        assert_eq!(
            url_origin("https://user@Example.COM:8443/feed.xml?a=1").unwrap(),
            UrlOrigin {
                host: "example.com".to_string(),
                origin: "https://example.com:8443".to_string(),
            }
        );
    }
}
