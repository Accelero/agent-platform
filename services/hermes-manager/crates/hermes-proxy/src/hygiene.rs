//! Header and cookie hygiene between browsers and native Hermes. Nothing a
//! browser sends can assert an identity or a route, and nothing an instance
//! returns can set a platform cookie.

use axum::http::header::{self, HeaderMap, HeaderName, HeaderValue};
use hermes_identity::browser::BrowserAuth;
use hermes_identity::crypto::Purpose;

/// Native Hermes's own session cookies, which a platform logout expires.
pub const NATIVE_COOKIES: [&str; 5] = [
    "hermes_session_at",
    "hermes_session_rt",
    "hermes_session_provider",
    "hermes_session_pkce",
    "hermes_sso_attempt",
];
/// Native Hermes names a cookie with one of these prefixes, depending on how
/// it was reached.
const NATIVE_PREFIXES: [&str; 3] = ["__Host-", "__Secure-", ""];

const HOP_BY_HOP: [&str; 8] = [
    "connection",
    "keep-alive",
    "proxy-connection",
    "proxy-authenticate",
    "proxy-authorization",
    "te",
    "trailer",
    "transfer-encoding",
];

/// Request headers that could carry routing or identity claims.
fn reserved(name: &str) -> bool {
    matches!(
        name,
        "forwarded" | "x-real-ip" | "remote-user" | "upgrade" | "host"
    ) || [
        "x-forwarded-",
        "x-hermes-",
        "x-auth-request-",
        "x-remote-",
        "x-original-",
    ]
    .iter()
    .any(|prefix| name.starts_with(prefix))
}

/// Any spelling of a platform cookie name. Matching is case-insensitive so a
/// variant can neither be smuggled in nor set by an instance.
fn platform_name(name: &str) -> bool {
    name.trim().to_ascii_lowercase().contains("hermes-platform")
}

/// Why a request's platform cookies cannot be used.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Ambiguous;

/// The session and browser-proof cookie values. A duplicated platform cookie,
/// in one header or across several, is refused.
pub fn platform_cookies(headers: &HeaderMap) -> Result<Option<(String, String)>, Ambiguous> {
    let (session_name, browser_name) = BrowserAuth::cookies(Purpose::Gateway);
    let (mut session, mut browser) = (None, None);
    for value in headers.get_all(header::COOKIE) {
        let value = value.to_str().map_err(|_| Ambiguous)?;
        for item in value.split(';') {
            let Some((name, value)) = item.trim().split_once('=') else {
                continue;
            };
            let slot = if name == session_name {
                &mut session
            } else if name == browser_name {
                &mut browser
            } else if platform_name(name) {
                return Err(Ambiguous);
            } else {
                continue;
            };
            if slot.replace(value.to_owned()).is_some() {
                return Err(Ambiguous);
            }
        }
    }
    Ok(session.zip(browser))
}

/// The headers forwarded to an instance: without hop-by-hop headers, the
/// headers `Connection` names, reserved headers and platform cookies. `Host`
/// is the public host; an upgrade keeps exactly its WebSocket handshake.
pub fn forward_request(headers: &HeaderMap, public_host: &str, websocket: bool) -> HeaderMap {
    let named: Vec<String> = connection_tokens(headers);
    let mut forwarded = HeaderMap::new();
    let mut cookies = Vec::new();
    for (name, value) in headers {
        let lower = name.as_str();
        if HOP_BY_HOP.contains(&lower) || reserved(lower) || named.iter().any(|n| n == lower) {
            continue;
        }
        if name == header::COOKIE {
            let Ok(value) = value.to_str() else {
                continue;
            };
            cookies.extend(
                value
                    .split(';')
                    .map(str::trim)
                    .filter(|item| !item.is_empty())
                    .filter(|item| !platform_name(item.split('=').next().unwrap_or(item)))
                    .map(str::to_owned),
            );
            continue;
        }
        forwarded.append(name, value.clone());
    }
    if !cookies.is_empty()
        && let Ok(value) = HeaderValue::from_str(&cookies.join("; "))
    {
        forwarded.insert(header::COOKIE, value);
    }
    if let Ok(host) = HeaderValue::from_str(public_host) {
        forwarded.insert(header::HOST, host.clone());
        forwarded.insert(HeaderName::from_static("x-forwarded-host"), host);
    }
    forwarded.insert(
        HeaderName::from_static("x-forwarded-proto"),
        HeaderValue::from_static("https"),
    );
    if websocket {
        forwarded.insert(header::CONNECTION, HeaderValue::from_static("upgrade"));
        forwarded.insert(header::UPGRADE, HeaderValue::from_static("websocket"));
    }
    forwarded
}

