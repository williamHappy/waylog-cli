use ::url::{form_urlencoded, Url};
use serde::Deserialize;
use std::sync::OnceLock;

const REDACTED: &str = "REDACTED";

/// Explicit, exact URL scope for parameters whose values may be archived.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct UrlRule {
    origin: String,
    path: String,
    #[serde(default)]
    fragment_path: Option<String>,
    params: Vec<String>,
}

/// Retain ordinary parameters and hide known credentials and authorization codes.
/// Invalid URLs are returned unchanged. Rules are loaded once per process.
pub fn sanitize_url(raw: &str) -> String {
    static RULES: OnceLock<Vec<UrlRule>> = OnceLock::new();
    sanitize_with_rules(raw, RULES.get_or_init(load_rules))
}

fn load_rules() -> Vec<UrlRule> {
    let Some(path) = std::env::var_os("WAYLOG_BROWSER_URL_RULES") else {
        return Vec::new();
    };
    read_rules(std::path::Path::new(&path))
}

fn read_rules(path: &std::path::Path) -> Vec<UrlRule> {
    let rules = std::fs::read(path)
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok());
    rules.unwrap_or_else(|| {
        // Do not include file contents or parser errors, which may expose values.
        tracing::warn!(
            "Browser URL rules could not be loaded; using default sensitive-parameter rules"
        );
        Vec::new()
    })
}

fn sanitize_with_rules(raw: &str, rules: &[UrlRule]) -> String {
    sanitize_at_depth(raw, rules, 0)
}

fn sanitize_at_depth(raw: &str, rules: &[UrlRule], depth: usize) -> String {
    let Ok(mut parsed) = Url::parse(raw) else {
        return raw.to_string();
    };
    let end = raw.find(['?', '#']).unwrap_or(raw.len());
    let mut result = raw[..end].to_string();

    // URL userinfo is another credential location; normalize only when removing it.
    if !parsed.username().is_empty() || parsed.password().is_some() {
        let _ = parsed.set_username("");
        let _ = parsed.set_password(None);
        let mut base = parsed.clone();
        base.set_query(None);
        base.set_fragment(None);
        result = base.to_string();
    }

    if let Some(query) = parsed.query() {
        result.push('?');
        result.push_str(&sanitize_params(query, &parsed, None, rules, depth));
    }
    if let Some(fragment) = parsed.fragment() {
        result.push('#');
        if fragment.starts_with('/') || fragment.starts_with("!/") {
            let (route, query) = fragment
                .split_once('?')
                .map_or((fragment, None), |(route, query)| (route, Some(query)));
            result.push_str(route);
            if let Some(query) = query {
                result.push('?');
                result.push_str(&sanitize_params(query, &parsed, Some(route), rules, depth));
            }
        } else if fragment.contains('=') {
            result.push_str(&sanitize_params(fragment, &parsed, Some(""), rules, depth));
        } else if !fragment.is_empty() {
            result.push_str(fragment);
        }
    }
    result
}

fn sanitize_params(
    raw: &str,
    url: &Url,
    route: Option<&str>,
    rules: &[UrlRule],
    depth: usize,
) -> String {
    let origin = url.origin().ascii_serialization();
    raw.split('&')
        .map(|part| {
            let Some((key, value)) = form_urlencoded::parse(part.as_bytes()).next() else {
                return part.to_string();
            };
            let code_exception = rules.iter().any(|rule| {
                origin != "null"
                    && rule.origin == origin
                    && rule.path == url.path()
                    && rule.fragment_path.as_deref() == route
                    && rule.params.iter().any(|param| param == key.as_ref())
            });
            let redact = sensitive_key(&key)
                || (key.eq_ignore_ascii_case("code")
                    && (sensitive_path(url.path())
                        || route.is_some_and(sensitive_path)
                        || !code_exception));
            if redact {
                let raw_key = part.split_once('=').map_or(part, |(key, _)| key);
                return format!("{raw_key}={REDACTED}");
            }
            // Preserve ordinary parameters byte-for-byte, including flags and encoding.
            if part.contains('=') {
                if let Some(nested) = sanitize_nested(&value, rules, depth) {
                    if nested != value {
                        let encoded = form_urlencoded::Serializer::new(String::new())
                            .append_pair(&key, &nested)
                            .finish();
                        return encoded;
                    }
                }
            }
            part.to_string()
        })
        .collect::<Vec<_>>()
        .join("&")
}

