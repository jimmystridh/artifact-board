use crate::{
    auth::{Auth, Config, error},
    core::{AppError, Core, Principal},
    ingest, ui,
};
use axum::{
    Json, Router,
    body::Bytes,
    extract::Request,
    extract::{DefaultBodyLimit, Path, Query, State},
    http::{HeaderMap, StatusCode, header},
    middleware::{self, Next},
    response::{Html, IntoResponse, Redirect, Response},
    routing::{get, post},
};
use base64::{Engine, engine::general_purpose::STANDARD};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, HashMap},
    path::PathBuf,
    sync::Arc,
};
pub struct App {
    pub core: crate::store::Store,
    pub auth: Auth,
    pub staging: PathBuf,
}
pub type Shared = Arc<App>;
impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        (
            StatusCode::from_u16(self.status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR),
            Json(json!({"error":{"code":self.code,"message":self.message}})),
        )
            .into_response()
    }
}
pub fn app(db: &std::path::Path, staging: PathBuf, config: Config) -> Result<Shared, AppError> {
    std::fs::create_dir_all(&staging)
        .map_err(|_| error(500, "storage_error", "Cannot initialize staging."))?;
    Ok(Arc::new(App {
        core: crate::store::Store::Local(Core::open(db)?),
        auth: Auth::new(config),
        staging,
    }))
}
pub fn cloud_app(store: crate::cloud_store::CloudStore, config: Config) -> Shared {
    Arc::new(App {
        core: crate::store::Store::Firestore(crate::firestore_core::FirestoreCore::new(store)),
        auth: Auth::new(config),
        staging: PathBuf::new(),
    })
}
pub fn router(state: Shared) -> Router {
    Router::new()
        .route("/", get(dashboard))
        .route("/login", get(login))
        .route("/dev/login", post(dev_login))
        .route("/auth/login", get(auth_login))
        .route("/auth/callback", get(auth_callback))
        .route("/logout", post(logout))
        .route("/health", get(health))
        .route("/assets/{name}", get(asset))
        .route("/api/shares", get(list).post(create))
        .route("/api/shares/{id}", get(detail))
        .route("/api/shares/{id}/notifications", get(notification_status))
        .route("/api/shares/{id}/uploads", post(new_upload))
        .route("/api/uploads/{id}", get(upload_status).put(upload))
        .route("/api/uploads/{id}/finalize", post(finalize))
        .route("/api/build-jobs/{id}", get(build_status))
        .route("/api/shares/{id}/lifecycle", post(lifecycle))
        .route("/api/shares/{id}/threads/{thread}/resolve", post(resolve))
        .route("/s/{cap}", get(view))
        .route("/content/{cap}/{revision}/{*path}", get(content))
        .route(
            "/frame/{cap}/{revision}/{ticket}/{*path}",
            get(frame_content),
        )
        .route("/api/view/{cap}/threads", get(threads).post(add_thread))
        .route("/api/view/{cap}/threads/{thread}/replies", post(reply))
        .route("/mcp", post(mcp))
        .layer(DefaultBodyLimit::max(48 * 1024 * 1024))
        .layer(middleware::from_fn(security))
        .with_state(state)
}
async fn security(req: Request, next: Next) -> Response {
    let mut out = next.run(req).await;
    let h = out.headers_mut();
    h.insert(header::CACHE_CONTROL, "no-store".parse().unwrap());
    h.insert(header::REFERRER_POLICY, "no-referrer".parse().unwrap());
    h.insert("x-content-type-options", "nosniff".parse().unwrap());
    h.insert(
        "x-robots-tag",
        "noindex, nofollow, noarchive".parse().unwrap(),
    );
    h.entry(header::CONTENT_SECURITY_POLICY).or_insert("default-src 'self'; script-src 'self'; style-src 'self' 'unsafe-inline'; img-src 'self' data:; frame-src 'self'; object-src 'none'; base-uri 'none'; form-action 'self'; frame-ancestors 'none'".parse().unwrap());
    out
}
async fn health(State(s): State<Shared>) -> Json<Value> {
    Json(
        json!({"status":"ok","identity":s.auth.status(),"build_mode":std::env::var("ARTIFACT_BUILD_MODE").unwrap_or_else(|_|"container".into())}),
    )
}
async fn login(State(s): State<Shared>, Query(q): Query<HashMap<String, String>>) -> Html<String> {
    Html(login_html(
        &s,
        q.get("next").map(String::as_str).unwrap_or("/"),
    ))
}
fn login_html(s: &App, next: &str) -> String {
    let next = crate::auth::safe_next(next);
    let html = if s.auth.config.dev {
        ui::login(Some(
            "Development fixtures enabled. Synthetic identities are for local validation only.",
        ))
    } else {
        ui::login_production(None)
    };
    let encoded: String = url::form_urlencoded::byte_serialize(next.as_bytes()).collect();
    html.replace(
        "href=\"/auth/login\"",
        &format!("href=\"/auth/login?next={encoded}\""),
    )
}
async fn dashboard(State(s): State<Shared>, h: HeaderMap) -> Result<Response, AppError> {
    let Some(p) = s.auth.principal(&h) else {
        return Ok(Redirect::to("/login").into_response());
    };
    let shares = s.core.list_shares(&p).await?;
    Ok(Html(ui::dashboard(
        json!({"shares":normalize_shares(shares),"principal":p,"development":s.auth.config.dev}),
    ))
    .into_response())
}
fn normalize_shares(mut v: Value) -> Value {
    if let Some(a) = v.as_array_mut() {
        for s in a {
            s["viewer_token"] = s["capability"].clone();
        }
    }
    v
}
async fn dev_login(
    State(s): State<Shared>,
    h: HeaderMap,
    Json(v): Json<Value>,
) -> Result<Response, AppError> {
    s.auth.mutation(&h)?;
    let c = s
        .auth
        .dev_login(v["identity"].as_str().unwrap_or("owner"))?;
    Ok(([(header::SET_COOKIE, c)], Json(json!({"ok":true}))).into_response())
}
async fn logout(State(s): State<Shared>, h: HeaderMap) -> Result<Response, AppError> {
    s.auth.mutation(&h)?;
    Ok((
        [(header::SET_COOKIE, s.auth.logout(&h))],
        Json(json!({"ok":true})),
    )
        .into_response())
}
async fn auth_login(
    State(s): State<Shared>,
    Query(q): Query<HashMap<String, String>>,
) -> Result<Response, AppError> {
    let (url, cookie) = s
        .auth
        .begin(q.get("next").map(String::as_str).unwrap_or("/"))?;
    let mut r = Redirect::to(&url).into_response();
    r.headers_mut()
        .insert(header::SET_COOKIE, cookie.parse().unwrap());
    Ok(r)
}
async fn auth_callback(
    State(s): State<Shared>,
    h: HeaderMap,
    Query(q): Query<HashMap<String, String>>,
) -> Result<Response, AppError> {
    let (cookie, next) = s
        .auth
        .callback(
            &h,
            q.get("state").map(String::as_str).unwrap_or(""),
            q.get("code").map(String::as_str).unwrap_or(""),
        )
        .await?;
    let mut r = Redirect::to(&next).into_response();
    r.headers_mut()
        .insert(header::SET_COOKIE, cookie.parse().unwrap());
    Ok(r)
}
async fn asset(Path(name): Path<String>) -> Response {
    let (data, kind) = match name.as_str() {
        "app.css" => (include_str!("../assets/app.css"), "text/css"),
        "app.js" => (include_str!("../assets/app.js"), "text/javascript"),
        _ => ("", "text/plain"),
    };
    ([(header::CONTENT_TYPE, kind)], data).into_response()
}
fn required<'a>(v: &'a Value, key: &str) -> Result<&'a str, AppError> {
    v[key]
        .as_str()
        .ok_or_else(|| error(400, "invalid_request", &format!("{key} is required.")))
}
fn manager(s: &App, h: &HeaderMap) -> Result<Principal, AppError> {
    s.auth.mutation(h)?;
    s.auth.require(h)
}
async fn list(State(s): State<Shared>, h: HeaderMap) -> Result<Json<Value>, AppError> {
    Ok(Json(normalize_shares(
        s.core.list_shares(&s.auth.require(&h)?).await?,
    )))
}
async fn detail(
    State(s): State<Shared>,
    h: HeaderMap,
    Path(id): Path<String>,
) -> Result<Json<Value>, AppError> {
    Ok(Json(s.core.get_share(&s.auth.require(&h)?, &id).await?))
}
async fn notification_status(
    State(s): State<Shared>,
    h: HeaderMap,
    Path(id): Path<String>,
) -> Result<Json<Value>, AppError> {
    Ok(Json(
        s.core
            .notification_status(&s.auth.require(&h)?, &id)
            .await?,
    ))
}
async fn create(
    State(s): State<Shared>,
    h: HeaderMap,
    Json(v): Json<Value>,
) -> Result<Json<Value>, AppError> {
    let p = manager(&s, &h)?;
    Ok(Json(
        s.core
            .create_share(
                &p,
                required(&v, "title")?,
                required(&v, "kind")?,
                required(&v, "policy")?,
                required(&v, "idempotency_key")?,
            )
            .await?,
    ))
}
fn with_upload_url(mut v: Value) -> Value {
    v["upload_url"] = json!(format!(
        "/api/uploads/{}?token={}",
        v["id"].as_str().unwrap(),
        v["token"].as_str().unwrap()
    ));
    v
}
async fn new_upload(
    State(s): State<Shared>,
    h: HeaderMap,
    Path(id): Path<String>,
) -> Result<Json<Value>, AppError> {
    Ok(Json(with_upload_url(
        s.core.create_upload(&manager(&s, &h)?, &id).await?,
    )))
}
async fn upload_status(
    State(s): State<Shared>,
    h: HeaderMap,
    Path(id): Path<String>,
) -> Result<Json<Value>, AppError> {
    Ok(Json(s.core.get_upload(&s.auth.require(&h)?, &id).await?))
}
fn staging_path(s: &App, id: &str) -> Result<PathBuf, AppError> {
    uuid::Uuid::parse_str(id)
        .map_err(|_| error(400, "invalid_upload", "Invalid upload identifier."))?;
    Ok(s.staging.join(format!("{id}.json")))
}
async fn upload(
    State(s): State<Shared>,
    h: HeaderMap,
    Path(id): Path<String>,
    Query(q): Query<HashMap<String, String>>,
    body: Bytes,
) -> Result<Json<Value>, AppError> {
    let p = manager(&s, &h)?;
    let u = s.core.get_upload(&p, &id).await?;
    if q.get("token").map(String::as_str) != u["token"].as_str() {
        return Err(error(403, "invalid_upload_token", "Upload URL is invalid."));
    }
    if !u["revision_id"].is_null() {
        return Err(error(
            409,
            "already_finalized",
            "Upload was already finalized.",
        ));
    }
    let files = if h
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.starts_with("application/zip"))
    {
        ingest::extract_zip(&body).map_err(|e| error(400, "invalid_archive", &e))?
    } else {
        let v: Value = serde_json::from_slice(&body)
            .map_err(|_| error(400, "invalid_upload", "Expected ZIP or JSON files."))?;
        let list = v["files"]
            .as_array()
            .ok_or_else(|| error(400, "invalid_upload", "files must be an array."))?;
        let mut files = BTreeMap::new();
        for f in list {
            let path = required(f, "path")?;
            let bytes = if let Some(b64) = f["content_base64"].as_str() {
                STANDARD
                    .decode(b64)
                    .map_err(|_| error(400, "invalid_upload", "Invalid base64 file."))?
            } else {
                required(f, "content")?.as_bytes().to_vec()
            };
            if files.insert(path.into(), bytes).is_some() {
                return Err(error(400, "invalid_manifest", "Duplicate file path."));
            }
        }
        ingest::validate_files(files).map_err(|e| error(400, "invalid_manifest", &e))?
    };
    match &s.core {
        crate::store::Store::Local(_) => {
            let path = staging_path(&s, &id)?;
            let tmp = path.with_extension(format!("{}.tmp", crate::auth::token()));
            std::fs::write(&tmp, serde_json::to_vec(&files).unwrap())
                .map_err(|_| error(500, "storage_error", "Cannot stage upload."))?;
            std::fs::rename(tmp, path)
                .map_err(|_| error(500, "storage_error", "Cannot commit staged upload."))?;
        }
        crate::store::Store::Firestore(cloud) => {
            cloud
                .stage_upload(
                    &p,
                    &id,
                    &files,
                    files
                        .keys()
                        .next()
                        .map(String::as_str)
                        .unwrap_or("index.html"),
                )
                .await?;
        }
    }
    Ok(Json(
        json!({"id":id,"state":"uploaded","file_count":files.len()}),
    ))
}
async fn finalize(
    State(s): State<Shared>,
    h: HeaderMap,
    Path(id): Path<String>,
    Json(v): Json<Value>,
) -> Result<Json<Value>, AppError> {
    let p = manager(&s, &h)?;
    Ok(Json(finalize_inner(s, p, id, v).await?))
}
async fn finalize_inner(s: Shared, p: Principal, id: String, v: Value) -> Result<Value, AppError> {
    let u = s.core.get_upload(&p, &id).await?;
    let sid = required(&u, "share_id")?;
    let share = s.core.get_share(&p, sid).await?;
    if u["revision_id"].as_str().is_some() {
        return Ok(
            json!({"share":share,"revision_id":u["revision_id"],"viewer_url":format!("/s/{}",share["capability"].as_str().unwrap()),"state":"ready"}),
        );
    }
    let files: BTreeMap<String, Vec<u8>> = match &s.core {
        crate::store::Store::Local(_) => {
            let bytes = std::fs::read(staging_path(&s, &id)?).map_err(|_| {
                error(
                    409,
                    "incomplete_upload",
                    "Upload files before finalization.",
                )
            })?;
            let files: BTreeMap<String, Vec<u8>> = serde_json::from_slice(&bytes)
                .map_err(|_| error(500, "storage_error", "Invalid staging record."))?;
            files
        }
        crate::store::Store::Firestore(cloud) => cloud.staged_files(&p, &id).await?.0,
    };
    let kind = required(&share, "kind")?.to_owned();
    let entry = v["entrypoint"]
        .as_str()
        .unwrap_or(if kind == "markdown" {
            "README.md"
        } else {
            "index.html"
        })
        .to_owned();
    if kind == "react"
        && let crate::store::Store::Firestore(cloud) = &s.core
    {
        let job = crate::build_jobs::enqueue(cloud, &p, &id, &entry).await?;
        let job_id = required(&job, "id")?;
        if let Some(dispatcher) = job_dispatcher()? {
            let _ = crate::build_jobs::dispatch(cloud, job_id, &dispatcher).await?;
        }
        let mut result = crate::build_jobs::status(cloud, &p, job_id).await?;
        result["job_id"] = json!(job_id);
        result["status_url"] = json!(format!("/api/build-jobs/{job_id}"));
        if std::env::var("ARTIFACT_BUILD_JOB_NAME").is_err() {
            result["operator_notice"] =
                json!("Build queued durably. A configured Cloud Run Job dispatcher is required.");
        }
        return Ok(result);
    }
    let prepared = tokio::task::spawn_blocking(move || {
        ingest::prepare_with_mode(
            &kind,
            files,
            &entry,
            &std::env::var("ARTIFACT_BUILD_MODE").unwrap_or_else(|_| "container".into()),
        )
    })
    .await
    .map_err(|_| error(500, "preparation_failed", "Preparation worker stopped."))?
    .map_err(|e| error(422, "preparation_failed", &e))?;
    let mut out = match &s.core {
        crate::store::Store::Local(local) => {
            local.publish(&p, &id, &prepared.files, &prepared.entrypoint)?
        }
        crate::store::Store::Firestore(cloud) => {
            cloud
                .publish_at_version(
                    &p,
                    &id,
                    &prepared.files,
                    &prepared.entrypoint,
                    share["version"].as_u64().unwrap_or(0),
                )
                .await?
        }
    };
    out["warnings"] = json!(prepared.warnings);
    out["state"] = json!("ready");
    out["viewer_url"] = json!(format!("/s/{}", share["capability"].as_str().unwrap()));
    Ok(out)
}
fn job_dispatcher() -> Result<Option<crate::build_jobs::CloudRunDispatcher>, AppError> {
    let Ok(job) = std::env::var("ARTIFACT_BUILD_JOB_NAME") else {
        return Ok(None);
    };
    let config = crate::build_jobs::CloudRunJobConfig {
        project: std::env::var("GOOGLE_CLOUD_PROJECT")
            .map_err(|_| error(503, "job_config", "Google project required."))?,
        region: std::env::var("ARTIFACT_BUILD_JOB_REGION")
            .map_err(|_| error(503, "job_config", "Build job region required."))?,
        job,
        container: std::env::var("ARTIFACT_BUILD_JOB_CONTAINER")
            .unwrap_or_else(|_| "worker".into()),
        timeout_seconds: 120,
    };
    Ok(Some(crate::build_jobs::CloudRunDispatcher::production(
        config,
    )?))
}
async fn build_status(
    State(s): State<Shared>,
    h: HeaderMap,
    Path(id): Path<String>,
) -> Result<Json<Value>, AppError> {
    let p = s.auth.require(&h)?;
    match &s.core {
        crate::store::Store::Firestore(cloud) => {
            Ok(Json(crate::build_jobs::status(cloud, &p, &id).await?))
        }
        _ => Err(error(
            404,
            "not_found",
            "Local reference builds return results directly.",
        )),
    }
}
async fn lifecycle(
    State(s): State<Shared>,
    h: HeaderMap,
    Path(id): Path<String>,
    Json(v): Json<Value>,
) -> Result<Json<Value>, AppError> {
    Ok(Json(
        s.core
            .lifecycle(&manager(&s, &h)?, &id, required(&v, "action")?)
            .await?,
    ))
}
async fn viewer(s: &App, h: &HeaderMap, cap: &str) -> Result<Option<Principal>, AppError> {
    let gated = s.auth.gated_principal(h);
    match s.core.viewer_share(cap, gated.as_ref()).await {
        Ok(_) => Ok(s.auth.principal(h)),
        Err(e) if e.status == 401 => Err(e),
        Err(e) => Err(e),
    }
}
async fn view(
    State(s): State<Shared>,
    h: HeaderMap,
    Path(cap): Path<String>,
    Query(q): Query<HashMap<String, String>>,
) -> Result<Response, AppError> {
    let p = match viewer(&s, &h, &cap).await {
        Ok(p) => p,
        Err(e) if e.status == 401 => {
            return Ok((
                StatusCode::UNAUTHORIZED,
                Html(login_html(&s, &format!("/s/{cap}"))),
            )
                .into_response());
        }
        Err(e) => return Err(e),
    };
    let share = s.core.viewer_share(&cap, p.as_ref()).await?;
    let rev = s
        .core
        .viewer_revision(&cap, q.get("revision").map(String::as_str), p.as_ref())
        .await?;
    let rid = required(&rev, "id")?;
    let content_url = format!(
        "/frame/{}/{}/{}/{}",
        cap,
        rid,
        s.auth.frame_ticket(&cap, rid),
        required(&rev, "entrypoint")?
    );
    let owner = if let Some(p) = s.auth.principal(&h) {
        s.core
            .get_share(&p, share["id"].as_str().unwrap())
            .await
            .is_ok()
    } else {
        false
    };
    Ok(Html(ui::review(json!({"share":share,"revision":rev,"threads":s.core.threads(&cap,p.as_ref()).await?,"capability":cap,"content_url":content_url,"principal":p,"is_owner":owner,"development":s.auth.config.dev}))).into_response())
}
async fn content(
    State(s): State<Shared>,
    h: HeaderMap,
    Path((cap, revision, path)): Path<(String, String, String)>,
) -> Result<Response, AppError> {
    let p = viewer(&s, &h, &cap).await?.unwrap_or(Principal {
        id: "link-viewer".into(),
        display_name: "Guest".into(),
        verified_email: None,
        authenticated: false,
    });
    serve_content(s, cap, revision, path, p).await
}
async fn frame_content(
    State(s): State<Shared>,
    Path((cap, revision, ticket, path)): Path<(String, String, String, String)>,
) -> Result<Response, AppError> {
    let p = s.auth.check_frame(&ticket, &cap, &revision)?;
    serve_content(s, cap, revision, path, p).await
}
async fn serve_content(
    s: Shared,
    cap: String,
    revision: String,
    path: String,
    p: Principal,
) -> Result<Response, AppError> {
    let files = s
        .core
        .revision_files(&cap, Some(&revision), Some(&p))
        .await?;
    let path = if path.ends_with('/') {
        format!("{path}index.html")
    } else {
        path
    };
    let bytes = files
        .get(&path)
        .ok_or_else(|| error(404, "not_found", "Artifact file not found."))?;
    let mime = mime_guess::from_path(&path).first_or_octet_stream();
    let mut data = bytes.clone();
    if mime.essence_str() == "text/html" {
        let html = String::from_utf8_lossy(bytes);
        let bridge = ui::bridge_script("");
        data = if let Some(index) = html.to_lowercase().rfind("</body>") {
            format!("{}{}{}", &html[..index], bridge, &html[index..]).into_bytes()
        } else {
            format!("{html}{bridge}").into_bytes()
        };
    }
    let mut out = ([(header::CONTENT_TYPE, mime.to_string())], data).into_response();
    let origin = &s.auth.config.origin;
    let sources = format!("{origin}/frame/{cap}/{revision}/ {origin}/content/{cap}/{revision}/");
    let policy = format!(
        "sandbox allow-scripts; default-src 'none'; script-src 'unsafe-inline' {sources}; style-src 'unsafe-inline' {sources}; img-src data: {sources}; font-src {sources}; connect-src 'none'; frame-src 'none'; form-action 'none'; base-uri 'none'; object-src 'none'"
    );
    out.headers_mut().insert(
        header::CONTENT_SECURITY_POLICY,
        policy.parse().map_err(|_| {
            error(
                500,
                "invalid_origin",
                "Configured content origin is invalid.",
            )
        })?,
    );
    out.headers_mut()
        .insert(header::ACCESS_CONTROL_ALLOW_ORIGIN, "null".parse().unwrap());
    Ok(out)
}
async fn threads(
    State(s): State<Shared>,
    h: HeaderMap,
    Path(cap): Path<String>,
) -> Result<Json<Value>, AppError> {
    let p = viewer(&s, &h, &cap).await?;
    Ok(Json(s.core.threads(&cap, p.as_ref()).await?))
}
async fn add_thread(
    State(s): State<Shared>,
    h: HeaderMap,
    Path(cap): Path<String>,
    Json(v): Json<Value>,
) -> Result<Json<Value>, AppError> {
    s.auth.mutation(&h)?;
    let p = viewer(&s, &h, &cap).await?;
    Ok(Json(
        s.core
            .add_thread(
                &cap,
                p.as_ref(),
                v["guest_name"].as_str(),
                required(&v, "body")?,
                required(&v, "revision")?,
                v.get("anchor")
                    .cloned()
                    .unwrap_or(json!({"kind":"general"})),
            )
            .await?,
    ))
}
async fn reply(
    State(s): State<Shared>,
    h: HeaderMap,
    Path((cap, id)): Path<(String, String)>,
    Json(v): Json<Value>,
) -> Result<Json<Value>, AppError> {
    s.auth.mutation(&h)?;
    let p = viewer(&s, &h, &cap).await?;
    Ok(Json(
        s.core
            .reply(
                &cap,
                &id,
                p.as_ref(),
                v["guest_name"].as_str(),
                required(&v, "body")?,
            )
            .await?,
    ))
}
async fn resolve(
    State(s): State<Shared>,
    h: HeaderMap,
    Path((id, thread)): Path<(String, String)>,
    Json(v): Json<Value>,
) -> Result<Json<Value>, AppError> {
    Ok(Json(
        s.core
            .resolve(
                &manager(&s, &h)?,
                &id,
                &thread,
                v["resolved"]
                    .as_bool()
                    .ok_or_else(|| error(400, "invalid_request", "resolved boolean required."))?,
            )
            .await?,
    ))
}
const TOOLS: &[(&str, &str)] = &[
    ("list_shares", "List shares owned by the publisher"),
    ("create_share", "Create a share with explicit policy"),
    ("get_share", "Get share and revision status"),
    (
        "create_upload",
        "Create an authenticated ZIP or multifile upload URL",
    ),
    ("get_upload", "Read upload status"),
    ("get_build", "Read durable source build status"),
    (
        "notification_status",
        "Read owner-only notification batch status",
    ),
    ("finalize_upload", "Prepare and publish staged content"),
    (
        "lifecycle",
        "Unpublish, archive, soft-delete, restore or republish",
    ),
    ("list_threads", "Read a share discussion"),
    ("add_thread", "Create general or anchored discussion"),
    ("reply", "Reply to a thread"),
    ("resolve", "Owner resolves or reopens a thread"),
];
fn tool_schema(name: &str) -> Value {
    let fields: &[&str] = match name {
        "list_shares" => &[],
        "create_share" => &["title", "kind", "policy", "idempotency_key"],
        "get_share" | "create_upload" | "notification_status" => &["share_id"],
        "get_upload" => &["upload_id"],
        "get_build" => &["job_id"],
        "finalize_upload" => &["upload_id", "entrypoint"],
        "lifecycle" => &["share_id", "action"],
        "list_threads" => &["capability"],
        "add_thread" => &["capability", "revision", "body", "anchor"],
        "reply" => &["capability", "thread_id", "body"],
        "resolve" => &["share_id", "thread_id", "resolved"],
        _ => &[],
    };
    let properties:serde_json::Map<String,Value>=fields.iter().map(|k|(k.to_string(),match *k{"resolved"=>json!({"type":"boolean"}),"anchor"=>json!({"type":"object","description":"Immutable original anchor: kind general/text/element, path, exact/prefix/suffix, element_id/selector."}),"kind"=>json!({"type":"string","enum":["static","markdown","react"]}),"policy"=>json!({"type":"string","enum":["link-only","entra-gated"]}),"action"=>json!({"type":"string","enum":["unpublish","archive","delete","restore","republish"]}),_=>json!({"type":"string"})})).collect();
    json!({"type":"object","properties":properties,"required":fields.iter().filter(|k|**k!="anchor"&&**k!="entrypoint").collect::<Vec<_>>(),"additionalProperties":false})
}
async fn mcp(
    State(s): State<Shared>,
    h: HeaderMap,
    Json(v): Json<Value>,
) -> Result<Json<Value>, AppError> {
    s.auth.mutation(&h)?;
    let p = s.auth.require(&h)?;
    let id = v.get("id").cloned().unwrap_or(Value::Null);
    let method = v["method"].as_str().unwrap_or("");
    let result = match method {
        "initialize" => Ok(
            json!({"protocolVersion":"2025-03-26","capabilities":{"tools":{}},"serverInfo":{"name":"artifact-board","version":"0.1.0"}}),
        ),
        "tools/list" => Ok(
            json!({"tools":TOOLS.iter().map(|(name,description)|json!({"name":name,"description":description,"inputSchema":tool_schema(name)})).collect::<Vec<_>>()}),
        ),
        "tools/call" => {
            let name = v["params"]["name"].as_str().unwrap_or("");
            let a = &v["params"]["arguments"];
            let result: Result<Value, AppError> = async {
                match name {
                    "list_shares" => s.core.list_shares(&p).await,
                    "create_share" => {
                        s.core
                            .create_share(
                                &p,
                                required(a, "title")?,
                                required(a, "kind")?,
                                required(a, "policy")?,
                                required(a, "idempotency_key")?,
                            )
                            .await
                    }
                    "get_share" => s.core.get_share(&p, required(a, "share_id")?).await,
                    "create_upload" => Ok(with_upload_url(
                        s.core.create_upload(&p, required(a, "share_id")?).await?,
                    )),
                    "get_upload" => s.core.get_upload(&p, required(a, "upload_id")?).await,
                    "notification_status" => {
                        s.core
                            .notification_status(&p, required(a, "share_id")?)
                            .await
                    }
                    "get_build" => match &s.core {
                        crate::store::Store::Firestore(cloud) => {
                            crate::build_jobs::status(cloud, &p, required(a, "job_id")?).await
                        }
                        _ => Err(error(404, "not_found", "No durable local build job.")),
                    },
                    "finalize_upload" => {
                        finalize_inner(
                            s.clone(),
                            p.clone(),
                            required(a, "upload_id")?.into(),
                            a.clone(),
                        )
                        .await
                    }
                    "lifecycle" => {
                        s.core
                            .lifecycle(&p, required(a, "share_id")?, required(a, "action")?)
                            .await
                    }
                    "list_threads" => {
                        let cap = required(a, "capability")?;
                        let vp = viewer(&s, &h, cap).await?;
                        s.core.threads(cap, vp.as_ref()).await
                    }
                    "add_thread" => {
                        let cap = required(a, "capability")?;
                        let vp = viewer(&s, &h, cap).await?;
                        s.core
                            .add_thread(
                                cap,
                                vp.as_ref(),
                                None,
                                required(a, "body")?,
                                required(a, "revision")?,
                                a.get("anchor")
                                    .cloned()
                                    .unwrap_or(json!({"kind":"general"})),
                            )
                            .await
                    }
                    "reply" => {
                        let cap = required(a, "capability")?;
                        let vp = viewer(&s, &h, cap).await?;
                        s.core
                            .reply(
                                cap,
                                required(a, "thread_id")?,
                                vp.as_ref(),
                                None,
                                required(a, "body")?,
                            )
                            .await
                    }
                    "resolve" => {
                        s.core
                            .resolve(
                                &p,
                                required(a, "share_id")?,
                                required(a, "thread_id")?,
                                a["resolved"].as_bool().ok_or_else(|| {
                                    error(400, "invalid_request", "resolved boolean required.")
                                })?,
                            )
                            .await
                    }
                    _ => Err(error(404, "unknown_tool", "Unknown MCP tool.")),
                }
            }
            .await;
            Ok(match result {
                Ok(r) => {
                    json!({"content":[{"type":"text","text":serde_json::to_string(&r).unwrap()}],"structuredContent":r,"isError":false})
                }
                Err(e) => {
                    json!({"content":[{"type":"text","text":e.to_string()}],"isError":true,"error":{"code":e.code,"message":e.message}})
                }
            })
        }
        _ => Err(error(400, "method_not_found", "Unknown JSON-RPC method.")),
    };
    Ok(Json(match result {
        Ok(result) => json!({"jsonrpc":"2.0","id":id,"result":result}),
        Err(e) => json!({"jsonrpc":"2.0","id":id,"error":{"code":-32601,"message":e.message}}),
    }))
}
