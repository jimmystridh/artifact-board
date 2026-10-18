//! Lost-ack/restart recovery exercises the production HTTP boundary, not a client cache.
use artifact_board::{auth::Config, server};
use axum::{
    body::{Body, to_bytes},
    http::{Request, StatusCode},
};
use serde_json::{Value, json};
use tower::ServiceExt;

fn router(path: &std::path::Path) -> axum::Router {
    server::router(
        server::app(
            &path.join("recover.sqlite"),
            path.join("staging"),
            Config {
                dev: true,
                origin: "http://127.0.0.1:4317".into(),
                tenant: None,
                client: None,
                secret: None,
                gated_tenant: None,
                signing_key: Some("finalize-recovery-test-signing-key-at-least-32-bytes".into()),
            },
        )
        .unwrap(),
    )
}

async fn request(
    app: &axum::Router,
    method: &str,
    url: &str,
    cookie: &str,
    body: Value,
) -> (StatusCode, Value, Option<String>) {
    let response = app
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
        .map(|c| c.to_str().unwrap().split(';').next().unwrap().to_owned());
    let bytes = to_bytes(response.into_body(), 50_000_000).await.unwrap();
    (status, serde_json::from_slice(&bytes).unwrap(), cookie)
}
async fn login(app: &axum::Router, identity: &str) -> String {
    let (status, _, cookie) =
        request(app, "POST", "/dev/login", "", json!({"identity":identity})).await;
    assert_eq!(status, 200);
    cookie.unwrap()
}
async fn share(app: &axum::Router, cookie: &str) -> Value {
    let (status, value, _) = request(app, "POST", "/api/shares", cookie, json!({"title":"Lost acknowledgement", "kind":"static", "policy":"link-only", "idempotency_key":uuid::Uuid::new_v4().to_string()})).await;
    assert_eq!(status, 200, "{value}");
    value
}
async fn staged(app: &axum::Router, cookie: &str, share: &Value, content: &str) -> Value {
    let (status, upload, _) = request(
        app,
        "POST",
        &format!("/api/shares/{}/uploads", share["id"].as_str().unwrap()),
        cookie,
        json!({}),
    )
    .await;
    assert_eq!(status, 200, "{upload}");
    let (status, result, _) = request(app, "PUT", upload["upload_url"].as_str().unwrap(), cookie, json!({"files":[{"path":"index.html","content":content},{"path":"other.html","content":"other entry"}]})).await;
    assert_eq!(status, 200, "{result}");
    upload
}
fn final_url(upload: &Value) -> String {
    format!("/api/uploads/{}/finalize", upload["id"].as_str().unwrap())
}
fn payload(key: &str, entry: &str) -> Value {
    json!({"entrypoint":entry,"idempotency_key":key,"mutation_actor":"auth:dev-owner"})
}

#[tokio::test]
async fn lost_ack_concurrent_replay_restart_and_later_revision_return_exact_receipt() {
    let dir = tempfile::tempdir().unwrap();
    let app = router(dir.path());
    let cookie = login(&app, "owner").await;
    let share = share(&app, &cookie).await;
    let upload = staged(&app, &cookie, &share, "<h1>first revision</h1>").await;
    let url = final_url(&upload);
    let body = payload("lost-ack", "index.html");
    let (a, b) = tokio::join!(
        request(&app, "POST", &url, &cookie, body.clone()),
        request(&app, "POST", &url, &cookie, body.clone())
    );
    let (status, receipt, _) = request(&app, "POST", &url, &cookie, body.clone()).await;
    assert_eq!(status, 200, "{receipt}");
    for response in [a, b] {
        if response.0 == 200 {
            assert_eq!(response.1, receipt);
        } else {
            assert_eq!(response.0, 409, "{}", response.1);
            assert_eq!(
                response.1["error"]["code"]
                    .as_str()
                    .or_else(|| response.1["code"].as_str()),
                Some("finalize_in_progress")
            );
        }
    }
    let (_, state, _) = request(
        &app,
        "GET",
        &format!("/api/shares/{}", share["id"].as_str().unwrap()),
        &cookie,
        json!({}),
    )
    .await;
    assert_eq!(state["revisions"].as_array().unwrap().len(), 1);
    drop(app);
    let app = router(dir.path());
    let cookie = login(&app, "owner").await;
    let (status, replay, _) = request(&app, "POST", &url, &cookie, body.clone()).await;
    assert_eq!(status, 200, "{replay}");
    assert_eq!(
        replay, receipt,
        "A restarted service must replay its persisted receipt"
    );
    let newer = staged(&app, &cookie, &share, "<h1>newer revision</h1>").await;
    let (status, new_receipt, _) = request(
        &app,
        "POST",
        &final_url(&newer),
        &cookie,
        payload("newer-revision", "index.html"),
    )
    .await;
    assert_eq!(status, 200, "{new_receipt}");
    assert_ne!(new_receipt["revision_id"], receipt["revision_id"]);
    let (status, replay, _) = request(&app, "POST", &url, &cookie, body).await;
    assert_eq!(status, 200);
    assert_eq!(
        replay, receipt,
        "Later share changes cannot replace the original operation receipt"
    );
}

