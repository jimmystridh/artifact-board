use artifact_board::{auth::Config, server};
use axum::{
    body::{Body, to_bytes},
    http::{Request, StatusCode},
};
use serde_json::{Value, json};
use tower::ServiceExt;
struct Test {
    router: axum::Router,
    _dir: tempfile::TempDir,
}
impl Test {
    fn new(dev: bool) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let state = server::app(
            &dir.path().join("test.sqlite"),
            dir.path().join("stage"),
            Config {
                dev,
                origin: "http://127.0.0.1:4317".into(),
                tenant: None,
                client: None,
                secret: None,
                gated_tenant: None,
                signing_key: Some(
                    "synthetic-http-retry-test-key-not-production-000000000000000000000000".into(),
                ),
            },
        )
        .unwrap();
        Self {
            router: server::router(state),
            _dir: dir,
        }
    }
    fn cloud() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let fs = std::env::var("FIRESTORE_EMULATOR_URL").expect("explicit emulator required");
        let gcs = std::env::var("GCS_EMULATOR_URL").expect("explicit fake GCS required");
        let cloud = artifact_board::cloud_store::CloudStore::emulator(
            "demo-artifact-board",
            "(default)",
            "artifact-board-test",
            &fs,
            &gcs,
        )
        .unwrap();
        let state = server::cloud_app(
            cloud,
            Config {
                dev: true,
                origin: "http://127.0.0.1:4317".into(),
                tenant: None,
                client: None,
                secret: None,
                gated_tenant: None,
                signing_key: Some(
                    "synthetic-http-retry-test-key-not-production-000000000000000000000000".into(),
                ),
            },
        );
        Self {
            router: server::router(state),
            _dir: dir,
        }
    }
    async fn req(
        &self,
        method: &str,
        url: &str,
        cookie: &str,
        body: Value,
    ) -> (StatusCode, Value, Option<String>) {
        let response = self
            .router
            .clone()
            .oneshot(
                Request::builder()
                    .method(method)
                    .uri(url)
                    .header("cookie", cookie)
                    .header("x-artifact-request", "1")
                    .header("content-type", "application/json")
                    .body(Body::from(body.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        let status = response.status();
        let cookie = response
            .headers()
            .get("set-cookie")
            .map(|s| s.to_str().unwrap().split(';').next().unwrap().to_owned());
        let bytes = to_bytes(response.into_body(), 50_000_000).await.unwrap();
        (
            status,
            serde_json::from_slice(&bytes)
                .unwrap_or(json!({"html":String::from_utf8_lossy(&bytes)})),
            cookie,
        )
    }
    async fn login(&self, name: &str) -> String {
        let (s, _, c) = self
            .req("POST", "/dev/login", "", json!({"identity":name}))
            .await;
        assert_eq!(s, 200);
        c.unwrap()
    }
    async fn publish(&self, c: &str, policy: &str) -> (Value, Value) {
        let(s,share,_)=self.req("POST","/api/shares",c,json!({"title":"HTTP fixture","kind":"static","policy":policy,"idempotency_key":uuid::Uuid::new_v4()})).await;
        assert_eq!(s, 200, "{share}");
        let (s, u, _) = self
            .req(
                "POST",
                &format!("/api/shares/{}/uploads", share["id"].as_str().unwrap()),
                c,
                json!({}),
            )
            .await;
        assert_eq!(s, 200);
        let(s,v,_)=self.req("PUT",u["upload_url"].as_str().unwrap(),c,json!({"files":[{"path":"index.html","content":"<h1 id='one'>Hello review</h1><script>window.hostile=true</script>"},{"path":"asset.txt","content":"asset"}]})).await;
        assert_eq!(s, 200, "{v}");
        let (s, r, _) = self
            .req(
                "POST",
                &format!("/api/uploads/{}/finalize", u["id"].as_str().unwrap()),
                c,
                json!({"entrypoint":"index.html","idempotency_key":uuid::Uuid::new_v4(),"mutation_actor":"auth:dev-owner"}),
            )
            .await;
        assert_eq!(s, 200, "{r}");
        (share, r)
    }
}
#[tokio::test]
async fn production_auth_closed_and_csrf_enforced() {
    let t = Test::new(false);
    assert_eq!(t.req("POST", "/dev/login", "", json!({})).await.0, 404);
    assert_eq!(t.req("GET", "/api/shares", "", json!({})).await.0, 401);
    assert_eq!(t.req("GET", "/auth/login", "", json!({})).await.0, 503);
    let t = Test::new(true);
    let r = t
        .router
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/dev/login")
                .header("content-type", "application/json")
                .body(Body::from("{}"))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(r.status(), 403);
    let r = t
        .router
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/dev/login")
                .header("x-artifact-request", "1")
                .header("origin", "https://evil.invalid")
                .header("content-type", "application/json")
                .body(Body::from("{}"))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(r.status(), 403);
}
#[tokio::test]
async fn publication_guest_discussion_gate_and_owner_boundaries() {
    publication_case(Test::new(true)).await;
}
#[tokio::test]
#[ignore = "requires real local Firestore emulator and fake GCS"]
async fn firestore_http_publication_guest_discussion_gate_and_owner_boundaries() {
    publication_case(Test::cloud()).await;
}
async fn publication_case(t: Test) {
    let owner = t.login("owner").await;
    let reviewer = t.login("reviewer").await;
    let (share, r) = t.publish(&owner, "link-only").await;
    let cap = share["capability"].as_str().unwrap();
    let revision = r["revision_id"].as_str().unwrap();
    assert_eq!(
        t.req("GET", &format!("/s/{cap}"), "", json!({})).await.0,
        200
    );
    assert_eq!(
        t.req(
            "GET",
            &format!("/api/shares/{}", share["id"].as_str().unwrap()),
            &reviewer,
            json!({})
        )
        .await
        .0,
        403
    );
    let endpoint = format!("/api/view/{cap}/threads");
    let (_, identity, guest_cookie) = t
        .req("POST", &format!("/api/view/{cap}/identity"), "", json!({}))
        .await;
    let guest_cookie = guest_cookie.unwrap();
    let mutation_actor = identity["mutation_actor"].as_str().unwrap();
    assert_eq!(
        t.req(
            "POST",
            &endpoint,
            &guest_cookie,
            json!({"guest_name":" ","body":"Hello","revision":revision,"idempotency_key":"bad-name","mutation_actor":mutation_actor})
        )
        .await
        .0,
        400
    );
    let(s,thread,_)=t.req("POST",&endpoint,&guest_cookie,json!({"idempotency_key":"guest-thread","mutation_actor":mutation_actor,"guest_name":"banana","body":"<script>alert(1)</script>","revision":revision,"anchor":{"kind":"text","path":"index.html","exact":"Hello review"}})).await;
    assert_eq!(s, 200, "{thread}");
    assert_eq!(thread["comments"][0]["authenticated"], false);
    let resolve = format!(
        "/api/shares/{}/threads/{}/resolve",
        share["id"].as_str().unwrap(),
        thread["id"].as_str().unwrap()
    );
    assert_eq!(
        t.req("POST", &resolve, &reviewer, json!({"resolved":true}))
            .await
            .0,
        403
    );
    assert_eq!(
        t.req("POST", &resolve, &owner, json!({"resolved":true}))
            .await
            .0,
        200
    );
    let (gated, gr) = t.publish(&owner, "entra-gated").await;
    let gc = gated["capability"].as_str().unwrap();
    for url in [
        format!("/s/{gc}"),
        format!("/api/view/{gc}/threads"),
        format!(
            "/content/{gc}/{}/asset.txt",
            gr["revision_id"].as_str().unwrap()
        ),
    ] {
        assert_eq!(t.req("GET", &url, "", json!({})).await.0, 401);
        assert_eq!(t.req("GET", &url, &reviewer, json!({})).await.0, 200);
    }
    let (s, _, _) = t
        .req(
            "POST",
            &format!("/api/shares/{}/lifecycle", share["id"].as_str().unwrap()),
            &owner,
            json!({"action":"delete"}),
        )
        .await;
    assert_eq!(s, 200);
    assert_eq!(
        t.req("GET", &format!("/s/{cap}"), "", json!({})).await.0,
        404
    );
}
#[tokio::test]
async fn unsafe_upload_rejected_and_content_sandboxed() {
    let t = Test::new(true);
    let owner = t.login("owner").await;
    let (share, r) = t.publish(&owner, "link-only").await;
    let (s, u, _) = t
        .req(
            "POST",
            &format!("/api/shares/{}/uploads", share["id"].as_str().unwrap()),
            &owner,
            json!({}),
        )
        .await;
    assert_eq!(s, 200);
    assert_eq!(
        t.req(
            "PUT",
            u["upload_url"].as_str().unwrap(),
            &owner,
            json!({"files":[{"path":"../escape","content":"bad"}]})
        )
        .await
        .0,
        400
    );
    let url = format!(
        "/content/{}/{}/index.html",
        share["capability"].as_str().unwrap(),
        r["revision_id"].as_str().unwrap()
    );
    let response = t
        .router
        .clone()
        .oneshot(Request::builder().uri(&url).body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    let csp = response.headers()["content-security-policy"]
        .to_str()
        .unwrap();
    assert!(csp.contains("sandbox allow-scripts"));
    assert!(!csp.contains("allow-same-origin"));
    assert_eq!(response.headers()["referrer-policy"], "no-referrer");
}
#[tokio::test]
async fn mcp_parity_and_unauthenticated_rejection() {
    let t = Test::new(true);
    assert_eq!(
        t.req(
            "POST",
            "/mcp",
            "",
            json!({"jsonrpc":"2.0","id":1,"method":"tools/list"})
        )
        .await
        .0,
        401
    );
    let c = t.login("owner").await;
    let (s, v, _) = t
        .req(
            "POST",
            "/mcp",
            &c,
            json!({"jsonrpc":"2.0","id":1,"method":"tools/list"}),
        )
        .await;
    assert_eq!(s, 200);
    assert!(v["result"]["tools"].as_array().unwrap().len() >= 10);
    let(s,v,_)=t.req("POST","/mcp",&c,json!({"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"create_share","arguments":{"title":"MCP fixture","kind":"markdown","policy":"link-only","idempotency_key":"mcp-once"}}})).await;
    assert_eq!(s, 200);
    assert_eq!(v["result"]["isError"], false);
    assert_eq!(
        t.req("GET", "/api/shares", &c, json!({}))
            .await
            .1
            .as_array()
            .unwrap()
            .len(),
        1
    );
}

#[tokio::test]
async fn http_comment_ack_loss_actor_boundaries_and_restart() {
    comment_retry_case(Test::new(true), false).await;
}
#[tokio::test]
#[ignore = "requires real local Firestore emulator and fake GCS"]
async fn firestore_http_comment_ack_loss_actor_boundaries_and_restart() {
    comment_retry_case(Test::cloud(), true).await;
}
async fn comment_retry_case(mut t: Test, cloud: bool) {
    let owner = t.login("owner").await;
    let (share, revision) = t.publish(&owner, "link-only").await;
    let cap = share["capability"].as_str().unwrap();
    let identity_url = format!("/api/view/{cap}/identity");
    // Dropping initialization acknowledgement is safe: no mutation has occurred.
    let _lost_initialization = t.req("POST", &identity_url, "", json!({})).await;
    let (_, identity, cookie) = t.req("POST", &identity_url, "", json!({})).await;
    let cookie = cookie.unwrap();
    let actor = identity["mutation_actor"].as_str().unwrap();
    let url = format!("/api/view/{cap}/threads");
    let request = json!({"mutation_actor":actor,"idempotency_key":"lost-thread-response","guest_name":"Same display name","body":"Only one original","revision":revision["revision_id"],"anchor":{"kind":"general"}});
    assert_eq!(t.req("POST", &url, "", request.clone()).await.0, 409);
    assert_eq!(
        t.req("POST", &url, "artifact_guest=tampered", request.clone())
            .await
            .0,
        409
    );
    let first = t.req("POST", &url, &cookie, request.clone()).await;
    assert_eq!(first.0, 200, "{}", first.1);
    // Simulate lost mutation acknowledgement by retrying without consuming its result.
    let (a, b) = tokio::join!(
        t.req("POST", &url, &cookie, request.clone()),
        t.req("POST", &url, &cookie, request.clone())
    );
    assert_eq!(a.0, 200);
    assert_eq!(a.1, first.1);
    assert_eq!(b.1, first.1);
    let mut changed = request.clone();
    changed["body"] = json!("Changed body");
    assert_eq!(t.req("POST", &url, &cookie, changed).await.0, 409);
    let (_, other_identity, other_cookie) = t.req("POST", &identity_url, "", json!({})).await;
    assert_ne!(identity["mutation_actor"], other_identity["mutation_actor"]);
    assert_eq!(
        t.req("POST", &url, &other_cookie.unwrap(), request.clone())
            .await
            .0,
        409
    );
    let reply_url = format!("{}/{}/replies", url, first.1["id"].as_str().unwrap());
    let reply = json!({"mutation_actor":actor,"idempotency_key":"lost-reply-response","guest_name":"Same display name","body":"Only one reply"});
    let saved = t.req("POST", &reply_url, &cookie, reply.clone()).await;
    assert_eq!(saved.0, 200, "{}", saved.1);
    // A later mutation cannot change the original response returned by replay.
    let later = json!({"mutation_actor":"auth:dev-owner","idempotency_key":"later-owner-reply","body":"Later reply"});
    assert_eq!(t.req("POST", &reply_url, &owner, later).await.0, 200);
    let config = Config {
        dev: true,
        origin: "http://127.0.0.1:4317".into(),
        tenant: None,
        client: None,
        secret: None,
        gated_tenant: None,
        signing_key: Some(
            "synthetic-http-retry-test-key-not-production-000000000000000000000000".into(),
        ),
    };
    t.router = if cloud {
        let fresh = Test::cloud();
        fresh.router
    } else {
        server::router(
            server::app(
                &t._dir.path().join("test.sqlite"),
                t._dir.path().join("stage"),
                config,
            )
            .unwrap(),
        )
    };
    assert_eq!(
        t.req("POST", &reply_url, &cookie, reply.clone()).await.1,
        saved.1
    );
    assert_eq!(
        t.req("POST", &url, &cookie, request.clone()).await.1,
        first.1
    );
    let list = t.req("GET", &url, &cookie, json!({})).await.1;
    assert_eq!(list.as_array().unwrap().len(), 1);
    assert_eq!(list[0]["comments"].as_array().unwrap().len(), 3);
    let lifecycle = format!("/api/shares/{}/lifecycle", share["id"].as_str().unwrap());
    assert_eq!(
        t.req("POST", &lifecycle, &owner, json!({"action":"unpublish"}))
            .await
            .0,
        200
    );
    assert_eq!(t.req("POST", &reply_url, &cookie, reply).await.0, 404);
    assert_eq!(t.req("POST", &url, &cookie, request).await.0, 404);
}
