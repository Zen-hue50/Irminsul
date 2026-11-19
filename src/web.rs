use crate::{
    config::{Paths, validate_goal, write_new},
    tracker::{Range, Report, Shared, lock},
};
use anyhow::Result;
use axum::{
    Json, Router,
    body::Body,
    extract::{DefaultBodyLimit, Query, Request, State},
    http::{HeaderMap, HeaderValue, Method, StatusCode, header},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::get,
};
use chrono::{Local, NaiveDate};
use serde::Deserialize;
use serde_json::json;
use std::{
    fs,
    io::Read,
    path::{Path, PathBuf},
    sync::Arc,
};
use tokio::sync::Notify;

const INDEX: &[u8] = include_bytes!("../web/index.html");
const STYLE: &[u8] = include_bytes!("../web/styles.css");
const PALETTES: &[u8] = include_bytes!("../web/palettes.css");
const JS: &[u8] = include_bytes!("../web/app.js");
const THEME: &[u8] = include_bytes!("../web/theme.css");
const FAVICON: &[u8] = include_bytes!("../web/irminsul.svg");
const DM_SANS: &[u8] = include_bytes!("../web/fonts/dm-sans.woff2");
const FRAUNCES: &[u8] = include_bytes!("../web/fonts/fraunces.woff2");
const MAX_ASSET_BYTES: u64 = 4 * 1024 * 1024;

#[derive(Clone)]
struct Web {
    core: Shared,
    overrides: Option<PathBuf>,
    preview: bool,
    port: u16,
    probe: Arc<Notify>,
}

pub fn router(
    core: Shared,
    overrides: Option<PathBuf>,
    preview: bool,
    port: u16,
    probe: Arc<Notify>,
) -> Router {
    let state = Web {
        core,
        overrides,
        preview,
        port,
        probe,
    };
    Router::new()
        .route("/api/v1/report", get(report))
        .route("/api/v1/export", get(export))
        .route("/api/v1/preferences", axum::routing::post(preferences))
        .route("/api/v1/health", get(health))
        .fallback(asset)
        .layer(DefaultBodyLimit::max(2048))
        .layer(middleware::from_fn_with_state(state.clone(), local_only))
        .with_state(state)
}

async fn local_only(State(state): State<Web>, request: Request, next: Next) -> Response {
    if !state.preview {
        let host = request
            .headers()
            .get(header::HOST)
            .and_then(|v| v.to_str().ok())
            .unwrap_or_default();
        if host != format!("127.0.0.1:{}", state.port)
            && !host.eq_ignore_ascii_case(&format!("localhost:{}", state.port))
        {
            return error(
                StatusCode::FORBIDDEN,
                "Use the dashboard's localhost address.",
            );
        }
        if request.method() == Method::POST {
            let origin = request
                .headers()
                .get(header::ORIGIN)
                .and_then(|v| v.to_str().ok());
            if origin != Some(format!("http://{host}").as_str()) {
                return error(StatusCode::FORBIDDEN, "A same-origin request is required.");
            }
        }
    }
    let mut response = next.run(request).await;
    let headers = response.headers_mut();
    headers.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    headers.insert(
        header::REFERRER_POLICY,
        HeaderValue::from_static("no-referrer"),
    );
    headers
        .entry(header::CACHE_CONTROL)
        .or_insert(HeaderValue::from_static("no-store"));
    let csp = if state.preview {
        "default-src 'self'; object-src 'none'; base-uri 'none'; form-action 'self'; style-src 'self' 'unsafe-inline'; img-src 'self' data:; worker-src 'none'"
    } else {
        "default-src 'self'; object-src 'none'; base-uri 'none'; form-action 'self'; style-src 'self' 'unsafe-inline'; img-src 'self' data:; worker-src 'none'; frame-ancestors 'none'"
    };
    headers.insert(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_static(csp),
    );
    if !state.preview {
        headers.insert(header::X_FRAME_OPTIONS, HeaderValue::from_static("DENY"));
    }
    response
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Selection {
    #[serde(default = "default_days")]
    days: u32,
    end: Option<String>,
}
fn default_days() -> u32 {
    7
}

fn make_report(
    web: &Web,
    selection: Selection,
) -> std::result::Result<Report, (StatusCode, String)> {
    let end = selection
        .end
        .as_deref()
        .map(|s| {
            let date = NaiveDate::parse_from_str(s, "%Y-%m-%d")
                .map_err(|_| (StatusCode::BAD_REQUEST, "end must be YYYY-MM-DD".into()))?;
            if date.to_string() != s {
                return Err((StatusCode::BAD_REQUEST, "end must be YYYY-MM-DD".into()));
            }
            Ok(date)
        })
        .transpose()?;
    Range::new(selection.days, end, Local::now().date_naive())
        .map_err(|e| (StatusCode::BAD_REQUEST, e.to_string()))?;
    lock(&web.core).report(selection.days, end).map_err(|e| {
        eprintln!("irminsul: report failed: {e:#}");
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            "Could not read usage history.".into(),
        )
    })
}