#[tokio::test]
async fn finalize_rejects_changed_payload_key_target_and_actor_before_replay() {
    let dir = tempfile::tempdir().unwrap();
    let app = router(dir.path());
    let owner = login(&app, "owner").await;
    let share = share(&app, &owner).await;
    let upload = staged(&app, &owner, &share, "<h1>original</h1>").await;
    let url = final_url(&upload);
    let (status, result, _) = request(
        &app,
        "POST",
        &url,
        &owner,
        payload("bound-key", "index.html"),
    )
    .await;
    assert_eq!(status, 200, "{result}");
    let (status, conflict, _) = request(
        &app,
        "POST",
        &url,
        &owner,
        payload("bound-key", "other.html"),
    )
    .await;
    assert_eq!(status, 409, "{conflict}");
    let (status, conflict, _) = request(
        &app,
        "POST",
        &url,
        &owner,
        payload("different-key", "index.html"),
    )
    .await;
    assert_eq!(status, 409, "{conflict}");
    let other_upload = staged(&app, &owner, &share, "<h1>another upload</h1>").await;
    let (status, conflict, _) = request(
        &app,
        "POST",
        &final_url(&other_upload),
        &owner,
        payload("bound-key", "index.html"),
    )
    .await;
    assert_eq!(status, 409, "{conflict}");
    let mut wrong_actor = payload("bound-key", "index.html");
    wrong_actor["mutation_actor"] = json!("auth:dev-reviewer");
    assert_eq!(
        request(&app, "POST", &url, &owner, wrong_actor).await.0,
        409
    );
    let reviewer = login(&app, "reviewer").await;
    let mut reviewer_body = payload("bound-key", "index.html");
    reviewer_body["mutation_actor"] = json!("auth:dev-reviewer");
    assert_eq!(
        request(&app, "POST", &url, &reviewer, reviewer_body)
            .await
            .0,
        403
    );
    assert_eq!(
        request(
            &app,
            "POST",
            &url,
            &owner,
            json!({"entrypoint":"index.html","mutation_actor":"auth:dev-owner"})
        )
        .await
        .0,
        400
    );
    assert_eq!(
        request(
            &app,
            "POST",
            &url,
            &owner,
            payload(&"x".repeat(201), "index.html")
        )
        .await
        .0,
        400
    );
}

#[tokio::test]
async fn failed_preparation_freezes_original_source_across_restart() {
    let dir = tempfile::tempdir().unwrap();
    let app = router(dir.path());
    let cookie = login(&app, "owner").await;
    let share = share(&app, &cookie).await;
    let upload = staged(&app, &cookie, &share, "<h1>frozen source</h1>").await;
    let url = final_url(&upload);
    let body = payload("frozen-request", "missing.html");
    let (status, failure, _) = request(&app, "POST", &url, &cookie, body.clone()).await;
    assert_eq!(status, 422, "{failure}");
    drop(app);
    let app = router(dir.path());
    let cookie = login(&app, "owner").await;
    let (status, frozen, _) = request(
        &app,
        "PUT",
        upload["upload_url"].as_str().unwrap(),
        &cookie,
        json!({"files":[{"path":"missing.html","content":"changed source"}]}),
    )
    .await;
    assert_eq!(status, 409, "{frozen}");
    let (status, failure, _) = request(&app, "POST", &url, &cookie, body).await;
    assert_eq!(status, 422, "{failure}");
    let (_, state, _) = request(
        &app,
        "GET",
        &format!("/api/shares/{}", share["id"].as_str().unwrap()),
        &cookie,
        json!({}),
    )
    .await;
    assert!(state["revisions"].as_array().unwrap().is_empty());
}

