//! Targets accepted by the user-initiated external-link action.
const MAX_URL_BYTES: usize = 8192;

/// Normalize a web address without letting message text launch arbitrary shell handlers.
/// Explicit Telegram and email links retain their protocol; bare domains use HTTPS.
pub fn external_url(target: &str) -> Option<String> {
    if target.is_empty()
        || target.len() > MAX_URL_BYTES
        || target
            .chars()
            .any(|c| c.is_control() || c.is_whitespace() || c == '\\')
    {
        return None;
    }
    let url = match target.split_once(':') {
        Some((scheme, rest))
            if ["http", "https", "tg"]
                .iter()
                .any(|s| s.eq_ignore_ascii_case(scheme)) =>
        {
            let authority = rest.strip_prefix("//")?;
            if authority.starts_with('/') {
                return None;
            }
            url::Url::parse(target)
        }
        Some((scheme, _)) if scheme.eq_ignore_ascii_case("mailto") => url::Url::parse(target),
        _ => {
            let address = target.strip_prefix("//").unwrap_or(target);
            let authority = address.split(['/', '?', '#']).next()?;
            if authority.is_empty() {
                return None;
            }
            // A colon in a bare authority denotes a port or an IPv6 literal, never a scheme.
            if !authority.starts_with('[')
                && let Some((host, port)) = authority.split_once(':')
                && ((address == target
                    && !host.contains('.')
                    && !host.eq_ignore_ascii_case("localhost"))
                    || port.is_empty()
                    || !port.bytes().all(|b| b.is_ascii_digit()))
            {
                return None;
            }
            url::Url::parse(&format!("https://{address}"))
        }
    };
    let url = url.ok()?;
    if url.as_str().len() > MAX_URL_BYTES {
        return None;
    }
    match url.scheme() {
        "http" | "https" | "tg" if url.host_str().is_some_and(|host| !host.is_empty()) => {
            Some(url.into())
        }
        "mailto" if url.cannot_be_a_base() && !url.path().is_empty() => Some(url.into()),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn web_addresses_and_explicit_protocols() {
        for (input, expected) in [
            (
                "example.org/path?q=1#part",
                "https://example.org/path?q=1#part",
            ),
            ("http://example.org/a", "http://example.org/a"),
            ("HTTPS://example.org", "https://example.org/"),
            ("//example.org/a", "https://example.org/a"),
            ("example.org:8080/a", "https://example.org:8080/a"),
            ("example.org:8080?q=1", "https://example.org:8080/?q=1"),
            ("example.org/a:b?q=x:y", "https://example.org/a:b?q=x:y"),
            ("[::1]:8080/a", "https://[::1]:8080/a"),
            ("//server:8080/a", "https://server:8080/a"),
            (
                "tg://resolve?domain=telegram",
                "tg://resolve?domain=telegram",
            ),
            ("mailto:hello@example.org", "mailto:hello@example.org"),
        ] {
            assert_eq!(external_url(input).as_deref(), Some(expected), "{input}");
        }
    }
    #[test]
    fn normalization_is_idempotent_and_encoded_length_is_bounded() {
        let normalized = external_url("https://example.org/привет").unwrap();
        assert_eq!(external_url(&normalized), Some(normalized));
        assert_eq!(
            external_url(&format!("https://example.org/{}", "я".repeat(2000))),
            None
        );
    }

    #[test]
    fn shell_paths_unknown_schemes_and_malformed_targets_are_rejected() {
        for input in [
            "",
            "javascript:alert(1)",
            "javascript:123",
            "ftp://example.org/a",
            "file:///C:/test.exe",
            "data:text/html,test",
            "shell:AppsFolder",
            "C:\\test.exe",
            "\\\\server\\test",
            "https://",
            "https:example.org",
            "https:///example.org",
            "/relative/path",
            "https://a\0b",
            "https://a\nb",
            "https://example.org/a b",
            "tg:",
            "mailto:",
        ] {
            assert_eq!(external_url(input), None, "{input}");
        }
    }
}
