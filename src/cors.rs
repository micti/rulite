//! CORS, so a browser app on another origin can call Rulite with supabase-js:
//! preflights are answered before routing and auth, other responses from an
//! allowed origin get the CORS headers.

use std::sync::Arc;

use axum::extract::{Request, State};
use axum::http::{HeaderMap, HeaderName, HeaderValue, Method, StatusCode, header};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};

const METHODS: &str = "GET, HEAD, POST, PATCH, DELETE, OPTIONS";
const EXPOSE: &str = "Content-Range, Content-Location, Location, Range-Unit";
const MAX_AGE: &str = "86400";

/// The origins allowed to call the API.
#[derive(Debug, Clone, PartialEq)]
pub enum Origins {
    /// `*`: any origin.
    Any,
    /// Exact origins (`scheme://host[:port]`), lowercase.
    List(Vec<String>),
}

/// What the CORS layer does with a request.
#[derive(Debug, PartialEq)]
pub enum Cors {
    /// Not a CORS request, or the origin isn't allowed: the request goes on unchanged.
    Pass,
    /// A preflight: answer `204` with these headers.
    Preflight(Vec<(HeaderName, HeaderValue)>),
    /// Run the request and add these headers to its response.
    Response(Vec<(HeaderName, HeaderValue)>),
}

impl Origins {
    /// `*`, or comma-separated origins.
    pub fn parse(list: &str) -> Result<Origins, String> {
        Origins::from_items(list.split(','))
    }

    /// `*` alone, or origins; each one checked.
    pub fn from_items<'a>(items: impl IntoIterator<Item = &'a str>) -> Result<Origins, String> {
        let items: Vec<&str> = items.into_iter().map(str::trim).collect();
        if items == ["*"] {
            return Ok(Origins::Any);
        }
        let mut list = Vec::with_capacity(items.len());
        for item in items {
            if !valid_origin(item) {
                return Err(format!("invalid cors origin {item:?} (expected scheme://host[:port] or *)"));
            }
            list.push(item.to_ascii_lowercase());
        }
        Ok(Origins::List(list))
    }

    fn allows(&self, origin: &str) -> bool {
        match self {
            Origins::Any => true,
            Origins::List(list) => list.iter().any(|o| o.eq_ignore_ascii_case(origin)),
        }
    }
}

/// `http(s)://host[:port]`: no path, query, user or trailing slash.
fn valid_origin(origin: &str) -> bool {
    let Some((scheme, rest)) = origin.split_once("://") else {
        return false;
    };
    let v6 = rest.starts_with('[');
    let (host, port) = match rest.strip_prefix('[').and_then(|r| r.split_once(']')) {
        Some(split) => split,
        None if v6 => return false,
        None => rest.split_at(rest.find(':').unwrap_or(rest.len())),
    };
    let host_ok =
        !host.is_empty() && host.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '.' || (v6 && c == ':'));
    let port_ok = port.is_empty()
        || port.strip_prefix(':').is_some_and(|p| p.bytes().all(|b| b.is_ascii_digit()) && p.parse::<u16>().is_ok());
    (scheme.eq_ignore_ascii_case("http") || scheme.eq_ignore_ascii_case("https")) && host_ok && port_ok
}

/// The CORS answer for a request with `method` and `headers` (pure, no I/O).
pub fn decide(origins: &Origins, method: &Method, headers: &HeaderMap) -> Cors {
    let Some(origin) = headers.get(header::ORIGIN).and_then(|v| v.to_str().ok()) else {
        return Cors::Pass;
    };
    if !origins.allows(origin) {
        return Cors::Pass;
    }
    let mut out = Vec::with_capacity(5);
    match origins {
        Origins::Any => out.push((header::ACCESS_CONTROL_ALLOW_ORIGIN, HeaderValue::from_static("*"))),
        Origins::List(_) => {
            let echoed = HeaderValue::from_str(origin).expect("came from a header value");
            out.push((header::ACCESS_CONTROL_ALLOW_ORIGIN, echoed));
            out.push((header::VARY, HeaderValue::from_static("Origin")));
        }
    }
    if method == Method::OPTIONS && headers.contains_key(header::ACCESS_CONTROL_REQUEST_METHOD) {
        out.push((header::ACCESS_CONTROL_ALLOW_METHODS, HeaderValue::from_static(METHODS)));
        if let Some(requested) = headers.get(header::ACCESS_CONTROL_REQUEST_HEADERS) {
            out.push((header::ACCESS_CONTROL_ALLOW_HEADERS, requested.clone()));
        }
        out.push((header::ACCESS_CONTROL_MAX_AGE, HeaderValue::from_static(MAX_AGE)));
        return Cors::Preflight(out);
    }
    out.push((header::ACCESS_CONTROL_EXPOSE_HEADERS, HeaderValue::from_static(EXPOSE)));
    Cors::Response(out)
}