#[tokio::test]
async fn receipt_replay_rechecks_current_owner_and_lifecycle_without_republishing() {
    let dir = tempfile::tempdir().unwrap();
    let app = router(dir.path());
    let cookie = login(&app, "owner").await;
    let share = share(&app, &cookie).await;
    let upload = staged(&app, &cookie, &share, "<h1>original</h1>").await;
    let url = final_url(&upload);
    let body = payload("lifecycle-replay", "index.html");
    let (status, receipt, _) = request(&app, "POST", &url, &cookie, body.clone()).await;
    assert_eq!(status, 200, "{receipt}");
    let lifecycle = format!("/api/shares/{}/lifecycle", share["id"].as_str().unwrap());
    let (status, archived, _) = request(
        &app,
        "POST",
        &lifecycle,
        &cookie,
        json!({"action":"archive"}),
    )
    .await;
    assert_eq!(status, 200, "{archived}");
    assert_eq!(
        request(&app, "POST", &url, &cookie, body.clone()).await.0,
        409
    );
    let (status, restored, _) = request(
        &app,
        "POST",
        &lifecycle,
        &cookie,
        json!({"action":"restore"}),
    )
    .await;
    assert_eq!(status, 200, "{restored}");
    let (status, replay, _) = request(&app, "POST", &url, &cookie, body).await;
    assert_eq!(status, 200, "{replay}");
    assert_eq!(replay, receipt);
    let (_, state, _) = request(
        &app,
        "GET",
        &format!("/api/shares/{}", share["id"].as_str().unwrap()),
        &cookie,
        json!({}),
    )
    .await;
    assert_eq!(
        state["state"], "unpublished",
        "Receipt replay must never publish again"
    );
    assert_eq!(state["revisions"].as_array().unwrap().len(), 1);
}

fn principal() -> artifact_board::core::Principal {
    artifact_board::core::Principal {
        id: "finalize-domain-owner".into(),
        display_name: "Owner".into(),
        verified_email: None,
        authenticated: true,
    }
}
fn source() -> std::collections::BTreeMap<String, Vec<u8>> {
    std::collections::BTreeMap::from([("index.html".into(), b"<h1>frozen bytes</h1>".to_vec())])
}

