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

/// Hide parameter values by default, retaining approved values only in exact scopes.
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
        tracing::warn!("Browser URL rules could not be loaded; hiding all parameter values");
        Vec::new()
    })
}

fn sanitize_with_rules(raw: &str, rules: &[UrlRule]) -> String {
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
        result.push_str(&sanitize_params(query, &parsed, None, rules));
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
                result.push_str(&sanitize_params(query, &parsed, Some(route), rules));
            }
        } else if fragment.contains('=') {
            result.push_str(&sanitize_params(fragment, &parsed, Some(""), rules));
        } else if !fragment.is_empty() {
            // Unknown fragments can be bearer credentials even without a key.
            result.push_str(REDACTED);
        }
    }
    result
}

fn sanitize_params(raw: &str, url: &Url, route: Option<&str>, rules: &[UrlRule]) -> String {
    let origin = url.origin().ascii_serialization();
    let mut serializer = form_urlencoded::Serializer::new(String::new());
    for part in raw.split('&').filter(|part| !part.is_empty()) {
        if !part.contains('=') {
            // A bare query component could itself be a credential, not a key.
            serializer.append_pair(REDACTED, REDACTED);
            continue;
        }
        let Some((key, value)) = form_urlencoded::parse(part.as_bytes()).next() else {
            continue;
        };
        let approved = !sensitive_key(&key)
            && !sensitive_path(url.path())
            && !route.is_some_and(sensitive_path)
            && Url::parse(&value).is_err()
            && !value.contains("://")
            && rules.iter().any(|rule| {
                origin != "null"
                    && rule.origin == origin
                    && rule.path == url.path()
                    && rule.fragment_path.as_deref() == route
                    && rule.params.iter().any(|param| param == key.as_ref())
            });
        serializer.append_pair(&key, if approved { &value } else { REDACTED });
    }
    serializer.finish()
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
                | "next"
                | "url"
                | "redirect"
                | "redirecturi"
                | "redirecturl"
                | "returnurl"
                | "returnto"
                | "continue"
                | "destination"
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
        serde_json::from_str(r#"[
            {"origin":"https://shop.example","path":"/product","params":["code","page","next","access_token","state"]},
            {"origin":"https://shop.example","path":"/oauth/callback","params":["code","page"]},
            {"origin":"https://shop.example","path":"/","fragment_path":"/orders","params":["page","access_token"]}
        ]"#).unwrap()
    }

    #[test]
    fn loads_rules_from_file_and_fails_closed_on_invalid_configuration() {
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
        for invalid in [
            "not JSON",
            r#"[{"origin":"https://shop.example","path":"/product","params":["code"],"typo":true}]"#,
        ] {
            std::fs::write(&path, invalid).unwrap();
            assert_eq!(
                sanitize_with_rules(
                    "https://shop.example/product?code=secret",
                    &read_rules(&path)
                ),
                "https://shop.example/product?code=REDACTED"
            );
        }
    }

    #[test]
    fn redacts_all_unapproved_values_and_nested_urls() {
        let raw = "https://a.com/x?code=abc&refresh_token=abc&access_token=abc&id_token=abc&_casTicket=abc&custom=abc&next=https%3A%2F%2Fb.com%2F%3Ftoken%3Dabc";
        assert_eq!(sanitize_with_rules(raw, &[]), "https://a.com/x?code=REDACTED&refresh_token=REDACTED&access_token=REDACTED&id_token=REDACTED&_casTicket=REDACTED&custom=REDACTED&next=REDACTED");
        assert_eq!(
            sanitize_with_rules("https://a.com/?opaque-secret&x=&x=abc", &[]),
            "https://a.com/?REDACTED=REDACTED&x=REDACTED&x=REDACTED"
        );
    }

    #[test]
    fn approves_only_exact_scopes_and_never_explicit_credentials() {
        let rules = rules();
        assert_eq!(sanitize_with_rules("https://shop.example/product?code=SKU123&page=2&custom=abc&access_token=abc&state=abc&next=https%3A%2F%2Fa.com", &rules), "https://shop.example/product?code=SKU123&page=2&custom=REDACTED&access_token=REDACTED&state=REDACTED&next=REDACTED");
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
    fn handles_encoded_keys_duplicates_and_is_idempotent() {
        let raw = "https://shop.example/product?%61ccess%5ftoken=abc&code=A&code=B#id_token=abc";
        let sanitized = sanitize_with_rules(raw, &rules());
        assert_eq!(
            sanitized,
            "https://shop.example/product?access_token=REDACTED&code=A&code=B#id_token=REDACTED"
        );
        assert_eq!(sanitize_with_rules(&sanitized, &rules()), sanitized);
    }

    #[test]
    fn nested_redirect_values_stay_hidden_even_if_allowlisted() {
        for value in [
            "/callback?code=secret",
            "https%3A%2F%2Fa.com",
            "https%253A%252F%252Fa.com",
        ] {
            let raw = format!("https://shop.example/product?next={value}");
            assert_eq!(
                sanitize_with_rules(&raw, &rules()),
                "https://shop.example/product?next=REDACTED"
            );
        }
    }

    #[test]
    fn handles_fragments_and_scopes_route_parameters_separately() {
        let rules = rules();
        for (raw, expected) in [
            (
                "https://shop.example/#/orders?page=2&access_token=abc",
                "https://shop.example/#/orders?page=2&access_token=REDACTED",
            ),
            (
                "https://shop.example/#/other?page=2",
                "https://shop.example/#/other?page=REDACTED",
            ),
            (
                "https://shop.example/?page=2",
                "https://shop.example/?page=REDACTED",
            ),
            (
                "https://shop.example/#access_token=abc&state=xyz",
                "https://shop.example/#access_token=REDACTED&state=REDACTED",
            ),
            (
                "https://shop.example/#opaque-secret",
                "https://shop.example/#REDACTED",
            ),
        ] {
            assert_eq!(sanitize_with_rules(raw, &rules), expected);
        }
    }

    #[test]
    fn removes_userinfo_but_preserves_ordinary_base_urls() {
        assert_eq!(
            sanitize_with_rules("https://user:secret@a.com/x?code=abc", &[]),
            "https://a.com/x?code=REDACTED"
        );
        for raw in [
            "https://a.com",
            "HTTPS://Example.COM:443/a/../b%3Fc%23d",
            "http://[::1]:8080/a",
            "chrome://settings/",
            "file:///tmp/page.html",
        ] {
            assert_eq!(sanitize_with_rules(raw, &[]), raw);
        }
    }

    #[test]
    fn preserves_invalid_urls() {
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