fn sanitize_nested(value: &str, rules: &[UrlRule], depth: usize) -> Option<String> {
    let lowercase = value.to_ascii_lowercase();
    let absolute = lowercase.starts_with("https://") || lowercase.starts_with("http://");
    let network_path = value.starts_with("//");
    let relative = value.starts_with('/') && !value.starts_with("//") && value.contains(['?', '#']);
    if !absolute && !relative && !network_path {
        // Some redirect parameters encode the nested address more than once.
        if depth < 4 && value.contains('%') && !value.contains('&') {
            let wrapped = format!("v={value}");
            let (_, decoded) = form_urlencoded::parse(wrapped.as_bytes()).next()?;
            if decoded != value {
                let sanitized = sanitize_nested(&decoded, rules, depth + 1)?;
                if sanitized != decoded {
                    let encoded = form_urlencoded::Serializer::new(String::new())
                        .append_pair("v", &sanitized)
                        .finish();
                    return Some(encoded[2..].to_string());
                }
            }
        }
        return None;
    }
    if depth >= 4 {
        return Some(REDACTED.to_string());
    }
    if network_path {
        let sanitized = sanitize_at_depth(&format!("https:{value}"), rules, depth + 1);
        Some(
            sanitized
                .strip_prefix("https:")
                .unwrap_or(REDACTED)
                .to_string(),
        )
    } else if absolute {
        Some(sanitize_at_depth(value, rules, depth + 1))
    } else {
        let sanitized =
            sanitize_at_depth(&format!("https://waylog.invalid{value}"), rules, depth + 1);
        Some(
            sanitized
                .strip_prefix("https://waylog.invalid")
                .unwrap_or(REDACTED)
                .to_string(),
        )
    }
}

fn sensitive_key(key: &str) -> bool {
    let key = key.to_ascii_lowercase().replace(['-', '_'], "");
    [
        "token",
        "ticket",
        "secret",
        "password",
        "passwd",
        "credential",
        "authorization",
        "cookie",
        "session",
        "signature",
        "apikey",
        "clientkey",
    ]
    .iter()
    .any(|marker| key.contains(marker))
        || matches!(
            key.as_str(),
            "auth"
                | "jwt"
                | "sig"
                | "key"
                | "state"
                | "nonce"
                | "samlresponse"
                | "samlrequest"
                | "assertion"
        )
}