async fn report(State(web): State<Web>, Query(selection): Query<Selection>) -> Response {
    match make_report(&web, selection) {
        Ok(r) => Json(r).into_response(),
        Err((status, detail)) => error(status, &detail),
    }
}

async fn health(State(web): State<Web>) -> Response {
    let c = lock(&web.core);
    Json(json!({ "version": env!("CARGO_PKG_VERSION"), "demo": c.demo, "session": c.session() }))
        .into_response()
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Preference {
    paused: Option<bool>,
    goal_minutes: Option<u16>,
}

async fn preferences(State(web): State<Web>, Json(p): Json<Preference>) -> Response {
    if p.paused.is_none() && p.goal_minutes.is_none() {
        return error(StatusCode::BAD_REQUEST, "No preferences provided.");
    }
    if p.paused.is_some() && p.goal_minutes.is_some() {
        return error(StatusCode::BAD_REQUEST, "Send one preference at a time.");
    }
    if let Some(goal) = p.goal_minutes {
        if let Err(e) = validate_goal(goal) {
            return error(StatusCode::BAD_REQUEST, &e.to_string());
        }
    }
    let mut core = lock(&web.core);
    if let Err(e) = core.set_preferences(p.paused, p.goal_minutes) {
        eprintln!("irminsul: could not save preference: {e:#}");
        return error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "Could not save that preference.",
        );
    }
    if p.paused.is_some() {
        web.probe.notify_one();
    }
    Json(json!({ "ok": true, "session": core.session() })).into_response()
}

async fn export(State(web): State<Web>, Query(selection): Query<Selection>) -> Response {
    match make_report(&web, selection) {
        Ok(r) => {
            let mut csv =
                String::from("date,application,app_id,category,active_seconds,app_switches\r\n");
            for (i, day) in r.daily.iter().enumerate() {
                for app in &r.apps {
                    let ms = app.daily[i];
                    let switches = app.daily_switches[i];
                    if ms > 0 || switches > 0 {
                        csv.push_str(&format!(
                            "{},{},{},{},{},{}\r\n",
                            day.date,
                            csv_text(&app.name),
                            csv_text(&app.id),
                            csv_text(&app.category),
                            seconds(ms),
                            switches
                        ));
                    }
                }
                if r.unattributed_daily_ms[i] > 0 {
                    csv.push_str(&format!(
                        "{},Desktop & excluded,,Desktop & excluded,{},0\r\n",
                        day.date,
                        seconds(r.unattributed_daily_ms[i])
                    ));
                }
            }
            let filename = format!(
                "attachment; filename=\"{}-{}-{}.csv\"",
                if r.demo { "irminsul-demo" } else { "irminsul" },
                r.start,
                r.end
            );
            (
                [
                    (
                        header::CONTENT_TYPE,
                        HeaderValue::from_static("text/csv; charset=utf-8"),
                    ),
                    (
                        header::CONTENT_DISPOSITION,
                        HeaderValue::from_str(&filename).unwrap(),
                    ),
                ],
                csv,
            )
                .into_response()
        }
        Err((status, detail)) => error(status, &detail),
    }
}

fn seconds(ms: u64) -> String {
    format!("{}.{:03}", ms / 1000, ms % 1000)
}
fn csv_text(text: &str) -> String {
    let injection = text
        .trim_start()
        .starts_with(['=', '-', '+', '@', '＝', '－', '＋', '＠'])
        || text.starts_with(['\t', '\r', '\n']);
    format!(
        "\"{}{}\"",
        if injection { "'" } else { "" },
        text.replace('"', "\"\"")
    )
}
fn error(status: StatusCode, msg: &str) -> Response {
    (status, Json(json!({ "error": msg }))).into_response()
}