/// Removes hop-by-hop headers and every `Set-Cookie` naming a platform
/// cookie from an instance's response. A switching response keeps its
/// handshake.
pub fn filter_response(headers: &mut HeaderMap, switching: bool) {
    for name in connection_tokens(headers) {
        if let Ok(name) = HeaderName::from_bytes(name.as_bytes()) {
            headers.remove(name);
        }
    }
    for name in HOP_BY_HOP {
        headers.remove(name);
    }
    if !switching {
        headers.remove(header::UPGRADE);
    }
    let cookies: Vec<HeaderValue> = headers
        .get_all(header::SET_COOKIE)
        .iter()
        .filter(|value| {
            value
                .to_str()
                .is_ok_and(|text| !platform_name(text.split(['=', ';']).next().unwrap_or(text)))
        })
        .cloned()
        .collect();
    headers.remove(header::SET_COOKIE);
    for value in cookies {
        headers.append(header::SET_COOKIE, value);
    }
    if switching {
        headers.insert(header::CONNECTION, HeaderValue::from_static("upgrade"));
        headers.insert(header::UPGRADE, HeaderValue::from_static("websocket"));
    }
}

/// Appends `Set-Cookie` headers expiring the platform cookies and every
/// variant of native Hermes's cookies.
pub fn expire_cookies(headers: &mut HeaderMap) {
    let (session, browser) = BrowserAuth::cookies(Purpose::Gateway);
    let names =
        [session.to_owned(), browser.to_owned()]
            .into_iter()
            .chain(NATIVE_COOKIES.iter().flat_map(|name| {
                NATIVE_PREFIXES
                    .iter()
                    .map(move |prefix| format!("{prefix}{name}"))
            }));
    for name in names {
        let value = format!("{name}=; Path=/; Secure; HttpOnly; SameSite=Lax; Max-Age=0");
        if let Ok(value) = HeaderValue::from_str(&value) {
            headers.append(header::SET_COOKIE, value);
        }
    }
}

fn connection_tokens(headers: &HeaderMap) -> Vec<String> {
    headers
        .get_all(header::CONNECTION)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .flat_map(|value| value.split(','))
        .map(|token| token.trim().to_ascii_lowercase())
        .filter(|token| !token.is_empty() && token != "upgrade" && token != "close")
        .collect()
}

/// A WebSocket handshake: `Connection: upgrade` and `Upgrade: websocket`.
pub fn is_websocket(headers: &HeaderMap) -> bool {
    let upgrade = headers
        .get(header::UPGRADE)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value.eq_ignore_ascii_case("websocket"));
    let connection = headers
        .get_all(header::CONNECTION)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .flat_map(|value| value.split(','))
        .any(|token| token.trim().eq_ignore_ascii_case("upgrade"));
    upgrade && connection
}

#[cfg(test)]
mod tests {
    use super::*;

    fn headers(pairs: &[(&str, &str)]) -> HeaderMap {
        let mut map = HeaderMap::new();
        for (name, value) in pairs {
            map.append(
                HeaderName::from_bytes(name.as_bytes()).unwrap(),
                HeaderValue::from_str(value).unwrap(),
            );
        }
        map
    }

    const SESSION: &str = "__Host-hermes-platform";
    const BROWSER: &str = "__Host-hermes-platform-browser";

    #[test]
    fn forged_identity_and_routing_headers_never_reach_an_instance() {
        let incoming = headers(&[
            ("host", "attacker.example.org"),
            ("x-forwarded-host", "kube-system"),
            ("x-forwarded-prefix", "/other"),
            ("x-forwarded-for", "10.0.0.1"),
            ("forwarded", "for=10.0.0.1;host=evil"),
            ("x-real-ip", "10.0.0.1"),
            ("x-hermes-verified-subject", "owner-b"),
            ("x-hermes-instance", "foreign"),
            ("x-auth-request-email", "b@example.org"),
            ("x-remote-user", "owner-b"),
            ("remote-user", "owner-b"),
            ("x-original-uri", "/admin"),
            ("connection", "keep-alive, x-secret"),
            ("x-secret", "hidden by connection"),
            ("te", "trailers"),
            ("transfer-encoding", "chunked"),
            ("upgrade", "h2c"),
            ("authorization", "Bearer native-token"),
            ("accept", "text/html"),
            (
                "cookie",
                &format!("{SESSION}=a; hermes_session_at=native; {BROWSER}=b"),
            ),
            ("cookie", "__HOST-Hermes-Platform=smuggled; theme=dark"),
        ]);
        let forwarded = forward_request(&incoming, "hermes.example.org", false);
        let mut names: Vec<_> = forwarded.keys().map(|k| k.as_str().to_owned()).collect();
        names.sort();
        assert_eq!(
            names,
            [
                "accept",
                "authorization",
                "cookie",
                "host",
                "x-forwarded-host",
                "x-forwarded-proto"
            ],
        );
        assert_eq!(forwarded["host"], "hermes.example.org");
        assert_eq!(forwarded["x-forwarded-host"], "hermes.example.org");
        assert_eq!(forwarded["x-forwarded-proto"], "https");
        assert_eq!(forwarded["authorization"], "Bearer native-token");
        assert_eq!(forwarded["cookie"], "hermes_session_at=native; theme=dark");
    }