#[test]
fn pending_reservation_resumes_after_restart_and_original_receipt_is_atomic() {
    use artifact_board::core::Core;
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("pending.sqlite");
    let p = principal();
    let core = Core::open(&path).unwrap();
    let s = core
        .create_share(&p, "Pending", "static", "link-only", "pending")
        .unwrap();
    let u = core.create_upload(&p, s["id"].as_str().unwrap()).unwrap();
    let uid = u["id"].as_str().unwrap();
    core.stage_upload(&p, uid, &source(), "index.html").unwrap();
    let reservation = core
        .begin_finalize(&p, uid, "pending-key", "index.html")
        .unwrap();
    assert!(reservation["result"].is_null());
    drop(core);
    let core = Core::open(&path).unwrap();
    assert_eq!(
        core.begin_finalize(&p, uid, "pending-key", "index.html")
            .unwrap(),
        reservation
    );
    assert_eq!(core.staged_files(&p, uid).unwrap().0, source());
    let receipt = core
        .publish_finalized(
            &p,
            uid,
            "pending-key",
            &source(),
            "index.html",
            &json!(["original warning"]),
        )
        .unwrap();
    drop(core);
    let core = Core::open(&path).unwrap();
    assert_eq!(
        core.begin_finalize(&p, uid, "pending-key", "index.html")
            .unwrap()["result"],
        receipt
    );
    assert_eq!(
        core.publish_finalized(
            &p,
            uid,
            "pending-key",
            &source(),
            "index.html",
            &json!(["different later warning"])
        )
        .unwrap(),
        receipt
    );
    assert_eq!(
        core.get_share(&p, s["id"].as_str().unwrap()).unwrap()["revisions"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
}

#[test]
fn pending_local_reservation_cannot_rebase_after_lifecycle_roundtrip() {
    use artifact_board::core::Core;
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("version.sqlite");
    let p = principal();
    let core = Core::open(&path).unwrap();
    let s = core
        .create_share(&p, "Fence", "static", "link-only", "fence")
        .unwrap();
    let sid = s["id"].as_str().unwrap();
    let u = core.create_upload(&p, sid).unwrap();
    let uid = u["id"].as_str().unwrap();
    core.stage_upload(&p, uid, &source(), "index.html").unwrap();
    core.begin_finalize(&p, uid, "lifecycle-fence", "index.html")
        .unwrap();
    core.lifecycle(&p, sid, "archive").unwrap();
    core.lifecycle(&p, sid, "restore").unwrap();
    drop(core);
    let core = Core::open(&path).unwrap();
    assert_eq!(
        core.begin_finalize(&p, uid, "lifecycle-fence", "index.html")
            .unwrap_err()
            .code,
        "stale_build"
    );
    assert_eq!(
        core.publish_finalized(
            &p,
            uid,
            "lifecycle-fence",
            &source(),
            "index.html",
            &json!([])
        )
        .unwrap_err()
        .code,
        "stale_build"
    );
    assert!(core.get_share(&p, sid).unwrap()["latest_revision"].is_null());
}

#[test]
fn separate_local_connections_converge_on_one_reservation_and_receipt() {
    use artifact_board::core::Core;
    use std::sync::{Arc, Barrier};
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("concurrent.sqlite");
    let p = principal();
    let core = Core::open(&path).unwrap();
    let s = core
        .create_share(&p, "Concurrent", "static", "link-only", "concurrent")
        .unwrap();
    let u = core.create_upload(&p, s["id"].as_str().unwrap()).unwrap();
    let uid = u["id"].as_str().unwrap().to_owned();
    core.stage_upload(&p, &uid, &source(), "index.html")
        .unwrap();
    let a = Core::open(&path).unwrap();
    let b = Core::open(&path).unwrap();
    let barrier = Arc::new(Barrier::new(2));
    let handles = [a, b]
        .into_iter()
        .map(|c| {
            let barrier = barrier.clone();
            let p = p.clone();
            let uid = uid.clone();
            std::thread::spawn(move || {
                barrier.wait();
                c.begin_finalize(&p, &uid, "same-request", "index.html")
                    .unwrap();
                c.publish_finalized(
                    &p,
                    &uid,
                    "same-request",
                    &source(),
                    "index.html",
                    &json!([]),
                )
                .unwrap()
            })
        })
        .collect::<Vec<_>>();
    let receipts = handles
        .into_iter()
        .map(|h| h.join().unwrap())
        .collect::<Vec<_>>();
    assert_eq!(receipts[0], receipts[1]);
    assert_eq!(
        core.get_share(&p, s["id"].as_str().unwrap()).unwrap()["revisions"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
}

fn cloud_core() -> artifact_board::firestore_core::FirestoreCore {
    artifact_board::firestore_core::FirestoreCore::new(
        artifact_board::cloud_store::CloudStore::emulator(
            "demo-artifact-board",
            "(default)",
            "artifact-board-test",
            &std::env::var("FIRESTORE_EMULATOR_URL").expect("explicit emulator required"),
            &std::env::var("GCS_EMULATOR_URL").expect("explicit fake GCS required"),
        )
        .unwrap(),
    )
}

#[tokio::test]
#[ignore = "requires explicitly started local Firestore emulator and fake GCS"]
async fn firestore_finalization_concurrent_restart_payload_and_receipt_contract() {
    let core = cloud_core();
    let mut p = principal();
    p.id = uuid::Uuid::new_v4().to_string();
    let s = core
        .create_share(&p, "Cloud replay", "static", "link-only", "cloud-replay")
        .await
        .unwrap();
    let sid = s["id"].as_str().unwrap();
    let u = core.create_upload(&p, sid).await.unwrap();
    let uid = u["id"].as_str().unwrap();
    core.stage_upload(&p, uid, &source(), "index.html")
        .await
        .unwrap();
    let (a, b) = tokio::join!(
        core.begin_finalize(&p, uid, "cloud-request", "index.html"),
        core.begin_finalize(&p, uid, "cloud-request", "index.html")
    );
    assert_eq!(a.unwrap(), b.unwrap());
    assert_eq!(
        core.begin_finalize(&p, uid, "cloud-request", "changed.html")
            .await
            .unwrap_err()
            .code,
        "idempotency_conflict"
    );
    assert_eq!(
        core.begin_finalize(&p, uid, "different-key", "index.html")
            .await
            .unwrap_err()
            .code,
        "idempotency_conflict"
    );
    assert_eq!(
        core.stage_upload(&p, uid, &source(), "index.html")
            .await
            .unwrap_err()
            .code,
        "upload_frozen"
    );
    drop(core);
    let core = cloud_core();
    let files = core.staged_files(&p, uid).await.unwrap().0;
    let warnings = json!(["original"]);
    let (a, b) = tokio::join!(
        core.publish_finalized(&p, uid, "cloud-request", &files, "index.html", &warnings),
        core.publish_finalized(&p, uid, "cloud-request", &files, "index.html", &warnings)
    );
    let receipt = a.unwrap();
    assert_eq!(receipt, b.unwrap());
    let other = core.create_upload(&p, sid).await.unwrap();
    core.stage_upload(&p, other["id"].as_str().unwrap(), &source(), "index.html")
        .await
        .unwrap();
    assert_eq!(
        core.begin_finalize(
            &p,
            other["id"].as_str().unwrap(),
            "cloud-request",
            "index.html"
        )
        .await
        .unwrap_err()
        .code,
        "idempotency_conflict"
    );
    core.publish(&p, other["id"].as_str().unwrap(), &source(), "index.html")
        .await
        .unwrap();
    drop(core);
    let core = cloud_core();
    assert_eq!(
        core.begin_finalize(&p, uid, "cloud-request", "index.html")
            .await
            .unwrap()["result"],
        receipt
    );
    assert_eq!(
        core.publish_finalized(&p, uid, "cloud-request", &files, "index.html", &json!([]))
            .await
            .unwrap(),
        receipt
    );
    core.lifecycle(&p, sid, "archive").await.unwrap();
    assert_eq!(
        core.begin_finalize(&p, uid, "cloud-request", "index.html")
            .await
            .unwrap_err()
            .code,
        "state_conflict"
    );
    core.lifecycle(&p, sid, "restore").await.unwrap();
    assert_eq!(
        core.begin_finalize(&p, uid, "cloud-request", "index.html")
            .await
            .unwrap()["result"],
        receipt
    );
    assert_eq!(
        core.get_share(&p, sid).await.unwrap()["state"],
        "unpublished"
    );
    assert_eq!(
        core.get_share(&p, sid).await.unwrap()["revisions"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
}

#[tokio::test]
#[ignore = "requires explicitly started local Firestore emulator and fake GCS"]
async fn firestore_pending_finalization_retains_lifecycle_fence_after_restart() {
    let core = cloud_core();
    let mut p = principal();
    p.id = uuid::Uuid::new_v4().to_string();
    let s = core
        .create_share(&p, "Cloud fence", "static", "link-only", "cloud-fence")
        .await
        .unwrap();
    let sid = s["id"].as_str().unwrap();
    let u = core.create_upload(&p, sid).await.unwrap();
    let uid = u["id"].as_str().unwrap();
    core.stage_upload(&p, uid, &source(), "index.html")
        .await
        .unwrap();
    core.begin_finalize(&p, uid, "pending-cloud", "index.html")
        .await
        .unwrap();
    core.lifecycle(&p, sid, "archive").await.unwrap();
    core.lifecycle(&p, sid, "restore").await.unwrap();
    drop(core);
    let core = cloud_core();
    assert_eq!(
        core.begin_finalize(&p, uid, "pending-cloud", "index.html")
            .await
            .unwrap_err()
            .code,
        "stale_build"
    );
    assert_eq!(
        core.publish_finalized(
            &p,
            uid,
            "pending-cloud",
            &source(),
            "index.html",
            &json!([])
        )
        .await
        .unwrap_err()
        .code,
        "stale_build"
    );
    assert!(core.get_share(&p, sid).await.unwrap()["latest_revision"].is_null());
}

#[tokio::test]
#[ignore = "requires explicitly started local Firestore emulator and fake GCS"]
async fn firestore_react_acceptance_replay_returns_original_job_receipt() {
    use artifact_board::build_jobs;
    use axum::{Json, Router, extract::State, routing::post};
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };
    let core = cloud_core();
    let mut p = principal();
    p.id = uuid::Uuid::new_v4().to_string();
    let s = core
        .create_share(&p, "React receipt", "react", "link-only", "react-receipt")
        .await
        .unwrap();
    let sid = s["id"].as_str().unwrap();
    let u = core.create_upload(&p, sid).await.unwrap();
    let uid = u["id"].as_str().unwrap();
    core.stage_upload(&p, uid, &source(), "index.html")
        .await
        .unwrap();
    core.begin_finalize(&p, uid, "react-key", "index.html")
        .await
        .unwrap();
    let (a, b) = tokio::join!(
        build_jobs::enqueue_finalized(&core, &p, uid, "index.html", "react-key"),
        build_jobs::enqueue_finalized(&core, &p, uid, "index.html", "react-key")
    );
    let receipt = a.unwrap();
    assert_eq!(receipt, b.unwrap());
    assert_eq!(receipt["state"], "queued");
    let calls = Arc::new(AtomicUsize::new(0));
    async fn fake_dispatch(State(calls): State<Arc<AtomicUsize>>) -> Json<Value> {
        calls.fetch_add(1, Ordering::SeqCst);
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        Json(
            json!({"name":"projects/demo-project/locations/europe-west1/operations/finalize-race"}),
        )
    }
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let service = tokio::spawn(
        axum::serve(
            listener,
            Router::new()
                .route(
                    "/v2/projects/demo-project/locations/europe-west1/jobs/builder:run",
                    post(fake_dispatch),
                )
                .with_state(calls.clone()),
        )
        .into_future(),
    );
    let dispatcher = build_jobs::CloudRunDispatcher::emulator(
        build_jobs::CloudRunJobConfig {
            project: "demo-project".into(),
            region: "europe-west1".into(),
            job: "builder".into(),
            container: "builder".into(),
            timeout_seconds: 120,
        },
        &format!("http://{address}"),
    )
    .unwrap();
    let job_id = receipt["job_id"].as_str().unwrap();
    let (a, b) = tokio::join!(
        build_jobs::dispatch(&core, job_id, &dispatcher),
        build_jobs::dispatch(&core, job_id, &dispatcher)
    );
    assert!(a.is_ok(), "{a:?}");
    assert!(b.is_ok(), "{b:?}");
    assert_eq!(
        calls.load(Ordering::SeqCst),
        1,
        "Concurrent acceptance retries must claim dispatch only once"
    );
    build_jobs::dispatch(&core, job_id, &dispatcher)
        .await
        .unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    service.abort();
    assert_eq!(
        core.stage_upload(&p, uid, &source(), "index.html")
            .await
            .unwrap_err()
            .code,
        "upload_frozen"
    );
    drop(core);
    let core = cloud_core();
    assert_eq!(
        core.begin_finalize(&p, uid, "react-key", "index.html")
            .await
            .unwrap()["result"],
        receipt
    );
    core.lifecycle(&p, sid, "unpublish").await.unwrap();
    assert_eq!(
        build_jobs::enqueue_finalized(&core, &p, uid, "index.html", "react-key")
            .await
            .unwrap(),
        receipt
    );
    assert!(core.get_share(&p, sid).await.unwrap()["latest_revision"].is_null());
}

#[tokio::test]
async fn local_react_preparation_failure_is_a_durable_claim_fenced_receipt() {
    let dir = tempfile::tempdir().unwrap();
    let app = router(dir.path());
    let cookie = login(&app, "owner").await;
    let(status,s,_)=request(&app,"POST","/api/shares",&cookie,json!({"title":"Unconfirmed local source build","kind":"react","policy":"link-only","idempotency_key":"local-react-failure"})).await;
    assert_eq!(status, 200, "{s}");
    // Missing package.json fails without running any publisher script in either build mode.
    let upload = staged(&app, &cookie, &s, "<h1>source without package</h1>").await;
    let url = final_url(&upload);
    let body = payload("local-react-request", "index.html");
    let (status, failure, _) = request(&app, "POST", &url, &cookie, body.clone()).await;
    assert_eq!(status, 422, "{failure}");
    assert_eq!(
        failure["error"]["code"]
            .as_str()
            .or_else(|| failure["code"].as_str()),
        Some("preparation_failed")
    );
    let (status, retry, _) = request(&app, "POST", &url, &cookie, body).await;
    assert_eq!(status, 422, "{retry}");
    let (status, _, _) = request(
        &app,
        "PUT",
        upload["upload_url"].as_str().unwrap(),
        &cookie,
        json!({"files":[{"path":"index.html","content":"replacement"}]}),
    )
    .await;
    assert_eq!(status, 409);
}

fn expire_local_claim(core: &artifact_board::core::Core, uid: &str) {
    // Advance only the persisted fixture lease. No sleeping or production clock override.
    core.conn
        .lock()
        .unwrap()
        .execute(
            "UPDATE finalize_attempts SET lease_until=0 WHERE upload=?",
            [uid],
        )
        .unwrap();
}

#[test]
fn local_claim_restart_reclaim_and_stale_success_failure_are_fenced() {
    use artifact_board::core::{AppError, Core};
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("claims.sqlite");
    let p = principal();
    let core = Core::open(&path).unwrap();
    let s = core
        .create_share(&p, "Claims", "static", "link-only", "claims")
        .unwrap();
    let sid = s["id"].as_str().unwrap();
    let u = core.create_upload(&p, sid).unwrap();
    let uid = u["id"].as_str().unwrap();
    core.stage_upload(&p, uid, &source(), "index.html").unwrap();
    core.begin_finalize(&p, uid, "claim-key", "index.html")
        .unwrap();
    let first = core.claim_finalize(&p, uid, "claim-key").unwrap();
    let old = first["claim_id"].as_str().unwrap();
    assert_eq!(
        core.publish_finalized(&p, uid, "claim-key", &source(), "index.html", &json!([]))
            .unwrap_err()
            .code,
        "finalization_lease_lost",
        "Unclaimed publication must not bypass the processing fence"
    );
    drop(core);
    let core = Core::open(&path).unwrap();
    assert_eq!(
        core.claim_finalize(&p, uid, "claim-key").unwrap_err().code,
        "finalize_in_progress",
        "Restart must not drop a live claim"
    );
    expire_local_claim(&core, uid);
    let second = core.claim_finalize(&p, uid, "claim-key").unwrap();
    let current = second["claim_id"].as_str().unwrap();
    assert_ne!(old, current);
    let failure = AppError {
        status: 422,
        code: "preparation_failed".into(),
        message: "Original terminal preparation failure".into(),
    };
    assert_eq!(
        core.publish_claimed(
            &p,
            uid,
            "claim-key",
            old,
            &source(),
            "index.html",
            &json!([])
        )
        .unwrap_err()
        .code,
        "finalization_lease_lost"
    );
    assert_eq!(
        core.record_finalize_failure(&p, uid, "claim-key", old, &failure)
            .unwrap_err()
            .code,
        "finalization_lease_lost"
    );
    assert_eq!(
        core.record_finalize_failure(&p, uid, "claim-key", current, &failure)
            .unwrap_err()
            .message,
        failure.message
    );
    assert_eq!(
        core.publish_claimed(
            &p,
            uid,
            "claim-key",
            old,
            &source(),
            "index.html",
            &json!([])
        )
        .unwrap_err()
        .message,
        failure.message
    );
    drop(core);
    let core = Core::open(&path).unwrap();
    assert_eq!(
        core.begin_finalize(&p, uid, "claim-key", "index.html")
            .unwrap_err()
            .message,
        failure.message
    );
    assert_eq!(
        core.claim_finalize(&p, uid, "claim-key")
            .unwrap_err()
            .message,
        failure.message
    );
    assert!(core.get_share(&p, sid).unwrap()["latest_revision"].is_null());
}

#[test]
fn independent_local_claims_busy_release_and_winner_receipt_survive_stale_failures() {
    use artifact_board::core::{AppError, Core};
    use std::sync::{Arc, Barrier};
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("claim-race.sqlite");
    let p = principal();
    let core = Core::open(&path).unwrap();
    let s = core
        .create_share(&p, "Claim race", "static", "link-only", "claim-race")
        .unwrap();
    let sid = s["id"].as_str().unwrap();
    let u = core.create_upload(&p, sid).unwrap();
    let uid = u["id"].as_str().unwrap();
    core.stage_upload(&p, uid, &source(), "index.html").unwrap();
    core.begin_finalize(&p, uid, "claim-race-key", "index.html")
        .unwrap();
    let a = Core::open(&path).unwrap();
    let b = Core::open(&path).unwrap();
    let gate = Arc::new(Barrier::new(2));
    let handles = [a, b]
        .into_iter()
        .map(|c| {
            let gate = gate.clone();
            let p = p.clone();
            let uid = uid.to_owned();
            std::thread::spawn(move || {
                gate.wait();
                c.claim_finalize(&p, &uid, "claim-race-key")
            })
        })
        .collect::<Vec<_>>();
    let results = handles
        .into_iter()
        .map(|h| h.join().unwrap())
        .collect::<Vec<_>>();
    assert_eq!(results.iter().filter(|r| r.is_ok()).count(), 1);
    assert_eq!(
        results
            .iter()
            .filter_map(|r| r.as_ref().err())
            .next()
            .unwrap()
            .code,
        "finalize_in_progress"
    );
    let first = results.into_iter().find_map(Result::ok).unwrap();
    let first = first["claim_id"].as_str().unwrap();
    let busy = AppError {
        status: 503,
        code: "build_busy".into(),
        message: "Another source build is active".into(),
    };
    assert_eq!(
        core.record_finalize_failure(&p, uid, "claim-race-key", first, &busy)
            .unwrap_err()
            .status,
        503
    );
    let next = core.claim_finalize(&p, uid, "claim-race-key").unwrap();
    let next = next["claim_id"].as_str().unwrap();
    assert_ne!(first, next);
    assert_eq!(
        core.record_finalize_failure(&p, uid, "claim-race-key", first, &busy)
            .unwrap_err()
            .code,
        "finalization_lease_lost",
        "Old busy failure cannot release the new claim"
    );
    expire_local_claim(&core, uid);
    let winner = core.claim_finalize(&p, uid, "claim-race-key").unwrap();
    let winner = winner["claim_id"].as_str().unwrap();
    let receipt = core
        .publish_claimed(
            &p,
            uid,
            "claim-race-key",
            winner,
            &source(),
            "index.html",
            &json!(["winner"]),
        )
        .unwrap();
    let failed = AppError {
        status: 422,
        code: "preparation_failed".into(),
        message: "Stale failure".into(),
    };
    assert_eq!(
        core.record_finalize_failure(&p, uid, "claim-race-key", next, &failed)
            .unwrap(),
        receipt
    );
    assert_eq!(
        core.publish_claimed(
            &p,
            uid,
            "claim-race-key",
            next,
            &source(),
            "index.html",
            &json!(["stale"])
        )
        .unwrap(),
        receipt
    );
    assert_eq!(
        core.get_share(&p, sid).unwrap()["revisions"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
}
