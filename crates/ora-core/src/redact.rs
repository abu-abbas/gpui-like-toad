//! Keeping secrets out of text that is shown, logged or copied into a bug
//! report: passwords inside URLs (`http://user:pass@proxy:8080`) and a
//! known secret that an error message happens to quote.

const MASK: &str = "***";

/// Replace the password of every `scheme://user:password@host` in `text`
/// with `***`. Text without credentials comes back unchanged.
pub fn mask_url_credentials(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(i) = rest.find("://") {
        let start = i + 3;
        out.push_str(&rest[..start]);
        let after = &rest[start..];
        // The authority ends at whitespace, a quote or the first slash.
        let end = after
            .find(|c: char| c.is_whitespace() || matches!(c, '/' | '"' | '\'' | '<' | '>'))
            .unwrap_or(after.len());
        let authority = &after[..end];
        match authority.rfind('@') {
            Some(at) => {
                let userinfo = &authority[..at];
                match userinfo.find(':') {
                    Some(colon) => {
                        out.push_str(&userinfo[..=colon]);
                        out.push_str(MASK);
                    }
                    None => out.push_str(userinfo),
                }
                out.push_str(&authority[at..]);
            }
            None => out.push_str(authority),
        }
        rest = &after[end..];
    }
    out.push_str(rest);
    out
}

/// Replace every occurrence of `secret` in `text` with `***`. An empty
/// secret changes nothing.
pub fn scrub_secret(text: &str, secret: &str) -> String {
    if secret.is_empty() {
        text.to_string()
    } else {
        text.replace(secret, MASK)
    }
}

/// Split a proxy URL into the URL without its password and the password.
/// `http://bob:pw@host:8080` gives (`http://bob@host:8080`, `pw`). A URL
/// without a password, or without credentials at all, is returned as is.
pub fn split_proxy_password(url: &str) -> (String, Option<String>) {
    let url = url.trim();
    let start = url.find("://").map_or(0, |i| i + 3);
    let after = &url[start..];
    let end = after.find(['/', '?', '#']).unwrap_or(after.len());
    let authority = &after[..end];
    let Some(at) = authority.rfind('@') else {
        return (url.to_string(), None);
    };
    let userinfo = &authority[..at];
    let Some(colon) = userinfo.find(':') else {
        return (url.to_string(), None);
    };
    let password = &userinfo[colon + 1..];
    let clean = format!(
        "{}{}{}",
        &url[..start],
        &userinfo[..colon],
        &url[start + at..]
    );
    (clean, Some(password.to_string()).filter(|p| !p.is_empty()))
}

/// Put `password` back into a proxy URL that names a user. A URL without a
/// user, or one that already carries a password, is returned unchanged.
pub fn join_proxy_password(url: &str, password: &str) -> String {
    let url = url.trim();
    let start = url.find("://").map_or(0, |i| i + 3);
    let after = &url[start..];
    let end = after.find(['/', '?', '#']).unwrap_or(after.len());
    let authority = &after[..end];
    match authority.rfind('@') {
        Some(at) if !authority[..at].contains(':') && !password.is_empty() => {
            format!("{}:{}{}", &url[..start + at], password, &url[start + at..])
        }
        _ => url.to_string(),
    }
}

/// Whether the URL names a user (so a saved password may belong to it).
pub fn proxy_has_user(url: &str) -> bool {
    let url = url.trim();
    let start = url.find("://").map_or(0, |i| i + 3);
    let after = &url[start..];
    let end = after.find(['/', '?', '#']).unwrap_or(after.len());
    after[..end].contains('@')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn masks_the_password_in_urls_but_keeps_the_rest() {
        assert_eq!(
            mask_url_credentials("Invalid proxy (http://bob:s3cret@proxy:8080): bad"),
            "Invalid proxy (http://bob:***@proxy:8080): bad"
        );
        assert_eq!(
            mask_url_credentials("GET https://u:p@a.example/x and http://plain:80/y"),
            "GET https://u:***@a.example/x and http://plain:80/y"
        );
        assert_eq!(
            mask_url_credentials("http://bob@proxy:8080"),
            "http://bob@proxy:8080"
        );
        assert_eq!(mask_url_credentials("no url here"), "no url here");
        // An '@' in the password: the last one separates the host.
        assert_eq!(
            mask_url_credentials("http://bob:p@ss@proxy:8080/"),
            "http://bob:***@proxy:8080/"
        );
    }

    #[test]
    fn scrub_removes_a_quoted_secret() {
        assert_eq!(
            scrub_secret("rejected hunter2 twice: hunter2", "hunter2"),
            "rejected *** twice: ***"
        );
        assert_eq!(scrub_secret("keep", ""), "keep");
    }

    #[test]
    fn proxy_password_splits_and_joins() {
        let (clean, pw) = split_proxy_password("http://bob:s3cret@proxy:8080");
        assert_eq!(clean, "http://bob@proxy:8080");
        assert_eq!(pw.as_deref(), Some("s3cret"));
        assert_eq!(
            join_proxy_password(&clean, "s3cret"),
            "http://bob:s3cret@proxy:8080"
        );
        assert_eq!(
            split_proxy_password("proxy:8080"),
            ("proxy:8080".to_string(), None)
        );
        assert_eq!(
            split_proxy_password("http://bob@proxy:8080"),
            ("http://bob@proxy:8080".to_string(), None)
        );
        let (clean, pw) = split_proxy_password("bob:pw@proxy:3128");
        assert_eq!(
            (clean.as_str(), pw.as_deref()),
            ("bob@proxy:3128", Some("pw"))
        );
        assert_eq!(
            join_proxy_password("http://proxy:8080", "pw"),
            "http://proxy:8080"
        );
        assert!(proxy_has_user("http://bob@proxy"));
        assert!(!proxy_has_user("http://proxy:8080/path@x"));
    }
}
