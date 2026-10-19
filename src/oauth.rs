//! First-party MCP authorization server. Entra authenticates the human; bounded
//! grants authorize the agent without exposing browser or upstream credentials.
use crate::{
    auth::{Auth, cookie, error, now, token},
    cloud_store::{CloudStore, Collection, Document, Mutation, Precondition},
    core::{AppError, Principal},
    server::Shared,
};
use axum::{
    Form, Json,
    extract::{Query, State},
    http::{HeaderMap, StatusCode, header},
    response::{Html, IntoResponse, Redirect, Response},
};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{collections::HashMap, sync::Mutex};

const SCOPE: &str = "artifact:manage";
const ACCESS_SECONDS: u64 = 900;
const GRANT_SECONDS: u64 = 30 * 86400;

pub enum Grants {
    Local(Mutex<Connection>),
    Cloud(CloudStore),
}
impl Grants {
    pub fn local(path: &std::path::Path) -> Result<Self, AppError> {
        let c = Connection::open(path).map_err(storage)?;
        c.busy_timeout(std::time::Duration::from_secs(5))
            .map_err(storage)?;
        c.execute_batch("CREATE TABLE IF NOT EXISTS oauth_grants(id TEXT PRIMARY KEY, data TEXT NOT NULL, version INTEGER NOT NULL)").map_err(storage)?;
        Ok(Self::Local(Mutex::new(c)))
    }
    async fn get(&self, id: &str) -> Result<Option<Document>, AppError> {
        match self {
            Self::Local(c) => c
                .lock()
                .unwrap()
                .query_row(
                    "SELECT data,version FROM oauth_grants WHERE id=?",
                    [id],
                    |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)),
                )
                .optional()
                .map_err(storage)?
                .map(|(data, v)| {
                    Ok(Document {
                        data: serde_json::from_str(&data).map_err(storage)?,
                        update_time: v.to_string(),
                    })
                })
                .transpose(),
            Self::Cloud(c) => {
                c.get(Collection::OAuthGrants, id, None, &c.runtime_token().await?)
                    .await
            }
        }
    }
    async fn put(&self, id: &str, data: Value, version: Option<&str>) -> Result<(), AppError> {
        match self {
            Self::Local(c) => {
                let c = c.lock().unwrap();
                let changed = if let Some(v) = version {
                    c.execute(
                        "UPDATE oauth_grants SET data=?,version=version+1 WHERE id=? AND version=?",
                        params![data.to_string(), id, v],
                    )
                    .map_err(storage)?
                } else {
                    c.execute(
                        "INSERT OR IGNORE INTO oauth_grants(id,data,version) VALUES(?,?,1)",
                        params![id, data.to_string()],
                    )
                    .map_err(storage)?
                };
                if changed != 1 {
                    return Err(error(
                        409,
                        "storage_conflict",
                        "Grant changed; restart authorization.",
                    ));
                }
                Ok(())
            }
            Self::Cloud(c) => {
                c.commit(
                    &[Mutation {
                        collection: Collection::OAuthGrants,
                        id: id.into(),
                        data,
                        condition: version
                            .map(|v| Precondition::UpdateTime(v.into()))
                            .unwrap_or(Precondition::Create),
                    }],
                    None,
                    &c.runtime_token().await?,
                )
                .await?;
                Ok(())
            }
        }
    }
    async fn list(&self, owner: &str) -> Result<Vec<Value>, AppError> {
        match self {
            Self::Local(c) => {
                let c = c.lock().unwrap();
                let mut stmt = c
                    .prepare("SELECT data FROM oauth_grants WHERE json_extract(data,'$.owner')=?")
                    .map_err(storage)?;
                stmt.query_map([owner], |r| r.get::<_, String>(0))
                    .map_err(storage)?
                    .map(|r| serde_json::from_str(&r.map_err(storage)?).map_err(storage))
                    .collect()
            }
            Self::Cloud(c) => Ok(c
                .query(
                    Collection::OAuthGrants,
                    "owner",
                    &json!(owner),
                    None,
                    &c.runtime_token().await?,
                )
                .await?
                .into_iter()
                .map(|d| d.data)
                .collect()),
        }
    }
}
fn storage(_: impl std::fmt::Display) -> AppError {
    error(
        503,
        "oauth_storage_unavailable",
        "Agent authorization storage is unavailable.",
    )
}
fn hash(s: &str) -> String {
    format!("{:x}", Sha256::digest(s.as_bytes()))
}
fn resource(auth: &Auth) -> String {
    format!("{}/mcp", auth.config.origin)
}
fn invalid() -> AppError {
    error(
        400,
        "invalid_grant",
        "Authorization expired, was revoked, or does not match this client. Sign in again.",
    )
}
fn text<'a>(v: &'a Value, k: &str) -> &'a str {
    v[k].as_str().unwrap_or("")
}
fn esc(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}
fn page(title: &str, content: &str) -> Response {
    let mut response = Html(format!(
        "<!doctype html><html lang=\"en\"><head><meta charset=\"utf-8\"><meta name=\"viewport\" content=\"width=device-width,initial-scale=1\"><title>{title} · Artifact Board</title><link rel=\"stylesheet\" href=\"/assets/app.css\"></head><body><main class=\"oauth-page\"><a class=\"oauth-home\" href=\"/\">Artifact Board</a><h1>{title}</h1>{content}</main></body></html>"
    )).into_response();
    response
        .headers_mut()
        .insert(header::REFERRER_POLICY, "strict-origin".parse().unwrap());
    response
}
#[derive(Clone, Serialize, Deserialize)]
struct Client {
    iss: String,
    name: String,
    redirect_uris: Vec<String>,
    exp: u64,
    aud: String,
}
#[derive(Clone, Serialize, Deserialize)]
struct Authorization {
    client_id: String,
    redirect_uri: String,
    state: String,
    challenge: String,
    resource: String,
    exp: u64,
    aud: String,
}
#[derive(Serialize, Deserialize)]
pub(crate) struct Access {
    pub principal: Principal,
    pub grant: String,
    exp: u64,
    aud: String,
    iss: String,
    scope: String,
}
#[derive(Serialize, Deserialize)]
struct Refresh {
    grant: String,
    nonce: String,
    exp: u64,
    aud: String,
    iss: String,
}
fn trusted(auth: &Auth, p: &Principal) -> bool {
    p.authenticated
        && (auth.config.dev
            || (auth.config.configured()
                && auth
                    .config
                    .tenant
                    .as_ref()
                    .is_some_and(|t| p.id.starts_with(&format!("entra:{t}:")))))
}
pub(crate) fn access_claim(auth: &Auth, h: &HeaderMap) -> Option<Access> {
    let t = h
        .get(header::AUTHORIZATION)?
        .to_str()
        .ok()?
        .strip_prefix("Bearer ")?;
    let c: Access = auth.decode(t, &resource(auth))?;
    (c.iss == auth.config.origin && c.scope == SCOPE && trusted(auth, &c.principal)).then_some(c)
}
pub async fn validate_access(s: &Shared, h: &HeaderMap) -> Result<(), AppError> {
    let c = access_claim(&s.auth, h)
        .ok_or_else(|| error(401, "invalid_token", "Sign in to Artifact Board again."))?;
    let d = s
        .grants
        .get(&c.grant)
        .await?
        .ok_or_else(|| error(401, "invalid_token", "Agent grant does not exist."))?;
    if d.data["revoked"] != false
        || d.data["used"] != true
        || d.data["expires"].as_u64().unwrap_or(0) <= now()
        || d.data["owner"] != c.principal.id
    {
        return Err(error(
            401,
            "invalid_token",
            "Agent grant expired or was revoked.",
        ));
    }
    Ok(())
}
pub fn challenge(auth: &Auth) -> String {
    format!(
        "Bearer resource_metadata=\"{}/.well-known/oauth-protected-resource/mcp\", scope=\"{SCOPE}\"",
        auth.config.origin
    )
}
pub async fn protected_metadata(State(s): State<Shared>) -> Json<Value> {
    Json(
        json!({"resource":resource(&s.auth),"authorization_servers":[s.auth.config.origin],"scopes_supported":[SCOPE],"bearer_methods_supported":["header"],"resource_name":"Artifact Board"}),
    )
}
pub async fn metadata(State(s): State<Shared>) -> Json<Value> {
    let o = &s.auth.config.origin;
    Json(
        json!({"issuer":o,"authorization_endpoint":format!("{o}/oauth/authorize"),"token_endpoint":format!("{o}/oauth/token"),"registration_endpoint":format!("{o}/oauth/register"),"revocation_endpoint":format!("{o}/oauth/revoke"),"response_types_supported":["code"],"grant_types_supported":["authorization_code","refresh_token"],"token_endpoint_auth_methods_supported":["none"],"code_challenge_methods_supported":["S256"],"scopes_supported":[SCOPE],"authorization_response_iss_parameter_supported":true}),
    )
}
fn loopback(uri: &str) -> bool {
    url::Url::parse(uri).is_ok_and(|u| {
        u.scheme() == "http"
            && matches!(u.host_str(), Some("127.0.0.1" | "[::1]" | "localhost"))
            && u.username().is_empty()
            && u.password().is_none()
            && u.fragment().is_none()
            && u.query().is_none()
            && (u.path() == "/callback" || u.path().starts_with("/callback/"))
            && uri.len() <= 512
    })
}
fn allowed_callback(uri: &str) -> bool {
    if loopback(uri) {
        return true;
    }
    if matches!(
        uri,
        "https://claude.ai/api/mcp/auth_callback"
            | "https://chatgpt.com/connector_platform_oauth_redirect"
    ) {
        return true;
    }
    uri.strip_prefix("https://chatgpt.com/connector/oauth/")
        .is_some_and(|id| {
            !id.is_empty()
                && id.len() <= 128
                && id
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
        })
}
pub async fn register(State(s): State<Shared>, Json(v): Json<Value>) -> Result<Response, AppError> {
    if !s.auth.config.dev && !s.auth.config.configured() {
        return Err(error(
            503,
            "identity_not_configured",
            "Entra identity is not configured.",
        ));
    }
    let uris: Vec<String> = serde_json::from_value(v["redirect_uris"].clone())
        .map_err(|_| error(400, "invalid_redirect_uri", "Callback URLs are required."))?;
    if uris.is_empty() || uris.len() > 4 || !uris.iter().all(|u| allowed_callback(u)) {
        return Err(error(
            400,
            "invalid_redirect_uri",
            "Use a desktop loopback callback or a supported Claude or ChatGPT HTTPS callback.",
        ));
    }
    if v.get("token_endpoint_auth_method")
        .is_some_and(|a| a != "none")
        || v.get("grant_types").is_some_and(|a| {
            a.as_array().is_none_or(|a| {
                a.iter()
                    .any(|g| g != "authorization_code" && g != "refresh_token")
            })
        })
        || v.get("response_types")
            .is_some_and(|a| a != &json!(["code"]))
    {
        return Err(error(
            400,
            "invalid_client_metadata",
            "Only public clients with authorization code and refresh grants are supported.",
        ));
    }
    let name = v["client_name"].as_str().unwrap_or("MCP client");
    if name.len() > 100 || name.chars().any(char::is_control) {
        return Err(error(
            400,
            "invalid_client_metadata",
            "Client name is too long or invalid.",
        ));
    }
    let c = Client {
        iss: s.auth.config.origin.clone(),
        name: name.into(),
        redirect_uris: uris.clone(),
        exp: now() + 365 * 86400,
        aud: "artifact-mcp-client".into(),
    };
    Ok((StatusCode::CREATED,Json(json!({"client_id":s.auth.sign(&c),"client_id_issued_at":now(),"client_name":name,"redirect_uris":uris,"token_endpoint_auth_method":"none","grant_types":["authorization_code","refresh_token"],"response_types":["code"]}))).into_response())
}
fn client(auth: &Auth, id: &str) -> Result<Client, AppError> {
    auth.decode::<Client>(id, "artifact-mcp-client")
        .filter(|c| c.iss == auth.config.origin)
        .ok_or_else(|| error(400, "invalid_client", "Register this client again."))
}
fn bounded(value: &str, max: usize) -> bool {
    !value.is_empty() && value.len() <= max && !value.chars().any(char::is_control)
}
pub async fn authorize(
    State(s): State<Shared>,
    Query(q): Query<HashMap<String, String>>,
) -> Result<Response, AppError> {
    let get = |k: &str| q.get(k).map(String::as_str).unwrap_or("");
    let c = client(&s.auth, get("client_id"))?;
    if !c.redirect_uris.iter().any(|u| u == get("redirect_uri")) {
        return Err(error(
            400,
            "invalid_redirect_uri",
            "Callback does not match client registration.",
        ));
    }
    let challenge = get("code_challenge");
    if get("response_type") != "code"
        || get("code_challenge_method") != "S256"
        || challenge.len() != 43
        || URL_SAFE_NO_PAD.decode(challenge).is_err()
        || !bounded(get("state"), 1024)
        || (!get("scope").is_empty() && get("scope") != SCOPE)
        || (!get("resource").is_empty() && get("resource") != resource(&s.auth))
    {
        return Err(error(
            400,
            "invalid_request",
            "Authorization requires code, S256 PKCE, state, and the Artifact Board resource/scope.",
        ));
    }
    let request = s.auth.sign(&Authorization {
        client_id: get("client_id").into(),
        redirect_uri: get("redirect_uri").into(),
        state: get("state").into(),
        challenge: challenge.into(),
        resource: resource(&s.auth),
        exp: now() + 600,
        aud: "artifact-mcp-authorization".into(),
    });
    let mut r = Redirect::to(&format!("/oauth/consent?request={request}")).into_response();
    r.headers_mut().insert(
        header::SET_COOKIE,
        format!(
            "artifact_mcp_authorize={}; Path=/oauth; HttpOnly; SameSite=Lax; Max-Age=600{}",
            hash(&request),
            if s.auth.config.dev { "" } else { "; Secure" }
        )
        .parse()
        .unwrap(),
    );
    Ok(r)
}
fn pending(auth: &Auth, h: &HeaderMap, request: &str) -> Result<Authorization, AppError> {
    if cookie(h, "artifact_mcp_authorize").as_deref() != Some(hash(request).as_str()) {
        return Err(error(
            403,
            "invalid_state",
            "Restart agent login in this browser.",
        ));
    }
    auth.decode::<Authorization>(request, "artifact-mcp-authorization")
        .filter(|a| a.resource == resource(auth))
        .ok_or_else(|| {
            error(
                400,
                "invalid_request",
                "Agent login expired. Restart login.",
            )
        })
}
fn origin(auth: &Auth, h: &HeaderMap) -> Result<(), AppError> {
    if h.get(header::ORIGIN).and_then(|v| v.to_str().ok()) != Some(auth.config.origin.as_str())
        || h.get("sec-fetch-site").is_some_and(|v| v == "cross-site")
    {
        return Err(error(403, "csrf", "Cross-origin approval rejected."));
    }
    Ok(())
}
pub async fn consent(
    State(s): State<Shared>,
    h: HeaderMap,
    Query(q): Query<HashMap<String, String>>,
) -> Result<Response, AppError> {
    let request = q.get("request").map(String::as_str).unwrap_or("");
    let a = pending(&s.auth, &h, request)?;
    let c = client(&s.auth, &a.client_id)?;
    let Some(p) = s.auth.principal(&h) else {
        let next = format!("/oauth/consent?request={request}");
        let encoded: String = url::form_urlencoded::byte_serialize(next.as_bytes()).collect();
        return Ok(Redirect::to(&format!("/auth/login?next={encoded}")).into_response());
    };
    let mut response = page(
        "Connect your agent",
        &format!(
            "<p class=\"subtle\">Signed in as {}</p><h2>{}</h2><p>This desktop client can manage your shares, publish revisions, upload files, and read and respond to feedback as you.</p><p>Access lasts up to 30 days. You can disconnect it at any time from <a href=\"/oauth/connections\">Connected agents</a>.</p><p class=\"fineprint\">Client name is supplied by the requesting application. Approve only a login you started. Callback: <code>{}</code></p><form method=\"post\" action=\"/oauth/approve\"><input type=\"hidden\" name=\"request\" value=\"{}\"><div class=\"oauth-actions\"><button class=\"button primary\" name=\"decision\" value=\"allow\">Connect agent</button><button class=\"button secondary\" name=\"decision\" value=\"deny\">Cancel</button></div></form>",
            esc(&p.display_name),
            esc(&c.name),
            esc(&a.redirect_uri),
            esc(request)
        ),
    );
    let callback_origin = url::Url::parse(&a.redirect_uri)
        .map_err(|_| invalid())?
        .origin()
        .ascii_serialization();
    response.headers_mut().insert(header::CONTENT_SECURITY_POLICY,format!("default-src 'self'; script-src 'self'; style-src 'self' 'unsafe-inline'; img-src 'self' data:; object-src 'none'; base-uri 'none'; frame-ancestors 'none'; form-action 'self' {callback_origin}").parse().map_err(|_|invalid())?);
    Ok(response)
}
pub async fn approve(
    State(s): State<Shared>,
    h: HeaderMap,
    Form(f): Form<HashMap<String, String>>,
) -> Result<Response, AppError> {
    origin(&s.auth, &h)?;
    let request = f.get("request").map(String::as_str).unwrap_or("");
    let a = pending(&s.auth, &h, request)?;
    let c = client(&s.auth, &a.client_id)?;
    let p = s.auth.require(&h)?;
    let mut redirect = url::Url::parse(&a.redirect_uri).map_err(|_| invalid())?;
    if f.get("decision").map(String::as_str) == Some("allow") {
        let id = token();
        let code = token();
        s.grants.put(&id,json!({"id":id,"owner":p.id,"principal":p,"client_id":a.client_id,"client_name":c.name,"redirect_uri":a.redirect_uri,"challenge":a.challenge,"resource":a.resource,"code_hash":hash(&code),"code_expires":now()+120,"used":false,"revoked":false,"created":now(),"expires":now()+GRANT_SECONDS}),None).await?;
        redirect
            .query_pairs_mut()
            .append_pair("code", &format!("{id}.{code}"));
    } else {
        redirect
            .query_pairs_mut()
            .append_pair("error", "access_denied");
    }
    redirect
        .query_pairs_mut()
        .append_pair("state", &a.state)
        .append_pair("iss", &s.auth.config.origin);
    let mut r = Redirect::to(redirect.as_str()).into_response();
    r.headers_mut().insert(
        header::SET_COOKIE,
        "artifact_mcp_authorize=; Path=/oauth; HttpOnly; SameSite=Lax; Max-Age=0"
            .parse()
            .unwrap(),
    );
    Ok(r)
}
fn tokens(auth: &Auth, id: &str, d: &mut Value) -> Result<Value, AppError> {
    let principal: Principal =
        serde_json::from_value(d["principal"].clone()).map_err(|_| invalid())?;
    if !trusted(auth, &principal) {
        return Err(invalid());
    }
    let access = auth.sign(&Access {
        principal,
        grant: id.into(),
        exp: now() + ACCESS_SECONDS,
        aud: resource(auth),
        iss: auth.config.origin.clone(),
        scope: SCOPE.into(),
    });
    let refresh = auth.sign(&Refresh {
        grant: id.into(),
        nonce: token(),
        exp: d["expires"].as_u64().ok_or_else(invalid)?,
        aud: "artifact-mcp-refresh".into(),
        iss: auth.config.origin.clone(),
    });
    d["refresh_hash"] = json!(hash(&refresh));
    d["used"] = json!(true);
    Ok(
        json!({"access_token":access,"token_type":"Bearer","expires_in":ACCESS_SECONDS,"refresh_token":refresh,"scope":SCOPE}),
    )
}
async fn exchange(s: &Shared, f: &HashMap<String, String>) -> Result<Value, AppError> {
    let get = |k: &str| f.get(k).map(String::as_str).unwrap_or("");
    client(&s.auth, get("client_id"))?;
    if !get("resource").is_empty() && get("resource") != resource(&s.auth) {
        return Err(error(
            400,
            "invalid_target",
            "Token resource must be this MCP server.",
        ));
    }
    if !get("scope").is_empty() && get("scope") != SCOPE {
        return Err(error(
            400,
            "invalid_scope",
            "Scope escalation is forbidden.",
        ));
    }
    let (id, secret, refresh) = match get("grant_type") {
        "authorization_code" => {
            let (id, c) = get("code").split_once('.').ok_or_else(invalid)?;
            (id.to_owned(), c.to_owned(), false)
        }
        "refresh_token" => {
            let c: Refresh = s
                .auth
                .decode(get("refresh_token"), "artifact-mcp-refresh")
                .ok_or_else(invalid)?;
            if c.iss != s.auth.config.origin {
                return Err(invalid());
            }
            (c.grant, get("refresh_token").to_owned(), true)
        }
        _ => {
            return Err(error(
                400,
                "unsupported_grant_type",
                "Use authorization_code or refresh_token.",
            ));
        }
    };
    if id.len() != 64 || !id.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(invalid());
    }
    let mut d = s.grants.get(&id).await?.ok_or_else(invalid)?;
    if d.data["client_id"] != get("client_id")
        || d.data["revoked"] != false
        || d.data["expires"].as_u64().unwrap_or(0) <= now()
    {
        return Err(invalid());
    }
    if refresh {
        if d.data["used"] != true {
            return Err(invalid());
        }
        if d.data["refresh_hash"] != hash(&secret) {
            d.data["revoked"] = json!(true);
            s.grants.put(&id, d.data, Some(&d.update_time)).await?;
            return Err(invalid());
        }
    } else {
        let verifier = get("code_verifier");
        if d.data["used"] != false
            || d.data["code_hash"] != hash(&secret)
            || d.data["code_expires"].as_u64().unwrap_or(0) <= now()
            || d.data["redirect_uri"] != get("redirect_uri")
            || !(43..=128).contains(&verifier.len())
            || !verifier
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"-._~".contains(&b))
            || d.data["challenge"] != URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()))
        {
            return Err(invalid());
        }
    }
    let response = tokens(&s.auth, &id, &mut d.data)?;
    s.grants
        .put(&id, d.data, Some(&d.update_time))
        .await
        .map_err(|e| if e.status == 409 { invalid() } else { e })?;
    Ok(response)
}
pub async fn token_endpoint(
    State(s): State<Shared>,
    Form(f): Form<HashMap<String, String>>,
) -> Response {
    match exchange(&s, &f).await {
        Ok(v) => Json(v).into_response(),
        Err(e) => (
            StatusCode::from_u16(e.status).unwrap_or(StatusCode::BAD_REQUEST),
            Json(json!({"error":e.code,"error_description":e.message})),
        )
            .into_response(),
    }
}
pub async fn revoke(
    State(s): State<Shared>,
    Form(f): Form<HashMap<String, String>>,
) -> Result<StatusCode, AppError> {
    let value = f.get("token").map(String::as_str).unwrap_or("");
    let grant = s
        .auth
        .decode::<Refresh>(value, "artifact-mcp-refresh")
        .filter(|c| c.iss == s.auth.config.origin)
        .map(|c| c.grant)
        .or_else(|| {
            s.auth
                .decode::<Access>(value, &resource(&s.auth))
                .filter(|c| c.iss == s.auth.config.origin)
                .map(|c| c.grant)
        });
    if let Some(id) = grant
        && let Some(mut d) = s.grants.get(&id).await?
        && d.data["client_id"] == f.get("client_id").map(String::as_str).unwrap_or("")
    {
        d.data["revoked"] = json!(true);
        s.grants.put(&id, d.data, Some(&d.update_time)).await?;
    }
    Ok(StatusCode::OK)
}
#[derive(Serialize, Deserialize)]
struct RevokeForm {
    owner: String,
    exp: u64,
    aud: String,
}
pub async fn connections(State(s): State<Shared>, h: HeaderMap) -> Result<Response, AppError> {
    let Some(p) = s.auth.principal(&h) else {
        return Ok(Redirect::to("/login").into_response());
    };
    let csrf = s.auth.sign(&RevokeForm {
        owner: p.id.clone(),
        exp: now() + 600,
        aud: "artifact-mcp-revoke".into(),
    });
    let mut body = String::from(
        "<p>Each connection can manage your artifacts for up to 30 days. Disconnecting stops access immediately.</p>",
    );
    let mut count = 0;
    for d in s.grants.list(&p.id).await? {
        if d["revoked"] != false || d["used"] != true || d["expires"].as_u64().unwrap_or(0) <= now()
        {
            continue;
        }
        count += 1;
        body.push_str(&format!("<section class=\"oauth-connection\"><h2>{}</h2><p class=\"fineprint\">Access expires in {} days</p><form method=\"post\" action=\"/oauth/disconnect\"><input type=\"hidden\" name=\"grant\" value=\"{}\"><input type=\"hidden\" name=\"csrf\" value=\"{}\"><button class=\"button secondary\">Disconnect</button></form></section>",esc(text(&d,"client_name")),d["expires"].as_u64().unwrap_or(0).saturating_sub(now()).div_ceil(86400),esc(text(&d,"id")),esc(&csrf)));
    }
    if count == 0 {
        body.push_str("<p>No connected agents.</p>");
    }
    Ok(page("Connected agents", &body).into_response())
}
pub async fn disconnect(
    State(s): State<Shared>,
    h: HeaderMap,
    Form(f): Form<HashMap<String, String>>,
) -> Result<Redirect, AppError> {
    origin(&s.auth, &h)?;
    let p = s.auth.require(&h)?;
    let csrf: RevokeForm = s
        .auth
        .decode(
            f.get("csrf").map(String::as_str).unwrap_or(""),
            "artifact-mcp-revoke",
        )
        .ok_or_else(|| error(403, "csrf", "Refresh the connected agents page."))?;
    if csrf.owner != p.id {
        return Err(error(403, "csrf", "Approval belongs to another publisher."));
    }
    let id = f.get("grant").map(String::as_str).unwrap_or("");
    let mut d = s.grants.get(id).await?.ok_or_else(invalid)?;
    if d.data["owner"] != p.id {
        return Err(error(
            403,
            "forbidden",
            "Grant belongs to another publisher.",
        ));
    }
    d.data["revoked"] = json!(true);
    s.grants.put(id, d.data, Some(&d.update_time)).await?;
    Ok(Redirect::to("/oauth/connections"))
}

