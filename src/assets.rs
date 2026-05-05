use axum::{
    body::Body,
    http::{HeaderValue, header},
    response::Response,
};

pub struct Asset {
    pub content_type: &'static str,
    pub bytes: &'static [u8],
}

static INDEX_HTML: &[u8] = include_bytes!("../web/index.html");
static APP_JS: &[u8] = include_bytes!("../web/app.js");
static STYLES_CSS: &[u8] = include_bytes!("../web/styles.css");

pub fn asset(path: &str) -> Option<Asset> {
    // Lead integration hint:
    // - Serve "/" with `asset("/")`.
    // - Serve "/app.js" + "/styles.css" via a fallback `asset(req.uri().path())`.
    match path {
        "" | "/" | "/index.html" => Some(Asset {
            content_type: "text/html; charset=utf-8",
            bytes: INDEX_HTML,
        }),
        "app.js" | "/app.js" => Some(Asset {
            content_type: "application/javascript; charset=utf-8",
            bytes: APP_JS,
        }),
        "styles.css" | "/styles.css" => Some(Asset {
            content_type: "text/css; charset=utf-8",
            bytes: STYLES_CSS,
        }),
        _ => None,
    }
}

pub fn response(path: &str) -> Option<Response> {
    let a = asset(path)?;
    let mut res = Response::new(Body::from(a.bytes));
    let headers = res.headers_mut();
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static(a.content_type),
    );
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-cache"));
    headers.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    Some(res)
}