    #[test]
    fn websocket_handshakes_keep_only_their_upgrade() {
        let incoming = headers(&[
            ("connection", "Upgrade"),
            ("upgrade", "websocket"),
            ("sec-websocket-key", "dGhlIHNhbXBsZSBub25jZQ=="),
            ("sec-websocket-version", "13"),
        ]);
        assert!(is_websocket(&incoming));
        let forwarded = forward_request(&incoming, "hermes.example.org", true);
        assert_eq!(forwarded["connection"], "upgrade");
        assert_eq!(forwarded["upgrade"], "websocket");
        assert_eq!(forwarded["sec-websocket-version"], "13");
        assert!(!is_websocket(&headers(&[("upgrade", "websocket")])));
        assert!(!is_websocket(&headers(&[
            ("connection", "upgrade"),
            ("upgrade", "h2c")
        ])));
    }

    #[test]
    fn platform_cookies_are_read_exactly_once() {
        let one = headers(&[("cookie", &format!("{SESSION}=s; x=1; {BROWSER}=b"))]);
        assert_eq!(platform_cookies(&one), Ok(Some(("s".into(), "b".into()))));
        assert_eq!(
            platform_cookies(&headers(&[("cookie", &format!("{SESSION}=s"))])),
            Ok(None)
        );
        for ambiguous in [
            headers(&[("cookie", &format!("{SESSION}=s; {SESSION}=t; {BROWSER}=b"))]),
            headers(&[
                ("cookie", &format!("{SESSION}=s; {BROWSER}=b")),
                ("cookie", &format!("{BROWSER}=c")),
            ]),
            headers(&[(
                "cookie",
                &format!("{SESSION}=s; {BROWSER}=b; __host-hermes-platform=t"),
            )]),
        ] {
            assert_eq!(platform_cookies(&ambiguous), Err(Ambiguous));
        }
    }

    #[test]
    fn instances_cannot_set_platform_cookies() {
        let mut response = headers(&[
            ("set-cookie", "__Host-hermes_session_at=x; Path=/; Secure"),
            ("set-cookie", &format!("{SESSION}=forged; Path=/")),
            ("set-cookie", "__Host-Hermes-Platform-Browser=forged"),
            ("set-cookie", "hermes-platform=forged"),
            ("connection", "close, x-internal"),
            ("x-internal", "1"),
            ("keep-alive", "timeout=5"),
            ("upgrade", "websocket"),
            ("content-type", "text/html"),
        ]);
        filter_response(&mut response, false);
        let cookies: Vec<_> = response.get_all("set-cookie").iter().collect();
        assert_eq!(cookies, ["__Host-hermes_session_at=x; Path=/; Secure"]);
        for removed in ["connection", "x-internal", "keep-alive", "upgrade"] {
            assert!(!response.contains_key(removed), "{removed}");
        }
        assert_eq!(response["content-type"], "text/html");
    }

    #[test]
    fn logout_expires_platform_and_every_native_cookie_variant() {
        let mut response = HeaderMap::new();
        expire_cookies(&mut response);
        let names: Vec<String> = response
            .get_all("set-cookie")
            .iter()
            .map(|v| {
                let text = v.to_str().unwrap();
                assert!(text.ends_with("; Path=/; Secure; HttpOnly; SameSite=Lax; Max-Age=0"));
                text.split('=').next().unwrap().to_owned()
            })
            .collect();
        assert_eq!(names.len(), 2 + 15);
        for expected in [
            SESSION,
            BROWSER,
            "__Host-hermes_session_at",
            "__Secure-hermes_session_rt",
            "hermes_session_provider",
            "__Host-hermes_session_pkce",
            "hermes_sso_attempt",
        ] {
            assert!(names.iter().any(|n| n == expected), "{expected}");
        }
    }
}
