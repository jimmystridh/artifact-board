use artifact_board::{cloud_store::CloudStore, core::Principal, firestore_core::FirestoreCore};
use serde_json::json;
use std::collections::BTreeMap;
fn principal(id: &str) -> Principal {
    Principal {
        id: id.into(),
        display_name: id.into(),
        verified_email: Some(format!("{id}@example.test")),
        authenticated: true,
    }
}
#[tokio::test]
#[ignore = "requires local Firestore and GCS emulators"]
async fn firestore_domain_roundtrip() {
    let fs = std::env::var("FIRESTORE_EMULATOR_URL").unwrap_or("http://127.0.0.1:8088".into());
    let gcs = std::env::var("GCS_EMULATOR_URL").unwrap_or("http://127.0.0.1:4443".into());
    let store = CloudStore::emulator(
        "artifact-board-test",
        "(default)",
        "artifact-board-test",
        &fs,
        &gcs,
    )
    .unwrap();
    let core = FirestoreCore::new(store.clone());
    let p = principal("owner");
    let other = principal("other");
    let key = uuid::Uuid::new_v4().to_string();
    let s = core
        .create_share(&p, "Test", "static", "link-only", &key)
        .await
        .unwrap();
    let sid = s["id"].as_str().unwrap();
    let cap = s["capability"].as_str().unwrap();
    assert_eq!(
        core.create_share(&p, "Test", "static", "link-only", &key)
            .await
            .unwrap()["id"],
        s["id"]
    );
    assert_eq!(core.get_share(&other, sid).await.unwrap_err().status, 403);
    assert!(core.viewer_share(cap, None).await.is_err());
    assert_eq!(
        core.create_share(&p, "Different", "static", "link-only", &key)
            .await
            .unwrap_err()
            .code,
        "idempotency_conflict"
    );
    let gated = core
        .create_share(
            &p,
            "Gated",
            "static",
            "entra-gated",
            &uuid::Uuid::new_v4().to_string(),
        )
        .await
        .unwrap();
    let gu = core
        .create_upload(&p, gated["id"].as_str().unwrap())
        .await
        .unwrap();
    let gf = BTreeMap::from([("index.html".into(), b"private".to_vec())]);
    core.publish(&p, gu["id"].as_str().unwrap(), &gf, "index.html")
        .await
        .unwrap();
    let gc = gated["capability"].as_str().unwrap();
    assert_eq!(core.viewer_share(gc, None).await.unwrap_err().status, 401);
    assert_eq!(core.revision_files(gc, None, Some(&p)).await.unwrap(), gf);
    let u = core.create_upload(&p, sid).await.unwrap();
    let uid = u["id"].as_str().unwrap();
    let files = BTreeMap::from([(
        "index.html".into(),
        b"<h1 id='test'>Hello review</h1>".to_vec(),
    )]);
    core.stage_upload(&p, uid, &files, "index.html")
        .await
        .unwrap();
    let restarted = FirestoreCore::new(store.clone());
    assert_eq!(restarted.staged_files(&p, uid).await.unwrap().0, files);
    let published = restarted
        .publish(&p, uid, &files, "index.html")
        .await
        .unwrap();
    let rid = published["revision_id"].as_str().unwrap();
    assert_eq!(
        core.publish(&p, uid, &files, "index.html").await.unwrap()["revision_id"],
        rid
    );
    assert_eq!(core.revision_files(cap, None, None).await.unwrap(), files);
    let t = core
        .add_thread(
            cap,
            None,
            Some("banana"),
            "Please review",
            rid,
            json!({"kind":"text","path":"index.html","exact":"Hello review"}),
        )
        .await
        .unwrap();
    let tid = t["id"].as_str().unwrap();
    assert_eq!(t["comments"][0]["authenticated"], false);
    core.reply(cap, tid, Some(&other), None, "Reply")
        .await
        .unwrap();
    assert_eq!(
        core.threads(cap, None).await.unwrap()[0]["comments"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
    let (a, b) = tokio::join!(
        core.reply(cap, tid, Some(&other), None, "Concurrent A"),
        core.reply(cap, tid, Some(&p), None, "Concurrent B")
    );
    a.unwrap();
    b.unwrap();
    assert_eq!(
        core.threads(cap, None).await.unwrap()[0]["comments"]
            .as_array()
            .unwrap()
            .len(),
        4
    );
    let second = core.create_upload(&p, sid).await.unwrap();
    let revision2 = core
        .publish(&p, second["id"].as_str().unwrap(), &files, "index.html")
        .await
        .unwrap();
    let discussion = core.threads(cap, None).await.unwrap();
    assert_eq!(discussion[0]["original_revision"], rid);
    assert_eq!(
        discussion[0]["placements"][0]["revision_id"],
        revision2["revision_id"]
    );
    assert_eq!(discussion[0]["placements"][0]["status"], "likely");
    assert_eq!(
        core.viewer_revision(cap, Some(rid), None).await.unwrap()["id"],
        rid
    );
    core.resolve(&p, sid, tid, true).await.unwrap();
    assert_eq!(core.threads(cap, None).await.unwrap()[0]["resolved"], true);
    let batches = core.claim_due_notifications(i64::MAX).await.unwrap();
    assert!(!batches.as_array().unwrap().is_empty());
    let b = batches
        .as_array()
        .unwrap()
        .iter()
        .find(|b| b["share_id"] == sid)
        .unwrap();
    assert!(
        core.notification_eligible(b["id"].as_str().unwrap(), b["lease"].as_str().unwrap())
            .await
            .unwrap()
    );
    core.lifecycle(&p, sid, "delete").await.unwrap();
    assert!(core.viewer_share(cap, None).await.is_err());
    assert!(
        !core
            .notification_eligible(b["id"].as_str().unwrap(), b["lease"].as_str().unwrap())
            .await
            .unwrap()
    );
    core.lifecycle(&p, sid, "restore").await.unwrap();
    assert!(core.viewer_share(cap, None).await.is_err());
    core.lifecycle(&p, sid, "republish").await.unwrap();
    assert_eq!(core.revision_files(cap, None, None).await.unwrap(), files);
}

#[tokio::test]
#[ignore = "requires local Firestore and GCS emulators"]
async fn worker_publication_is_fenced_and_atomic() {
    use artifact_board::cloud_store::{Collection as C, Mutation, Precondition};
    let fs = std::env::var("FIRESTORE_EMULATOR_URL").unwrap_or("http://127.0.0.1:8088".into());
    let gcs = std::env::var("GCS_EMULATOR_URL").unwrap_or("http://127.0.0.1:4443".into());
    let store = CloudStore::emulator(
        "artifact-board-test",
        "(default)",
        "artifact-board-test",
        &fs,
        &gcs,
    )
    .unwrap();
    let core = FirestoreCore::new(store.clone());
    let p = principal("worker-owner");
    let token = store.runtime_token().await.unwrap();
    for invalidate in [false, true] {
        let s = core
            .create_share(
                &p,
                "Worker",
                "static",
                "link-only",
                &uuid::Uuid::new_v4().to_string(),
            )
            .await
            .unwrap();
        let sid = s["id"].as_str().unwrap();
        let u = core.create_upload(&p, sid).await.unwrap();
        let uid = u["id"].as_str().unwrap();
        let files = BTreeMap::from([("index.html".into(), b"job content".to_vec())]);
        core.stage_upload(&p, uid, &files, "index.html")
            .await
            .unwrap();
        let old = store
            .get(C::Uploads, uid, None, &token)
            .await
            .unwrap()
            .unwrap();
        let jid = uuid::Uuid::new_v4().to_string();
        let mut upload = old.data.clone();
        upload["build_job_id"] = json!(jid);
        store.commit(&[Mutation{collection:C::Uploads,id:uid.into(),data:upload,condition:Precondition::UpdateTime(old.update_time)},Mutation{collection:C::BuildJobs,id:jid.clone(),data:json!({"id":jid,"upload_id":uid,"share_id":sid,"state":"running","claim_id":"claim","lease_expires_at":4102444800_i64,"expected_share_version":0,"expected_latest_revision":null}),condition:Precondition::Create}],None,&token).await.unwrap();
        assert_eq!(
            core.stage_upload(&p, uid, &files, "index.html")
                .await
                .unwrap_err()
                .code,
            "upload_frozen"
        );
        assert_eq!(
            core.publish(&p, uid, &files, "index.html")
                .await
                .unwrap_err()
                .code,
            "upload_frozen"
        );
        assert_eq!(
            core.publish_for_job(&p, uid, &files, "index.html", &jid, "wrong-claim")
                .await
                .unwrap_err()
                .code,
            "build_lease_lost"
        );
        if invalidate {
            core.lifecycle(&p, sid, "unpublish").await.unwrap();
            assert_eq!(
                core.publish_for_job(&p, uid, &files, "index.html", &jid, "claim")
                    .await
                    .unwrap_err()
                    .code,
                "stale_build"
            );
            assert!(core.get_upload(&p, uid).await.unwrap()["revision_id"].is_null());
        } else {
            let result = core
                .publish_for_job(&p, uid, &files, "index.html", &jid, "claim")
                .await
                .unwrap();
            let job = store
                .get(C::BuildJobs, &jid, None, &token)
                .await
                .unwrap()
                .unwrap();
            assert_eq!(job.data["state"], "succeeded");
            assert_eq!(job.data["revision_id"], result["revision_id"]);
            assert_eq!(
                core.publish_for_job(&p, uid, &files, "index.html", &jid, "claim")
                    .await
                    .unwrap()["revision_id"],
                result["revision_id"]
            );
        }
    }
}

#[tokio::test]
#[ignore = "requires local Firestore and GCS emulators"]
async fn lifecycle_change_fences_non_react_preparation() {
    let fs = std::env::var("FIRESTORE_EMULATOR_URL").unwrap_or("http://127.0.0.1:8088".into());
    let gcs = std::env::var("GCS_EMULATOR_URL").unwrap_or("http://127.0.0.1:4443".into());
    let store = CloudStore::emulator(
        "artifact-board-test",
        "(default)",
        "artifact-board-test",
        &fs,
        &gcs,
    )
    .unwrap();
    let core = FirestoreCore::new(store);
    let p = principal("static-owner");
    let s = core
        .create_share(
            &p,
            "Static",
            "static",
            "link-only",
            &uuid::Uuid::new_v4().to_string(),
        )
        .await
        .unwrap();
    let sid = s["id"].as_str().unwrap();
    let u = core.create_upload(&p, sid).await.unwrap();
    let uid = u["id"].as_str().unwrap();
    let files = BTreeMap::from([("index.html".into(), b"static content".to_vec())]);
    let before = core.get_share(&p, sid).await.unwrap();
    let version = before["version"].as_u64().unwrap();
    core.lifecycle(&p, sid, "unpublish").await.unwrap();
    assert_eq!(
        core.publish_at_version(&p, uid, &files, "index.html", version)
            .await
            .unwrap_err()
            .code,
        "stale_build"
    );
    let after = core.get_share(&p, sid).await.unwrap();
    assert_eq!(after["state"], "unpublished");
    assert!(after["latest_revision"].is_null());
    assert!(core.get_upload(&p, uid).await.unwrap()["revision_id"].is_null());
    // A fresh explicit finalization may publish using the current lifecycle version.
    core.publish_at_version(
        &p,
        uid,
        &files,
        "index.html",
        after["version"].as_u64().unwrap(),
    )
    .await
    .unwrap();
    assert!(
        core.viewer_share(s["capability"].as_str().unwrap(), None)
            .await
            .unwrap()
            .get("version")
            .is_none()
    );
}

#[tokio::test]
#[ignore = "requires local Firestore and GCS emulators"]
async fn crashed_notification_sender_becomes_ambiguous_without_resend() {
    use artifact_board::cloud_store::Collection as C;
    let fs = std::env::var("FIRESTORE_EMULATOR_URL").unwrap_or("http://127.0.0.1:8088".into());
    let gcs = std::env::var("GCS_EMULATOR_URL").unwrap_or("http://127.0.0.1:4443".into());
    let store = CloudStore::emulator(
        "artifact-notifications-test",
        "(default)",
        "artifact-board-test",
        &fs,
        &gcs,
    )
    .unwrap();
    let core = FirestoreCore::new(store.clone());
    let p = principal("notification-owner");
    let s = core
        .create_share(
            &p,
            "Notifications",
            "static",
            "link-only",
            &uuid::Uuid::new_v4().to_string(),
        )
        .await
        .unwrap();
    let sid = s["id"].as_str().unwrap();
    let cap = s["capability"].as_str().unwrap();
    let u = core.create_upload(&p, sid).await.unwrap();
    let files = BTreeMap::from([("index.html".into(), b"review".to_vec())]);
    let r = core
        .publish(&p, u["id"].as_str().unwrap(), &files, "index.html")
        .await
        .unwrap();
    core.add_thread(
        cap,
        None,
        Some("banana"),
        "Review",
        r["revision_id"].as_str().unwrap(),
        json!({"kind":"general"}),
    )
    .await
    .unwrap();
    let restarted = FirestoreCore::new(store.clone());
    let pending = restarted.notification_status(&p, sid).await.unwrap();
    assert_eq!(pending[0]["state"], "pending");
    let due = pending[0]["due_at"].as_i64().unwrap();
    assert!(
        restarted
            .claim_due_notifications(due - 1)
            .await
            .unwrap()
            .as_array()
            .unwrap()
            .is_empty()
    );
    let claimed = restarted.claim_due_notifications(due).await.unwrap();
    let b = &claimed[0];
    let bid = b["id"].as_str().unwrap();
    let lease = b["lease"].as_str().unwrap();
    let expiry = b["lease_expires_at"].as_i64().unwrap();
    assert_eq!(expiry, due + 180);
    let token = store.runtime_token().await.unwrap();
    let before = store
        .query(C::BatchEvents, "batch_id", &json!(bid), None, &token)
        .await
        .unwrap()
        .len();
    assert_eq!(before, 1);
    let after_crash = FirestoreCore::new(store.clone());
    assert!(
        after_crash
            .reconcile_notification_claims(expiry - 1)
            .await
            .unwrap()
            .as_array()
            .unwrap()
            .is_empty()
    );
    let recovered = after_crash
        .reconcile_notification_claims(expiry)
        .await
        .unwrap();
    assert_eq!(recovered[0]["state"], "ambiguous");
    assert_eq!(
        after_crash.notification_status(&p, sid).await.unwrap()[0]["state"],
        "ambiguous"
    );
    assert!(!after_crash.notification_eligible(bid, lease).await.unwrap());
    assert!(
        after_crash
            .claim_due_notifications(expiry + 1000)
            .await
            .unwrap()
            .as_array()
            .unwrap()
            .is_empty()
    );
    assert!(
        after_crash
            .reconcile_notification_claims(expiry + 1000)
            .await
            .unwrap()
            .as_array()
            .unwrap()
            .is_empty()
    );
    assert!(
        after_crash
            .finish_notification(bid, lease, "delivered", None)
            .await
            .is_err()
    );
    assert_eq!(
        store
            .query(C::BatchEvents, "batch_id", &json!(bid), None, &token)
            .await
            .unwrap()
            .len(),
        before
    );
}
