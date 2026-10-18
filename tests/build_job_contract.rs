// Unit and fake-server contracts live with the production build job coordinator.
use artifact_board::build_jobs::{CloudRunDispatcher, CloudRunJobConfig};
#[test]
fn production_dispatch_endpoint_is_fixed() {
    let d = CloudRunDispatcher::production(CloudRunJobConfig {
        project: "demo-project".into(),
        region: "europe-west1".into(),
        job: "builder".into(),
        container: "builder".into(),
        timeout_seconds: 120,
    })
    .unwrap();
    assert!(
        d.request_for_job("build-123")
            .unwrap()
            .url
            .starts_with("https://run.googleapis.com/v2/")
    );
}

#[tokio::test]
#[ignore = "requires explicitly started local Firestore emulator and fake GCS"]
async fn durable_build_claim_dispatch_success_failure_and_lifecycle() {
    use artifact_board::{
        build_jobs as jobs, cloud_store::CloudStore, core::Principal, firestore_core::FirestoreCore,
    };
    use axum::{Json, Router, extract::State, http::HeaderMap, routing::post};
    use serde_json::{Value, json};
    use std::{
        collections::BTreeMap,
        sync::{
            Arc,
            atomic::{AtomicUsize, Ordering},
        },
    };
    let store = CloudStore::emulator(
        "artifact-build-test",
        "(default)",
        "artifact-build-test",
        &std::env::var("FIRESTORE_EMULATOR_URL").unwrap(),
        &std::env::var("GCS_EMULATOR_URL").unwrap(),
    )
    .unwrap();
    let core = FirestoreCore::new(store.clone());
    let p = Principal {
        id: format!("build-owner-{}", uuid::Uuid::new_v4()),
        display_name: "Build test".into(),
        verified_email: None,
        authenticated: true,
    };
    let share = core
        .create_share(
            &p,
            "Durable React fixture",
            "react",
            "link-only",
            &uuid::Uuid::new_v4().to_string(),
        )
        .await
        .unwrap();
    let sid = share["id"].as_str().unwrap();
    let upload = core.create_upload(&p, sid).await.unwrap();
    let uid = upload["id"].as_str().unwrap();
    // Our synthetic script tests coordinator publication. The separate ingestion smoke test builds real React/Vite.
    let source=BTreeMap::from([
        ("index.html".into(), b"<h1>source placeholder</h1>".to_vec()),
        ("package.json".into(),br#"{"name":"durable-fixture","version":"1.0.0","scripts":{"build":"node build.cjs"}}"#.to_vec()),
        ("build.cjs".into(),b"const fs=require('fs');fs.mkdirSync('dist');fs.writeFileSync('dist/index.html','<h1>Durable source fixture</h1>');".to_vec()),
    ]);
    core.stage_upload(&p, uid, &source, "index.html")
        .await
        .unwrap();
    let job = jobs::enqueue(&core, &p, uid, "index.html").await.unwrap();
    let jid = job["id"].as_str().unwrap();
    assert_eq!(
        jobs::enqueue(&core, &p, uid, "index.html").await.unwrap()["id"],
        jid
    );
    assert_eq!(
        core.stage_upload(&p, uid, &source, "index.html")
            .await
            .unwrap_err()
            .status,
        409
    );
    let count = Arc::new(AtomicUsize::new(0));
    async fn fake(
        State(count): State<Arc<AtomicUsize>>,
        headers: HeaderMap,
        Json(body): Json<Value>,
    ) -> Json<Value> {
        assert_eq!(
            headers["authorization"],
            "Bearer local-fixture-no-credential"
        );
        assert_eq!(body["overrides"]["taskCount"], 1);
        count.fetch_add(1, Ordering::SeqCst);
        Json(json!({"name":"projects/demo-project/locations/europe-west1/operations/fixture-op"}))
    }
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let task = tokio::spawn(
        axum::serve(
            listener,
            Router::new()
                .route(
                    "/v2/projects/demo-project/locations/europe-west1/jobs/builder:run",
                    post(fake),
                )
                .with_state(count.clone()),
        )
        .into_future(),
    );
    let config = CloudRunJobConfig {
        project: "demo-project".into(),
        region: "europe-west1".into(),
        job: "builder".into(),
        container: "builder".into(),
        timeout_seconds: 120,
    };
    let dispatcher = CloudRunDispatcher::emulator(config, &format!("http://{address}")).unwrap();
    assert_eq!(
        jobs::dispatch(&core, jid, &dispatcher).await.unwrap()["dispatch_state"],
        "accepted"
    );
    jobs::dispatch(&core, jid, &dispatcher).await.unwrap();
    assert_eq!(count.load(Ordering::SeqCst), 1);
    // Invoke the actual application worker entrypoint in a separate process.
    let output = tokio::process::Command::new(env!("CARGO_BIN_EXE_artifact-board"))
        .arg("--dev-fixtures")
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        // Keep credentials scrubbed, but preserve the operator-selected runtime.
        .envs(
            std::env::var("ARTIFACT_NODE_BIN_DIR")
                .ok()
                .map(|path| ("ARTIFACT_NODE_BIN_DIR", path)),
        )
        .env("ARTIFACT_STORE", "firestore")
        .env("GOOGLE_CLOUD_PROJECT", "artifact-build-test")
        .env("ARTIFACT_BUCKET", "artifact-build-test")
        .env(
            "ARTIFACT_FIRESTORE_EMULATOR",
            std::env::var("FIRESTORE_EMULATOR_URL").unwrap(),
        )
        .env(
            "ARTIFACT_STORAGE_EMULATOR",
            std::env::var("GCS_EMULATOR_URL").unwrap(),
        )
        .env("ARTIFACT_BUILD_JOB_ID", jid)
        .env("ARTIFACT_BUILD_MODE", "trusted-host")
        .kill_on_drop(true)
        .output()
        .await
        .unwrap();
    assert!(
        output.status.success(),
        "Worker process failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let log = String::from_utf8_lossy(&output.stdout);
    assert!(
        !log.contains(share["capability"].as_str().unwrap()),
        "Worker must not log a viewer bearer capability"
    );
    let completed = jobs::status(&core, &p, jid).await.unwrap();
    assert_eq!(completed["state"], "succeeded", "{completed}");
    assert_eq!(completed["attempt"], 1);
    assert!(!completed["warnings"].as_array().unwrap().is_empty());
    let revision = completed["revision_id"].clone();
    assert_eq!(
        jobs::run_worker(&core, jid, "trusted-host").await.unwrap()["revision_id"],
        revision
    );
    // Source failure on a new upload must preserve the already published revision.
    let failing = core.create_upload(&p, sid).await.unwrap();
    let fail_uid = failing["id"].as_str().unwrap();
    core.stage_upload(&p, fail_uid, &source, "index.html")
        .await
        .unwrap();
    let fail_job = jobs::enqueue(&core, &p, fail_uid, "index.html")
        .await
        .unwrap();
    let failed = jobs::run_worker(&core, fail_job["id"].as_str().unwrap(), "container")
        .await
        .unwrap();
    assert_eq!(failed["state"], "failed");
    assert_eq!(
        core.get_share(&p, sid).await.unwrap()["latest_revision"],
        revision
    );
    // Lifecycle change after enqueue invalidates promotion even if the owned script succeeds.
    let stopped = core.create_upload(&p, sid).await.unwrap();
    let stop_uid = stopped["id"].as_str().unwrap();
    core.stage_upload(&p, stop_uid, &source, "index.html")
        .await
        .unwrap();
    let stop_job = jobs::enqueue(&core, &p, stop_uid, "index.html")
        .await
        .unwrap();
    core.lifecycle(&p, sid, "unpublish").await.unwrap();
    let stopped = jobs::run_worker(&core, stop_job["id"].as_str().unwrap(), "trusted-host")
        .await
        .unwrap();
    assert_eq!(stopped["state"], "failed", "{stopped}");
    assert_eq!(
        core.get_share(&p, sid).await.unwrap()["state"],
        "unpublished"
    );
    assert_eq!(
        core.get_share(&p, sid).await.unwrap()["latest_revision"],
        revision
    );
    // An ambiguous dispatch is never silently retried, and durable expiry fences execution.
    let stalled = core.create_upload(&p, sid).await.unwrap();
    let stalled_uid = stalled["id"].as_str().unwrap();
    core.stage_upload(&p, stalled_uid, &source, "index.html")
        .await
        .unwrap();
    let stalled_job = jobs::enqueue(&core, &p, stalled_uid, "index.html")
        .await
        .unwrap();
    let stalled_id = stalled_job["id"].as_str().unwrap();
    let bad_config = CloudRunJobConfig {
        project: "demo-project".into(),
        region: "europe-west1".into(),
        job: "missing-job".into(),
        container: "builder".into(),
        timeout_seconds: 120,
    };
    let bad_dispatcher =
        CloudRunDispatcher::emulator(bad_config, &format!("http://{address}")).unwrap();
    assert_eq!(
        jobs::dispatch(&core, stalled_id, &bad_dispatcher)
            .await
            .unwrap()["dispatch_state"],
        "ambiguous"
    );
    assert_eq!(
        jobs::dispatch(&core, stalled_id, &bad_dispatcher)
            .await
            .unwrap()["attempt"],
        0
    );
    let later = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64
        + 1000;
    assert_eq!(
        jobs::reconcile_expired(&core, stalled_id, later)
            .await
            .unwrap()["state"],
        "timed_out"
    );
    assert_eq!(
        jobs::run_worker(&core, stalled_id, "trusted-host")
            .await
            .unwrap()["state"],
        "timed_out"
    );
    let outsider = Principal {
        id: "not-owner".into(),
        display_name: "Outsider".into(),
        verified_email: None,
        authenticated: true,
    };
    assert_eq!(
        jobs::status(&core, &outsider, stalled_id)
            .await
            .unwrap_err()
            .status,
        403
    );
    task.abort();
}