pub fn router() -> axum::Router<Shared> {
    use axum::routing::{get, post};
    axum::Router::new()
        .route(
            "/.well-known/oauth-protected-resource",
            get(protected_metadata),
        )
        .route(
            "/.well-known/oauth-protected-resource/mcp",
            get(protected_metadata),
        )
        .route("/.well-known/oauth-authorization-server", get(metadata))
        .route("/oauth/register", post(register))
        .route("/oauth/authorize", get(authorize))
        .route("/oauth/consent", get(consent))
        .route("/oauth/approve", post(approve))
        .route("/oauth/token", post(token_endpoint))
        .route("/oauth/revoke", post(revoke))
        .route("/oauth/connections", get(connections))
        .route("/oauth/disconnect", post(disconnect))
        .layer(axum::extract::DefaultBodyLimit::max(24 * 1024))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn bearer_claims_reject_wrong_audience_issuer_expiry_and_tenant() {
        let tenant = "c166b9c4-5053-4eec-9665-aba0782d0804";
        let auth = Auth::new(crate::auth::Config {
            dev: false,
            origin: "https://artifact.invalid".into(),
            tenant: Some(tenant.into()),
            client: Some("fixture".into()),
            secret: Some("fixture".into()),
            gated_tenant: Some(tenant.into()),
            signing_key: Some(
                "synthetic-oauth-unit-test-material-never-production-00000000000000000".into(),
            ),
        });
        let mut c = Access {
            principal: Principal {
                id: format!("entra:{tenant}:fixture-owner"),
                display_name: "Fixture".into(),
                verified_email: None,
                authenticated: true,
            },
            grant: token(),
            exp: now() + 60,
            aud: resource(&auth),
            iss: auth.config.origin.clone(),
            scope: SCOPE.into(),
        };
        let valid = |c: &Access| {
            let mut h = HeaderMap::new();
            h.insert(
                header::AUTHORIZATION,
                format!("Bearer {}", auth.sign(c)).parse().unwrap(),
            );
            access_claim(&auth, &h).is_some()
        };
        assert!(valid(&c));
        c.aud = "artifact-session".into();
        assert!(!valid(&c));
        c.aud = resource(&auth);
        c.iss = "https://other.invalid".into();
        assert!(!valid(&c));
        c.iss = auth.config.origin.clone();
        c.exp = now() - 1;
        assert!(!valid(&c));
        c.exp = now() + 60;
        c.principal.id = "entra:wrong-tenant:owner".into();
        assert!(!valid(&c));
    }
}