/// The middleware: every route, the fallback and errors included.
pub async fn layer(State(origins): State<Arc<Origins>>, request: Request, next: Next) -> Response {
    match decide(&origins, request.method(), request.headers()) {
        Cors::Pass => next.run(request).await,
        Cors::Preflight(headers) => {
            let mut r = StatusCode::NO_CONTENT.into_response();
            r.headers_mut().extend(headers);
            r
        }
        Cors::Response(headers) => {
            let mut r = next.run(request).await;
            for (name, value) in headers {
                r.headers_mut().append(name, value);
            }
            r
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request(pairs: &[(&str, &str)]) -> HeaderMap {
        pairs.iter().map(|(k, v)| (k.parse().unwrap(), v.parse().unwrap())).collect()
    }

    fn get<'a>(cors: &'a Cors, name: &str) -> Option<&'a str> {
        let (Cors::Preflight(h) | Cors::Response(h)) = cors else { return None };
        h.iter().find(|(n, _)| n == name).map(|(_, v)| v.to_str().unwrap())
    }

    #[test]
    fn lists() {
        assert_eq!(Origins::parse("*"), Ok(Origins::Any));
        assert_eq!(Origins::parse(" * "), Ok(Origins::Any));
        assert_eq!(
            Origins::parse("http://localhost:5173, https://App.example.com"),
            Ok(Origins::List(vec!["http://localhost:5173".into(), "https://app.example.com".into()]))
        );
        assert_eq!(Origins::parse("http://[::1]:8080").unwrap(), Origins::List(vec!["http://[::1]:8080".into()]));
        assert!(Origins::parse("https://[::1]").is_ok());
        for bad in [
            "",
            "localhost:5173",
            "ftp://x.dev",
            "http://",
            "http://x.dev/",
            "http://x.dev/app",
            "http://x.dev:",
            "http://x.dev:99999",
            "http://x.dev:80a",
            "http://user@x.dev",
            "http://x.dev?q",
            "http://x.dev,*",
            "http://[::1",
            "http://[::1]x",
            "*,*",
        ] {
            let e = Origins::parse(bad).unwrap_err();
            assert!(e.starts_with("invalid cors origin"), "{bad}: {e}");
        }
    }

    #[test]
    fn any_origin() {
        let cors = decide(&Origins::Any, &Method::GET, &request(&[("origin", "https://a.dev")]));
        assert_eq!(get(&cors, "access-control-allow-origin"), Some("*"));
        assert_eq!(get(&cors, "vary"), None);
        assert_eq!(get(&cors, "access-control-expose-headers"), Some(EXPOSE));
        assert_eq!(get(&cors, "access-control-allow-credentials"), None);
    }

    #[test]
    fn listed_origin_is_echoed() {
        let origins = Origins::parse("https://a.dev,http://localhost:5173").unwrap();
        let cors = decide(&origins, &Method::POST, &request(&[("origin", "http://localhost:5173")]));
        assert!(matches!(cors, Cors::Response(_)));
        assert_eq!(get(&cors, "access-control-allow-origin"), Some("http://localhost:5173"));
        assert_eq!(get(&cors, "vary"), Some("Origin"));
        assert_eq!(get(&cors, "access-control-expose-headers"), Some(EXPOSE));
        assert_eq!(get(&cors, "access-control-allow-methods"), None);
    }

    #[test]
    fn preflight() {
        let origins = Origins::parse("https://a.dev").unwrap();
        let headers = request(&[
            ("origin", "https://a.dev"),
            ("access-control-request-method", "PATCH"),
            ("access-control-request-headers", "apikey, authorization, x-client-info, prefer, range"),
        ]);
        let cors = decide(&origins, &Method::OPTIONS, &headers);
        assert!(matches!(cors, Cors::Preflight(_)));
        assert_eq!(get(&cors, "access-control-allow-origin"), Some("https://a.dev"));
        assert_eq!(get(&cors, "access-control-allow-methods"), Some(METHODS));
        assert_eq!(
            get(&cors, "access-control-allow-headers"),
            Some("apikey, authorization, x-client-info, prefer, range")
        );
        assert_eq!(get(&cors, "access-control-max-age"), Some("86400"));
        assert_eq!(get(&cors, "access-control-expose-headers"), None);

        // OPTIONS without Access-Control-Request-Method is a plain request.
        let plain = decide(&origins, &Method::OPTIONS, &request(&[("origin", "https://a.dev")]));
        assert!(matches!(plain, Cors::Response(_)));
        let no_headers = decide(
            &Origins::Any,
            &Method::OPTIONS,
            &request(&[("origin", "https://a.dev"), ("access-control-request-method", "GET")]),
        );
        assert_eq!(get(&no_headers, "access-control-allow-headers"), None);
    }

    #[test]
    fn disallowed_or_missing_origin() {
        let origins = Origins::parse("https://a.dev").unwrap();
        let preflight = [("origin", "https://evil.dev"), ("access-control-request-method", "GET")];
        assert_eq!(decide(&origins, &Method::OPTIONS, &request(&preflight)), Cors::Pass);
        assert_eq!(decide(&origins, &Method::GET, &request(&[("origin", "https://a.dev.evil")])), Cors::Pass);
        assert_eq!(decide(&origins, &Method::GET, &request(&[])), Cors::Pass);
        assert_eq!(decide(&Origins::Any, &Method::GET, &request(&[])), Cors::Pass);
        assert!(matches!(decide(&origins, &Method::GET, &request(&[("origin", "HTTPS://A.DEV")])), Cors::Response(_)));
    }
}
