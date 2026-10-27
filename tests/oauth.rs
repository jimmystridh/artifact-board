use artifact_board::{auth::Config, server};
use axum::{
    body::{Body, to_bytes},
    http::{Request, StatusCode},
};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tower::ServiceExt;
const ORIGIN: &str = "http://127.0.0.1:4317";
const CALLBACK: &str = "http://127.0.0.1:39871/callback";
const VERIFIER: &str = "synthetic-test-code-verifier-00000000000000000000000";
struct Test {
    state: server::Shared,
    router: axum::Router,
    dir: tempfile::TempDir,
}
struct Reply {
    status: StatusCode,
    headers: axum::http::HeaderMap,
    body: String,
}
impl Reply {
    fn json(&self) -> Value {
        serde_json::from_str(&self.body).unwrap()
    }
    fn location(&self) -> &str {
        self.headers["location"].to_str().unwrap()
    }
}
impl Test {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let state = server::app(
            &dir.path().join("test.sqlite"),
            dir.path().join("stage"),
            config(),
        )
        .unwrap();
        let router = server::router(state.clone());
        Self { state, router, dir }
    }
    async fn req(&self, method: &str, path: &str, headers: &[(&str, &str)], body: &str) -> Reply {
        let mut r = Request::builder().method(method).uri(path);
        for (k, v) in headers {
            r = r.header(*k, *v);
        }
        let r = self
            .router
            .clone()
            .oneshot(r.body(Body::from(body.to_owned())).unwrap())
            .await
            .unwrap();
        let status = r.status();
        let headers = r.headers().clone();
        let body =
            String::from_utf8(to_bytes(r.into_body(), usize::MAX).await.unwrap().to_vec()).unwrap();
        Reply {
            status,
            headers,
            body,
        }
    }
    async fn form(&self, path: &str, pairs: &[(&str, &str)], cookie: &str) -> Reply {
        let body = url::form_urlencoded::Serializer::new(String::new())
            .extend_pairs(pairs.iter().copied())
            .finish();
        self.req(
            "POST",
            path,
            &[
                ("content-type", "application/x-www-form-urlencoded"),
                ("origin", ORIGIN),
                ("cookie", cookie),
            ],
            &body,
        )
        .await
    }
    async fn register(&self) -> String {
        let r=self.req("POST","/oauth/register",&[("content-type","application/json")],&json!({"client_name":"Codex test","redirect_uris":[CALLBACK],"token_endpoint_auth_method":"none"}).to_string()).await;
        assert_eq!(r.status, StatusCode::CREATED);
        r.json()["client_id"].as_str().unwrap().to_owned()
    }
    async fn authorize(&self, client: &str) -> (String, String, String) {
        self.authorize_callback(client, CALLBACK).await
    }
    async fn authorize_callback(&self, client: &str, callback: &str) -> (String, String, String) {
        let q = url::form_urlencoded::Serializer::new(String::new())
            .extend_pairs([
                ("response_type", "code"),
                ("client_id", client),
                ("redirect_uri", callback),
                ("state", "synthetic-state"),
                ("code_challenge_method", "S256"),
                (
                    "code_challenge",
                    &URL_SAFE_NO_PAD.encode(Sha256::digest(VERIFIER.as_bytes())),
                ),
                ("resource", &format!("{ORIGIN}/mcp")),
            ])
            .finish();
        let r = self
            .req("GET", &format!("/oauth/authorize?{q}"), &[], "")
            .await;
        assert_eq!(r.status, StatusCode::SEE_OTHER);
        let cookie = r.headers["set-cookie"]
            .to_str()
            .unwrap()
            .split(';')
            .next()
            .unwrap()
            .to_owned();
        let location = r.location().to_owned();
        let request = url::Url::parse(&format!("{ORIGIN}{location}"))
            .unwrap()
            .query_pairs()
            .find(|(k, _)| k == "request")
            .unwrap()
            .1
            .to_string();
        (request, cookie, location)
    }
    async fn code(&self, client: &str) -> String {
        self.code_callback(client, CALLBACK).await
    }
    async fn code_callback(&self, client: &str, callback: &str) -> String {
        let (request, cookie, location) = self.authorize_callback(client, callback).await;
        let login = self.state.auth.dev_login("owner").unwrap();
        let cookie = format!("{cookie}; {}", login.split(';').next().unwrap());
        let r = self.req("GET", &location, &[("cookie", &cookie)], "").await;
        assert_eq!(r.status, StatusCode::OK);
        assert!(r.body.contains("Connect agent"));
        assert_eq!(r.headers["referrer-policy"], "strict-origin");
        assert!(
            r.headers["content-security-policy"]
                .to_str()
                .unwrap()
                .contains(&format!(
                    "form-action 'self' {}",
                    url::Url::parse(callback)
                        .unwrap()
                        .origin()
                        .ascii_serialization()
                ))
        );
        let r = self
            .form(
                "/oauth/approve",
                &[("request", &request), ("decision", "allow")],
                &cookie,
            )
            .await;
        assert_eq!(r.status, StatusCode::SEE_OTHER);
        let u = url::Url::parse(r.location()).unwrap();
        let q: std::collections::HashMap<_, _> = u.query_pairs().collect();
        assert_eq!(q["state"], "synthetic-state");
        assert_eq!(q["iss"], ORIGIN);
        q["code"].to_string()
    }
    async fn exchange(&self, client: &str, code: &str, verifier: &str) -> Reply {
        self.exchange_callback(client, code, verifier, CALLBACK)
            .await
    }
    async fn exchange_callback(
        &self,
        client: &str,
        code: &str,
        verifier: &str,
        callback: &str,
    ) -> Reply {
        self.form(
            "/oauth/token",
            &[
                ("client_id", client),
                ("grant_type", "authorization_code"),
                ("code", code),
                ("redirect_uri", callback),
                ("code_verifier", verifier),
                ("resource", &format!("{ORIGIN}/mcp")),
            ],
            "",
        )
        .await
    }
    async fn refresh(&self, client: &str, refresh: &str) -> Reply {
        self.form(
            "/oauth/token",
            &[
                ("client_id", client),
                ("grant_type", "refresh_token"),
                ("refresh_token", refresh),
            ],
            "",
        )
        .await
    }
    async fn rpc(&self, access: &str, body: Value) -> Reply {
        self.req(
            "POST",
            "/mcp",
            &[
                ("authorization", &format!("Bearer {access}")),
                ("content-type", "application/json"),
            ],
            &body.to_string(),
        )
        .await
    }
}
fn config() -> Config {
    Config {
        dev: true,
        origin: ORIGIN.into(),
        tenant: None,
        client: None,
        secret: None,
        gated_tenant: None,
        signing_key: Some(
            "synthetic-oauth-test-signing-material-never-production-0000000000000000".into(),
        ),
    }
}
#[tokio::test]
async fn discovery_registration_consent_pkce_and_mcp_work_without_browser_cookie() {
    let t = Test::new();
    let r = t.req("GET", "/mcp", &[], "").await;
    assert_eq!(r.status, StatusCode::UNAUTHORIZED);
    assert!(
        r.headers["www-authenticate"]
            .to_str()
            .unwrap()
            .contains("oauth-protected-resource/mcp")
    );
    let r = t
        .req("GET", "/.well-known/oauth-authorization-server", &[], "")
        .await;
    assert_eq!(
        r.json()["code_challenge_methods_supported"],
        json!(["S256"])
    );
    let c = t.register().await;
    let code = t.code(&c).await;
    assert_eq!(
        t.exchange(&c, &code, "wrong").await.status,
        StatusCode::BAD_REQUEST
    );
    let r = t.exchange(&c, &code, VERIFIER).await;
    assert_eq!(r.status, StatusCode::OK);
    let tokens = r.json();
    let access = tokens["access_token"].as_str().unwrap();
    assert_eq!(
        t.exchange(&c, &code, VERIFIER).await.status,
        StatusCode::BAD_REQUEST
    );
    let r=t.rpc(access,json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-03-26"}})).await;
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(r.json()["result"]["serverInfo"]["name"], "artifact-board");
    assert_eq!(
        t.rpc(
            access,
            json!({"jsonrpc":"2.0","method":"notifications/initialized"})
        )
        .await
        .status,
        StatusCode::ACCEPTED
    );
    let r = t
        .rpc(
            access,
            json!({"jsonrpc":"2.0","id":2,"method":"tools/list"}),
        )
        .await;
    assert_eq!(r.json()["result"]["tools"].as_array().unwrap().len(), 16);
    assert_eq!(
        t.req(
            "GET",
            "/",
            &[("authorization", &format!("Bearer {access}"))],
            ""
        )
        .await
        .status,
        StatusCode::FORBIDDEN
    );
    let r = t
        .req(
            "POST",
            "/mcp",
            &[
                ("authorization", &format!("Bearer {access}")),
                ("content-type", "application/json"),
                ("origin", "https://attacker.invalid"),
            ],
            "{}",
        )
        .await;
    assert_eq!(r.status, StatusCode::FORBIDDEN);
    let newstate = server::app(
        &t.dir.path().join("test.sqlite"),
        t.dir.path().join("stage"),
        config(),
    )
    .unwrap();
    let reopened = Test {
        router: server::router(newstate.clone()),
        state: newstate,
        dir: tempfile::tempdir().unwrap(),
    };
    assert_eq!(
        reopened
            .rpc(access, json!({"jsonrpc":"2.0","id":3,"method":"ping"}))
            .await
            .status,
        StatusCode::OK
    );
    let r=t.rpc(access,json!({"jsonrpc":"2.0","id":4,"method":"tools/call","params":{"name":"create_share","arguments":{"title":"OAuth fixture","kind":"static","policy":"link-only","idempotency_key":"oauth-fixture-create"}}})).await;
    assert_eq!(r.json()["result"]["isError"], false);
    let share = r.json()["result"]["structuredContent"]["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let r=t.rpc(access,json!({"jsonrpc":"2.0","id":5,"method":"tools/call","params":{"name":"create_upload","arguments":{"share_id":share}}})).await;
    let upload = r.json()["result"]["structuredContent"].clone();
    let r = t
        .req(
            "PUT",
            upload["upload_url"].as_str().unwrap(),
            &[
                ("authorization", &format!("Bearer {access}")),
                ("content-type", "application/json"),
            ],
            &json!({"files":[{"path":"index.html","content":"<h1>OAuth works</h1>"}]}).to_string(),
        )
        .await;
    assert_eq!(r.status, StatusCode::OK);
    let r=t.rpc(access,json!({"jsonrpc":"2.0","id":6,"method":"tools/call","params":{"name":"upload_files","arguments":{"upload_id":upload["id"],"files":[{"path":"index.html","content":"<h1>MCP upload</h1>"}]}}})).await;
    assert_eq!(r.json()["result"]["isError"], false);
    let r=t.rpc(access,json!({"jsonrpc":"2.0","id":7,"method":"tools/call","params":{"name":"upload_files","arguments":{"upload_id":upload["id"],"files":[{"path":"../escape.html","content":"bad"}]}}})).await;
    assert_eq!(r.json()["result"]["isError"], true);
    let identity = t
        .rpc(
            access,
            json!({"id":8,"method":"tools/call","params":{"name":"get_identity","arguments":{}}}),
        )
        .await
        .json();
    let r=t.rpc(access,json!({"id":9,"method":"tools/call","params":{"name":"finalize_upload","arguments":{"upload_id":upload["id"],"entrypoint":"index.html","idempotency_key":"oauth-finalize-fixture","mutation_actor":identity["result"]["structuredContent"]["mutation_actor"]}}})).await;
    assert_eq!(r.json()["result"]["isError"], false);
}
#[tokio::test]
async fn refresh_rotation_reuse_detection_revokes_the_entire_grant() {
    let t = Test::new();
    let c = t.register().await;
    let code = t.code(&c).await;
    let r = t.exchange(&c, &code, VERIFIER).await;
    let old = r.json();
    let r = t.refresh(&c, old["refresh_token"].as_str().unwrap()).await;
    assert_eq!(r.status, StatusCode::OK);
    let new = r.json();
    assert_ne!(old["refresh_token"], new["refresh_token"]);
    assert_eq!(
        t.refresh(&c, old["refresh_token"].as_str().unwrap())
            .await
            .status,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        t.rpc(
            new["access_token"].as_str().unwrap(),
            json!({"id":1,"method":"ping"})
        )
        .await
        .status,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        t.refresh(&c, new["refresh_token"].as_str().unwrap())
            .await
            .status,
        StatusCode::BAD_REQUEST
    );
}
#[tokio::test]
async fn approvals_bind_browser_origin_and_client_callback() {
    let t = Test::new();
    let c = t.register().await;
    let (request, cookie, location) = t.authorize(&c).await;
    let r = t.req("GET", &location, &[("cookie", &cookie)], "").await;
    assert_eq!(r.status, StatusCode::SEE_OTHER);
    assert!(r.location().starts_with("/auth/login?next="));
    let login = t.state.auth.dev_login("owner").unwrap();
    assert_eq!(
        t.form(
            "/oauth/approve",
            &[("request", &request), ("decision", "allow")],
            &login
        )
        .await
        .status,
        StatusCode::FORBIDDEN
    );
    let body = url::form_urlencoded::Serializer::new(String::new())
        .extend_pairs([("request", request.as_str()), ("decision", "allow")])
        .finish();
    assert_eq!(
        t.req(
            "POST",
            "/oauth/approve",
            &[
                ("content-type", "application/x-www-form-urlencoded"),
                ("cookie", &format!("{cookie}; {login}")),
                ("origin", "https://attacker.invalid")
            ],
            &body
        )
        .await
        .status,
        StatusCode::FORBIDDEN
    );
    for uri in [
        "http://127.0.0.1.attacker.invalid/callback",
        "https://attacker.invalid/callback",
        "http://127.0.0.1/callback?next=evil",
        "http://user@localhost/callback",
        "http://127.0.0.2/callback",
    ] {
        let r = t
            .req(
                "POST",
                "/oauth/register",
                &[("content-type", "application/json")],
                &json!({"redirect_uris":[uri]}).to_string(),
            )
            .await;
        assert_eq!(r.status, StatusCode::BAD_REQUEST);
    }
}
#[tokio::test]
async fn concurrent_code_exchanges_issue_only_one_token_pair_and_revocation_is_immediate() {
    let t = Test::new();
    let c = t.register().await;
    let code = t.code(&c).await;
    let (a, b) = tokio::join!(
        t.exchange(&c, &code, VERIFIER),
        t.exchange(&c, &code, VERIFIER)
    );
    assert!(matches!(
        (a.status, b.status),
        (StatusCode::OK, StatusCode::BAD_REQUEST) | (StatusCode::BAD_REQUEST, StatusCode::OK)
    ));
    let tokens = if a.status == StatusCode::OK {
        a.json()
    } else {
        b.json()
    };
    assert_eq!(
        t.form(
            "/oauth/revoke",
            &[
                ("client_id", &c),
                ("token", tokens["refresh_token"].as_str().unwrap())
            ],
            ""
        )
        .await
        .status,
        StatusCode::OK
    );
    assert_eq!(
        t.rpc(
            tokens["access_token"].as_str().unwrap(),
            json!({"id":1,"method":"ping"})
        )
        .await
        .status,
        StatusCode::UNAUTHORIZED
    );
}

#[tokio::test]
async fn connected_agents_page_only_shows_owners_grants_and_can_disconnect() {
    let t = Test::new();
    let c = t.register().await;
    let code = t.code(&c).await;
    let tokens = t.exchange(&c, &code, VERIFIER).await.json();
    let owner = t.state.auth.dev_login("owner").unwrap();
    let reviewer = t.state.auth.dev_login("reviewer").unwrap();
    let r = t
        .req("GET", "/oauth/connections", &[("cookie", &reviewer)], "")
        .await;
    assert!(r.body.contains("No connected agents"));
    let r = t
        .req("GET", "/oauth/connections", &[("cookie", &owner)], "")
        .await;
    assert!(r.body.contains("Codex test"));
    let hidden = |name: &str| {
        r.body
            .split(&format!("name=\"{name}\" value=\""))
            .nth(1)
            .unwrap()
            .split('"')
            .next()
            .unwrap()
            .to_owned()
    };
    let grant = hidden("grant");
    let csrf = hidden("csrf");
    assert_eq!(
        t.form(
            "/oauth/disconnect",
            &[("grant", &grant), ("csrf", &csrf)],
            &reviewer
        )
        .await
        .status,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        t.form(
            "/oauth/disconnect",
            &[("grant", &grant), ("csrf", &csrf)],
            &owner
        )
        .await
        .status,
        StatusCode::SEE_OTHER
    );
    assert_eq!(
        t.rpc(
            tokens["access_token"].as_str().unwrap(),
            json!({"id":1,"method":"ping"})
        )
        .await
        .status,
        StatusCode::UNAUTHORIZED
    );
}

#[tokio::test]
#[ignore = "requires official Firestore emulator and local fake GCS fixture"]
async fn firestore_grants_survive_instances_and_enforce_atomic_rotation() {
    let mut t = Test::new();
    let cloud = artifact_board::cloud_store::CloudStore::emulator(
        "demo-artifact-board",
        "(default)",
        "artifact-board-test",
        &std::env::var("FIRESTORE_EMULATOR_URL").unwrap(),
        &std::env::var("GCS_EMULATOR_URL").unwrap(),
    )
    .unwrap();
    t.state = server::cloud_app(cloud.clone(), config());
    t.router = server::router(t.state.clone());
    let c = t.register().await;
    let code = t.code(&c).await;
    let (a, b) = tokio::join!(
        t.exchange(&c, &code, VERIFIER),
        t.exchange(&c, &code, VERIFIER)
    );
    assert!(matches!(
        (a.status, b.status),
        (StatusCode::OK, StatusCode::BAD_REQUEST) | (StatusCode::BAD_REQUEST, StatusCode::OK)
    ));
    let tokens = if a.status == StatusCode::OK {
        a.json()
    } else {
        b.json()
    };
    t.state = server::cloud_app(cloud, config());
    t.router = server::router(t.state.clone());
    assert_eq!(
        t.rpc(
            tokens["access_token"].as_str().unwrap(),
            json!({"id":1,"method":"ping"})
        )
        .await
        .status,
        StatusCode::OK
    );
    let r = t
        .refresh(&c, tokens["refresh_token"].as_str().unwrap())
        .await;
    assert_eq!(r.status, StatusCode::OK);
    let owner = t.state.auth.dev_login("owner").unwrap();
    assert!(
        t.req("GET", "/oauth/connections", &[("cookie", &owner)], "")
            .await
            .body
            .contains("Codex test")
    );
    assert_eq!(
        t.refresh(&c, tokens["refresh_token"].as_str().unwrap())
            .await
            .status,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        t.rpc(
            r.json()["access_token"].as_str().unwrap(),
            json!({"id":2,"method":"ping"})
        )
        .await
        .status,
        StatusCode::UNAUTHORIZED
    );
}

#[tokio::test]
async fn web_clients_complete_pkce_consent_exchange_refresh_and_mcp_access() {
    let t = Test::new();
    for callback in [
        "https://claude.ai/api/mcp/auth_callback",
        "https://chatgpt.com/connector_platform_oauth_redirect",
        "https://chatgpt.com/connector/oauth/callback-test_123",
    ] {
        let registration=t.req("POST","/oauth/register",&[("content-type","application/json")],&json!({"client_name":"Web MCP client","redirect_uris":[callback],"token_endpoint_auth_method":"none"}).to_string()).await;
        assert_eq!(
            registration.status,
            StatusCode::CREATED,
            "{}",
            registration.body
        );
        let client = registration.json()["client_id"]
            .as_str()
            .unwrap()
            .to_owned();
        let code = t.code_callback(&client, callback).await;
        assert_eq!(
            t.exchange(&client, &code, VERIFIER).await.status,
            StatusCode::BAD_REQUEST
        );
        let tokens = t
            .exchange_callback(&client, &code, VERIFIER, callback)
            .await;
        assert_eq!(tokens.status, StatusCode::OK, "{}", tokens.body);
        let tokens = tokens.json();
        assert_eq!(
            t.exchange_callback(&client, &code, VERIFIER, callback)
                .await
                .status,
            StatusCode::BAD_REQUEST
        );
        let tools = t
            .rpc(
                tokens["access_token"].as_str().unwrap(),
                json!({"jsonrpc":"2.0","id":1,"method":"tools/list"}),
            )
            .await;
        assert_eq!(tools.status, StatusCode::OK);
        assert_eq!(
            tools.json()["result"]["tools"].as_array().unwrap().len(),
            16
        );
        assert_eq!(
            t.refresh(&client, tokens["refresh_token"].as_str().unwrap())
                .await
                .status,
            StatusCode::OK
        );
    }
}
#[tokio::test]
async fn web_callback_registration_rejects_lookalikes_paths_ports_queries_and_credentials() {
    let t = Test::new();
    for callback in [
        "http://claude.ai/api/mcp/auth_callback",
        "https://claude.ai.attacker.invalid/api/mcp/auth_callback",
        "https://claude.ai/api/mcp/auth_callback/extra",
        "https://claude.ai/api/mcp/auth_callback?next=https://attacker.invalid",
        "https://claude.ai/api/mcp/auth_callback#fragment",
        "https://user@claude.ai/api/mcp/auth_callback",
        "https://claude.ai:444/api/mcp/auth_callback",
        "https://claude.ai/other/../api/mcp/auth_callback",
        "https://chatgpt.com.attacker.invalid/connector_platform_oauth_redirect",
        "https://chatgpt.com/connector_platform_oauth_redirect?next=evil",
        "https://chatgpt.com/connector/oauth/",
        "https://chatgpt.com/connector/oauth/id/extra",
        "https://chatgpt.com/connector/oauth/../evil",
        "https://chatgpt.com/connector/oauth/%2e%2e",
        "https://chatgpt.com/connector/oauth/id#evil",
        "https://attacker.invalid/callback",
    ] {
        let r = t
            .req(
                "POST",
                "/oauth/register",
                &[("content-type", "application/json")],
                &json!({"redirect_uris":[callback]}).to_string(),
            )
            .await;
        assert_eq!(r.status, StatusCode::BAD_REQUEST, "accepted {callback}");
    }
    let client = t
        .req(
            "POST",
            "/oauth/register",
            &[("content-type", "application/json")],
            &json!({"redirect_uris":["https://claude.ai/api/mcp/auth_callback"]}).to_string(),
        )
        .await
        .json()["client_id"]
        .as_str()
        .unwrap()
        .to_owned();
    let q = url::form_urlencoded::Serializer::new(String::new())
        .extend_pairs([
            ("client_id", client.as_str()),
            (
                "redirect_uri",
                "https://chatgpt.com/connector_platform_oauth_redirect",
            ),
        ])
        .finish();
    assert_eq!(
        t.req("GET", &format!("/oauth/authorize?{q}"), &[], "")
            .await
            .status,
        StatusCode::BAD_REQUEST
    );
}