fn sensitive_path(path: &str) -> bool {
    let path = path.to_ascii_lowercase();
    [
        "oauth",
        "callback",
        "login",
        "signin",
        "authorize",
        "/sso",
        "/cas",
    ]
    .iter()
    .any(|marker| path.contains(marker))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rules() -> Vec<UrlRule> {
        serde_json::from_str(
            r#"[
            {"origin":"https://shop.example","path":"/product","params":["code","access_token"]},
            {"origin":"https://shop.example","path":"/oauth/callback","params":["code"]}
        ]"#,
        )
        .unwrap()
    }

    #[test]
    fn ordinary_parameters_are_preserved_without_configuration() {
        for raw in [
            "https://a.com/product?id=123&page=2&sort=time&q=Rust&custom=abc",
            "https://a.com/?flag&x=&x=abc&&q=a%20b&z=%2f",
            "https://a.com/#/orders?page=2",
            "https://a.com/#section-2",
            "https://a.com/#page=2",
            "HTTPS://Example.COM:443/a/../b%3Fc%23d",
            "file:///tmp/page.html",
        ] {
            assert_eq!(sanitize_with_rules(raw, &[]), raw);
        }
    }

    #[test]
    fn mixed_sensitive_and_ordinary_parameters_are_handled_in_url_and_fragment() {
        for (raw, expected) in [
            (
                "https://a.com/product?id=123&page=2&access_token=secret",
                "https://a.com/product?id=123&page=2&access_token=REDACTED",
            ),
            (
                "https://a.com/callback?code=abc&state=xyz&page=2",
                "https://a.com/callback?code=REDACTED&state=REDACTED&page=2",
            ),
            (
                "https://a.com/#/orders?page=2&refresh_token=secret",
                "https://a.com/#/orders?page=2&refresh_token=REDACTED",
            ),
            (
                "https://a.com/#id_token=secret&lang=zh",
                "https://a.com/#id_token=REDACTED&lang=zh",
            ),
            (
                "https://a.com/?_casTicket=secret&id=1&API-KEY=secret&password=secret",
                "https://a.com/?_casTicket=REDACTED&id=1&API-KEY=REDACTED&password=REDACTED",
            ),
            (
                "https://a.com/?%61ccess%5ftoken=secret&x=abc",
                "https://a.com/?%61ccess%5ftoken=REDACTED&x=abc",
            ),
        ] {
            let sanitized = sanitize_with_rules(raw, &[]);
            assert_eq!(sanitized, expected);
            assert_eq!(sanitize_with_rules(&sanitized, &[]), sanitized);
        }
    }

    #[test]
    fn code_exceptions_are_exact_and_cannot_release_known_credentials_or_callback_codes() {
        let rules = rules();
        assert_eq!(
            sanitize_with_rules(
                "https://shop.example/product?code=SKU123&page=2&access_token=secret",
                &rules
            ),
            "https://shop.example/product?code=SKU123&page=2&access_token=REDACTED"
        );
        for raw in [
            "https://shop.example/other?code=abc",
            "https://shop.example.evil/product?code=abc",
            "http://shop.example/product?code=abc",
            "https://shop.example:8443/product?code=abc",
            "https://shop.example/oauth/callback?code=abc",
        ] {
            assert!(!sanitize_with_rules(raw, &rules).contains("=abc"));
        }
    }

    #[test]
    fn nested_urls_preserve_ordinary_values_and_redact_credentials() {
        for (raw, expected) in [
            (
                "https://a.com/?next=https%3A%2F%2Fb.com%2F%3Fpage%3D2%26token%3Dsecret",
                "https://a.com/?next=https%3A%2F%2Fb.com%2F%3Fpage%3D2%26token%3DREDACTED",
            ),
            (
                "https://a.com/?next=%2Fcallback%3Fcode%3Dsecret%26page%3D2",
                "https://a.com/?next=%2Fcallback%3Fcode%3DREDACTED%26page%3D2",
            ),
            (
                "https://a.com/?next=%2Forders%3Fpage%3D2",
                "https://a.com/?next=%2Forders%3Fpage%3D2",
            ),
            (
                "https://a.com/?next=https%253A%252F%252Fb.com%252F%253Ftoken%253Dsecret",
                "https://a.com/?next=https%253A%252F%252Fb.com%252F%253Ftoken%253DREDACTED",
            ),
            (
                "https://a.com/?next=%2F%2Fb.com%2F%3Fpage%3D2%26token%3Dsecret",
                "https://a.com/?next=%2F%2Fb.com%2F%3Fpage%3D2%26token%3DREDACTED",
            ),
        ] {
            assert_eq!(sanitize_with_rules(raw, &[]), expected);
        }
        let mut nested = "https://a.com/?token=secret".to_string();
        for _ in 0..7 {
            nested = format!(
                "https://a.com/?{}",
                form_urlencoded::Serializer::new(String::new())
                    .append_pair("next", &nested)
                    .finish()
            );
        }
        let sanitized = sanitize_with_rules(&nested, &[]);
        assert!(!sanitized.contains("secret"));
        assert_eq!(sanitize_with_rules(&sanitized, &[]), sanitized);
    }

    #[test]
    fn config_failures_do_not_hide_ordinary_parameters_or_release_code() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("rules.json");
        assert!(read_rules(&path).is_empty());
        std::fs::write(&path, include_str!("../../browser-url-rules.example.json")).unwrap();
        assert_eq!(
            sanitize_with_rules(
                "https://shop.example/product?code=SKU123&page=2",
                &read_rules(&path)
            ),
            "https://shop.example/product?code=SKU123&page=2"
        );
        std::fs::write(&path, "not JSON").unwrap();
        assert_eq!(
            sanitize_with_rules(
                "https://shop.example/product?code=secret&page=2",
                &read_rules(&path)
            ),
            "https://shop.example/product?code=REDACTED&page=2"
        );
    }

    #[test]
    fn removes_userinfo_and_preserves_invalid_urls() {
        assert_eq!(
            sanitize_with_rules("https://user:secret@a.com/x?page=2", &[]),
            "https://a.com/x?page=2"
        );
        for raw in [
            "",
            "not a URL?code=abc#f",
            "/relative?code=abc#f",
            "https://[invalid?code=abc",
        ] {
            assert_eq!(sanitize_with_rules(raw, &[]), raw);
        }
    }
}