pub fn theme_init(paths: &Paths) -> Result<()> {
    let web = paths.web();
    fs::create_dir_all(&web)?;
    for (file, data) in [
        ("index.html", INDEX),
        ("styles.css", STYLE),
        ("palettes.css", PALETTES),
        ("app.js", JS),
        ("theme.css", THEME),
        ("irminsul.svg", FAVICON),
    ] {
        let path = web.join(file);
        let created = write_new(&path, data)?;
        println!(
            "{:<10} {}",
            if created { "created" } else { "kept" },
            path.display()
        );
    }
    Ok(())
}

async fn asset(State(web): State<Web>, request: Request) -> Response {
    if !matches!(*request.method(), Method::GET | Method::HEAD) {
        return StatusCode::METHOD_NOT_ALLOWED.into_response();
    }
    let path = request.uri().path();
    let (file, mime, fallback, cache) = match path {
        "/" | "/index.html" => ("index.html", "text/html; charset=utf-8", Some(INDEX), false),
        "/styles.css" => ("styles.css", "text/css; charset=utf-8", Some(STYLE), false),
        "/app.js" => ("app.js", "text/javascript; charset=utf-8", Some(JS), false),
        "/palettes.css" => (
            "palettes.css",
            "text/css; charset=utf-8",
            Some(PALETTES),
            false,
        ),
        "/theme.css" => ("theme.css", "text/css; charset=utf-8", Some(THEME), false),
        "/irminsul.svg" => ("irminsul.svg", "image/svg+xml", Some(FAVICON), false),
        "/fonts/dm-sans.woff2" => ("fonts/dm-sans.woff2", "font/woff2", Some(DM_SANS), true),
        "/fonts/fraunces.woff2" => ("fonts/fraunces.woff2", "font/woff2", Some(FRAUNCES), true),
        _ => match extra_mime(path) {
            Some(mime) => (&path[1..], mime, None, false),
            None => return StatusCode::NOT_FOUND.into_response(),
        },
    };
    let custom = web
        .overrides
        .as_deref()
        .and_then(|root| local_asset(root, file));
    let mut headers = HeaderMap::new();
    headers.insert(header::CONTENT_TYPE, HeaderValue::from_static(mime));
    let body = match (custom, fallback) {
        (Some(bytes), _) => Body::from(bytes),
        (None, Some(bytes)) => {
            if cache {
                headers.insert(
                    header::CACHE_CONTROL,
                    HeaderValue::from_static("public, max-age=604800"),
                );
            }
            Body::from(bytes)
        }
        _ => return StatusCode::NOT_FOUND.into_response(),
    };
    if request.method() == Method::HEAD {
        (headers, Body::empty()).into_response()
    } else {
        (headers, body).into_response()
    }
}

fn extra_mime(path: &str) -> Option<&'static str> {
    let file = path.strip_prefix("/extras/")?;
    let (name, ext) = file.rsplit_once('.')?;
    if name.is_empty()
        || !name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-'))
    {
        return None;
    }
    Some(match ext {
        "woff2" => "font/woff2",
        "svg" => "image/svg+xml",
        "png" => "image/png",
        "webp" => "image/webp",
        "jpg" | "jpeg" => "image/jpeg",
        "css" => "text/css; charset=utf-8",
        "js" => "text/javascript; charset=utf-8",
        _ => return None,
    })
}

fn local_asset(root: &Path, file: &str) -> Option<Vec<u8>> {
    let path = root.join(file);
    let mt = fs::metadata(&path).ok()?;
    if !mt.is_file() || mt.len() > MAX_ASSET_BYTES {
        return None;
    }
    let canonical = fs::canonicalize(&path).ok()?;
    if !canonical.starts_with(fs::canonicalize(root).ok()?) {
        return None;
    }
    let mut buf = Vec::with_capacity(mt.len() as usize);
    fs::File::open(canonical)
        .ok()?
        .take(MAX_ASSET_BYTES + 1)
        .read_to_end(&mut buf)
        .ok()?;
    (buf.len() as u64 <= MAX_ASSET_BYTES).then_some(buf)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn csv_escapes_quotes_and_neutralizes_formula_cells() {
        assert_eq!(csv_text("  =2+2"), "\"'  =2+2\"");
        assert_eq!(csv_text("A,\"B\""), "\"A,\"\"B\"\"\"");
    }
    #[test]
    fn theme_extras_cannot_walk_paths() {
        assert!(extra_mime("/extras/icon-1.svg").is_some());
        for p in [
            "/extras/../clock.svg",
            "/extras/%2e%2e.svg",
            "/extras/private.sqlite3",
            "/extras/sub/themes.css",
        ] {
            assert!(extra_mime(p).is_none());
        }
    }
}
