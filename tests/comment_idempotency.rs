//! Same contract runs against the reference adapter and official Firestore emulator.
//! No live service credentials, cloud resources, or browser automation are used.
use artifact_board::{
    cloud_store::{CloudStore, Collection as C},
    core::{Core, Principal},
    firestore_core::FirestoreCore,
    store::Store,
};
use serde_json::json;
use std::{collections::BTreeMap, path::PathBuf, sync::Arc};

fn principal(id: &str) -> Principal {
    Principal {
        id: id.into(),
        display_name: id.into(),
        verified_email: Some(format!("{id}@example.test")),
        authenticated: true,
    }
}
struct Fixture {
    _dir: Option<tempfile::TempDir>,
    path: Option<PathBuf>,
    store: Store,
}
impl Fixture {
    fn local() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("comments.sqlite");
        Self {
            store: Store::Local(Core::open(&path).unwrap()),
            _dir: Some(dir),
            path: Some(path),
        }
    }
    fn firestore() -> Self {
        let fs = std::env::var("FIRESTORE_EMULATOR_URL").unwrap();
        let gcs = std::env::var("GCS_EMULATOR_URL").unwrap();
        let store = CloudStore::emulator(
            "artifact-board-test",
            "(default)",
            "artifact-board-test",
            &fs,
            &gcs,
        )
        .unwrap();
        Self {
            _dir: None,
            path: None,
            store: Store::Firestore(FirestoreCore::new(store)),
        }
    }
    fn restart(&self) -> Store {
        match &self.store {
            Store::Local(_) => Store::Local(Core::open(self.path.as_ref().unwrap()).unwrap()),
            Store::Firestore(s) => Store::Firestore(FirestoreCore::new(s.store.clone())),
        }
    }
}
async fn published(store: &Store, owner: &Principal, policy: &str) -> (String, String, String) {
    let share = store
        .create_share(
            owner,
            "Comment retry contract",
            "static",
            policy,
            &uuid::Uuid::new_v4().to_string(),
        )
        .await
        .unwrap();
    let sid = share["id"].as_str().unwrap().to_owned();
    let cap = share["capability"].as_str().unwrap().to_owned();
    let upload = store.create_upload(owner, &sid).await.unwrap();
    let files = BTreeMap::from([("index.html".to_owned(), b"<h1>Review</h1>".to_vec())]);
    let revision = store
        .publish(owner, upload["id"].as_str().unwrap(), &files, "index.html")
        .await
        .unwrap()["revision_id"]
        .as_str()
        .unwrap()
        .to_owned();
    (sid, cap, revision)
}
// Count authoritative persisted effects, not only the replayed client response.
async fn counts(store: &Store, sid: &str) -> (usize, usize, usize, usize) {
    match store {
        Store::Local(core) => {
            let c = core.conn.lock().unwrap();
            let comments = c.query_row("SELECT COUNT(*) FROM comments c JOIN threads t ON c.thread=t.id WHERE t.share=?", [sid], |r| r.get::<_, i64>(0)).unwrap();
            let events = c
                .query_row("SELECT COUNT(*) FROM events WHERE share=?", [sid], |r| {
                    r.get::<_, i64>(0)
                })
                .unwrap();
            let links = c.query_row("SELECT COUNT(*) FROM batch_events b JOIN events e ON e.id=b.event WHERE e.share=?", [sid], |r| r.get::<_, i64>(0)).unwrap();
            let receipts = c
                .query_row(
                    "SELECT COUNT(*) FROM comment_receipts WHERE share=?",
                    [sid],
                    |r| r.get::<_, i64>(0),
                )
                .unwrap();
            (
                comments as usize,
                events as usize,
                links as usize,
                receipts as usize,
            )
        }
        Store::Firestore(core) => {
            let token = core.store.runtime_token().await.unwrap();
            let threads = core
                .store
                .query(C::Threads, "share_id", &json!(sid), None, &token)
                .await
                .unwrap();
            let mut comments = 0;
            for t in threads {
                comments += core
                    .store
                    .query(C::Comments, "thread_id", &t.data["id"], None, &token)
                    .await
                    .unwrap()
                    .len();
            }
            let events = core
                .store
                .query(C::Events, "share_id", &json!(sid), None, &token)
                .await
                .unwrap();
            let mut links = 0;
            for e in &events {
                links += core
                    .store
                    .query(C::BatchEvents, "event_id", &e.data["id"], None, &token)
                    .await
                    .unwrap()
                    .len();
            }
            let receipts = core
                .store
                .query(C::Idempotency, "share_id", &json!(sid), None, &token)
                .await
                .unwrap()
                .iter()
                .filter(|d| d.data["kind"] == "comment")
                .count();
            (comments, events.len(), links, receipts)
        }
    }
}
async fn contract(f: Fixture) {
    let s = &f.store;
    let owner = principal("comment-owner");
    let (sid, cap, revision) = published(s, &owner, "link-only").await;
    let anchor = json!({"kind":"text", "exact":"Review", "path":"index.html"});
    let original = s
        .add_thread_idempotent(
            &cap,
            None,
            Some(" Guest "),
            "First",
            &revision,
            anchor.clone(),
            "same-key",
            "guest:alpha",
        )
        .await
        .unwrap();
    let tid = original["id"].as_str().unwrap();
    assert_eq!(
        original["_mutation"],
        json!({
            "idempotency_key":"same-key", "operation":"thread", "comment_id":original["comments"][0]["id"],
        })
    );
    assert_eq!(counts(s, &sid).await, (1, 1, 1, 1));
    assert_eq!(
        original,
        s.add_thread_idempotent(
            &cap,
            None,
            Some(" Guest "),
            "First",
            &revision,
            anchor.clone(),
            "same-key",
            "guest:alpha"
        )
        .await
        .unwrap()
    );
    assert_eq!(counts(s, &sid).await, (1, 1, 1, 1));

    // Exact raw fields are fingerprinted, including the untrimmed guest name.
    for (name, body, rev, a) in [
        (Some("Guest"), "First", revision.as_str(), anchor.clone()),
        (None, "First", revision.as_str(), anchor.clone()),
        (Some(" Guest "), "First ", revision.as_str(), anchor.clone()),
        (Some(" Guest "), "", revision.as_str(), anchor.clone()),
        (
            Some(" Guest "),
            "First",
            "different-revision",
            anchor.clone(),
        ),
        (
            Some(" Guest "),
            "First",
            revision.as_str(),
            json!({"kind":"general"}),
        ),
    ] {
        let error = s
            .add_thread_idempotent(&cap, None, name, body, rev, a, "same-key", "guest:alpha")
            .await
            .unwrap_err();
        assert_eq!(
            (error.status, error.code.as_str()),
            (409, "idempotency_conflict")
        );
    }
    // Operation is a namespace dimension, so a reply may reuse a thread key.
    let replied = s
        .reply_idempotent(
            &cap,
            tid,
            None,
            Some(" Guest "),
            "Reply",
            "same-key",
            "guest:alpha",
        )
        .await
        .unwrap();
    assert_eq!(replied["comments"].as_array().unwrap().len(), 2);
    let reply_comment = replied["comments"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["body"] == "Reply")
        .unwrap();
    assert_eq!(
        replied["_mutation"],
        json!({
            "idempotency_key":"same-key", "operation":"reply", "comment_id":reply_comment["id"],
        })
    );
    assert_ne!(
        replied["_mutation"]["comment_id"],
        original["_mutation"]["comment_id"]
    );
    assert_eq!(counts(s, &sid).await, (2, 2, 2, 2));
    let second = s
        .add_thread_idempotent(
            &cap,
            None,
            Some(" Guest "),
            "First",
            &revision,
            anchor.clone(),
            "same-key",
            "guest:beta",
        )
        .await
        .unwrap();
    assert_ne!(original["id"], second["id"]);
    for (thread, name, body) in [
        (tid, Some("Guest"), "Reply"),
        (tid, None, "Reply"),
        (tid, Some(" Guest "), "Reply "),
        (second["id"].as_str().unwrap(), Some(" Guest "), "Reply"),
    ] {
        let error = s
            .reply_idempotent(&cap, thread, None, name, body, "same-key", "guest:alpha")
            .await
            .unwrap_err();
        assert_eq!(
            (error.status, error.code.as_str()),
            (409, "idempotency_conflict")
        );
    }
    // Exact response must survive later replies, resolution and repository reconstruction.
    s.reply(&cap, tid, None, Some("Third"), "Later comment")
        .await
        .unwrap();
    s.resolve(&owner, &sid, tid, true).await.unwrap();
    let before = counts(s, &sid).await;
    let restarted = f.restart();
    assert_eq!(
        original,
        restarted
            .add_thread_idempotent(
                &cap,
                None,
                Some(" Guest "),
                "First",
                &revision,
                anchor.clone(),
                "same-key",
                "guest:alpha"
            )
            .await
            .unwrap()
    );
    assert_eq!(
        replied,
        restarted
            .reply_idempotent(
                &cap,
                tid,
                None,
                Some(" Guest "),
                "Reply",
                "same-key",
                "guest:alpha"
            )
            .await
            .unwrap()
    );
    assert_eq!(counts(&restarted, &sid).await, before);

    // Authenticated principals cannot substitute one another's scope, even below HTTP.
    let alice = principal("alice-idem");
    let bob = principal("bob-idem");
    let a = s
        .add_thread_idempotent(
            &cap,
            Some(&alice),
            None,
            "Signed in",
            &revision,
            json!({"kind":"general"}),
            "signed-key",
            "auth:alice-idem",
        )
        .await
        .unwrap();
    let b = s
        .add_thread_idempotent(
            &cap,
            Some(&bob),
            None,
            "Signed in",
            &revision,
            json!({"kind":"general"}),
            "signed-key",
            "auth:bob-idem",
        )
        .await
        .unwrap();
    assert_ne!(a["id"], b["id"]);
    assert_eq!(
        s.add_thread_idempotent(
            &cap,
            Some(&bob),
            None,
            "Signed in",
            &revision,
            json!({"kind":"general"}),
            "signed-key",
            "auth:alice-idem"
        )
        .await
        .unwrap_err()
        .status,
        403
    );
    assert_eq!(
        s.add_thread_idempotent(
            &cap,
            None,
            Some("Guest"),
            "Signed in",
            &revision,
            json!({"kind":"general"}),
            "signed-key",
            "auth:alice-idem"
        )
        .await
        .unwrap_err()
        .status,
        400
    );

    // A key from another share never reveals or returns that share's response.
    let (sid2, cap2, revision2) = published(s, &owner, "link-only").await;
    let cross = s
        .add_thread_idempotent(
            &cap2,
            None,
            Some(" Guest "),
            "First",
            &revision2,
            anchor.clone(),
            "same-key",
            "guest:alpha",
        )
        .await
        .unwrap();
    assert_ne!(original["id"], cross["id"]);
    assert_eq!(cross["share_id"], sid2);
    assert_eq!(counts(s, &sid2).await, (1, 1, 1, 1));
    let before = counts(s, &sid).await;
    for (key, scope) in [
        ("".to_owned(), "guest:alpha".to_owned()),
        ("x".repeat(201), "guest:alpha".to_owned()),
        ("key".into(), "guest:".into()),
        ("key".into(), "guest:".to_owned() + &"x".repeat(507)),
        ("bad\nkey".into(), "guest:alpha".into()),
    ] {
        assert_eq!(
            s.reply_idempotent(&cap, tid, None, Some("Guest"), "Reply", &key, &scope)
                .await
                .unwrap_err()
                .status,
            400
        );
    }
    assert_eq!(counts(s, &sid).await, before);
    s.lifecycle(&owner, &sid, "unpublish").await.unwrap();
    assert_eq!(
        restarted
            .add_thread_idempotent(
                &cap,
                None,
                Some(" Guest "),
                "First",
                &revision,
                anchor,
                "same-key",
                "guest:alpha"
            )
            .await
            .unwrap_err()
            .status,
        404
    );
    assert_eq!(
        restarted
            .reply_idempotent(
                &cap,
                tid,
                None,
                Some(" Guest "),
                "Reply",
                "same-key",
                "guest:alpha"
            )
            .await
            .unwrap_err()
            .status,
        404
    );
    assert_eq!(counts(s, &sid).await, before);

    let (gated_sid, gated_cap, gated_rev) = published(s, &owner, "entra-gated").await;
    let signed = s
        .add_thread_idempotent(
            &gated_cap,
            Some(&alice),
            None,
            "Gated",
            &gated_rev,
            json!({}),
            "gated-key",
            "auth:alice-idem",
        )
        .await
        .unwrap();
    let mut invalid_alice = alice.clone();
    invalid_alice.authenticated = false;
    assert_eq!(
        s.add_thread_idempotent(
            &gated_cap,
            Some(&invalid_alice),
            None,
            "Gated",
            &gated_rev,
            json!({}),
            "gated-key",
            "auth:alice-idem"
        )
        .await
        .unwrap_err()
        .status,
        401
    );
    s.lifecycle(&owner, &gated_sid, "delete").await.unwrap();
    assert_eq!(
        s.reply_idempotent(
            &gated_cap,
            signed["id"].as_str().unwrap(),
            Some(&alice),
            None,
            "Denied",
            "new-key",
            "auth:alice-idem"
        )
        .await
        .unwrap_err()
        .status,
        404
    );
}
async fn concurrent_contract(f: Fixture) {
    let owner = principal("concurrent-owner");
    let (sid, cap, revision) = published(&f.store, &owner, "link-only").await;
    // Separate connections/instances ensure we exercise storage locking, not one Mutex.
    let a = Arc::new(f.restart());
    let b = Arc::new(f.restart());
    let barrier = Arc::new(tokio::sync::Barrier::new(2));
    let mut tasks = vec![];
    for s in [a.clone(), b.clone()] {
        let (cap, revision, barrier) = (cap.clone(), revision.clone(), barrier.clone());
        tasks.push(tokio::spawn(async move {
            barrier.wait().await;
            s.add_thread_idempotent(
                &cap,
                None,
                Some("Guest"),
                "Concurrent",
                &revision,
                json!({"kind":"general"}),
                "concurrent-key",
                "guest:concurrent",
            )
            .await
            .unwrap()
        }));
    }
    let first = tasks.remove(0).await.unwrap();
    assert_eq!(first, tasks.remove(0).await.unwrap());
    assert_eq!(counts(&f.store, &sid).await, (1, 1, 1, 1));
    let tid = first["id"].as_str().unwrap().to_owned();
    let mut tasks = vec![];
    for s in [a, b] {
        let (cap, tid, barrier) = (cap.clone(), tid.clone(), barrier.clone());
        tasks.push(tokio::spawn(async move {
            barrier.wait().await;
            s.reply_idempotent(
                &cap,
                &tid,
                None,
                Some("Guest"),
                "Concurrent reply",
                "concurrent-reply",
                "guest:concurrent",
            )
            .await
            .unwrap()
        }));
    }
    let reply = tasks.remove(0).await.unwrap();
    assert_eq!(reply, tasks.remove(0).await.unwrap());
    assert_eq!(counts(&f.store, &sid).await, (2, 2, 2, 2));
    assert_eq!(
        reply,
        f.restart()
            .reply_idempotent(
                &cap,
                &tid,
                None,
                Some("Guest"),
                "Concurrent reply",
                "concurrent-reply",
                "guest:concurrent"
            )
            .await
            .unwrap()
    );
}
async fn response_limit_contract(f: Fixture) {
    let s = &f.store;
    let owner = principal("limit-owner");
    let (sid, cap, revision) = published(s, &owner, "link-only").await;
    let thread = s
        .add_thread(&cap, None, Some("Guest"), "Root", &revision, json!({}))
        .await
        .unwrap();
    let tid = thread["id"].as_str().unwrap();
    let mut projected = thread.clone();
    for _ in 0..25 {
        projected = s
            .reply(&cap, tid, None, Some("Guest"), &"x".repeat(20_000))
            .await
            .unwrap();
    }
    // UUIDs/guest identifiers have fixed lengths. Project a response that fits exactly
    // without acknowledgement metadata: only the durable _mutation field puts it over.
    let mut prospective_comment = projected["comments"][0].clone();
    prospective_comment["body"] = json!("x");
    projected["comments"]
        .as_array_mut()
        .unwrap()
        .push(prospective_comment);
    let padding = 512 * 1024 - projected.to_string().len() + 1;
    assert!((1..=20_000).contains(&padding));
    let body = "x".repeat(padding);
    projected["comments"]
        .as_array_mut()
        .unwrap()
        .last_mut()
        .unwrap()["body"] = json!(body);
    assert_eq!(projected.to_string().len(), 512 * 1024);
    let before = counts(s, &sid).await;
    let error = s
        .reply_idempotent(
            &cap,
            tid,
            None,
            Some("Guest"),
            &body,
            "limit-key",
            "guest:limit",
        )
        .await
        .unwrap_err();
    assert_eq!(
        (error.status, error.code.as_str()),
        (413, "comment_response_limit")
    );
    assert_eq!(
        counts(s, &sid).await,
        before,
        "failed receipt must roll back comment/events/notifications"
    );
}
#[tokio::test]
async fn local_comment_receipts_are_exact_durable_scoped_and_authorized() {
    contract(Fixture::local()).await;
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn local_comment_receipts_deduplicate_independent_concurrent_connections() {
    concurrent_contract(Fixture::local()).await;
}
#[tokio::test]
async fn local_comment_receipt_size_failure_rolls_back_all_effects() {
    response_limit_contract(Fixture::local()).await;
}
#[tokio::test]
#[ignore = "requires official Firestore emulator and local fake GCS fixture"]
async fn firestore_comment_receipts_are_exact_durable_scoped_and_authorized() {
    contract(Fixture::firestore()).await;
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires official Firestore emulator and local fake GCS fixture"]
async fn firestore_comment_receipts_deduplicate_independent_concurrent_connections() {
    concurrent_contract(Fixture::firestore()).await;
}
#[tokio::test]
#[ignore = "requires official Firestore emulator and local fake GCS fixture"]
async fn firestore_comment_receipt_size_failure_rolls_back_all_effects() {
    response_limit_contract(Fixture::firestore()).await;
}

#[tokio::test]
async fn local_comment_acknowledgement_uses_inserted_id_instead_of_last_sorted_comment() {
    let f = Fixture::local();
    let owner = principal("comment-sort-owner");
    let (_, cap, revision) = published(&f.store, &owner, "link-only").await;
    let original = f
        .store
        .add_thread(&cap, None, Some("Guest"), "Earlier", &revision, json!({}))
        .await
        .unwrap();
    assert!(original.get("_mutation").is_none());
    let tid = original["id"].as_str().unwrap();
    let Store::Local(core) = &f.store else {
        unreachable!()
    };
    // The new comment is not the last item when an older timestamp is ahead of the clock.
    core.conn
        .lock()
        .unwrap()
        .execute(
            "UPDATE comments SET created=? WHERE thread=?",
            rusqlite::params![i64::MAX, tid],
        )
        .unwrap();
    let result = f
        .store
        .reply_idempotent(
            &cap,
            tid,
            None,
            Some("Guest"),
            "Inserted",
            "sort-key",
            "guest:sort",
        )
        .await
        .unwrap();
    let comments = result["comments"].as_array().unwrap();
    assert_eq!(comments.last().unwrap()["body"], "Earlier");
    assert_eq!(
        result["_mutation"]["comment_id"],
        comments.iter().find(|c| c["body"] == "Inserted").unwrap()["id"]
    );
    assert_ne!(
        result["_mutation"]["comment_id"],
        comments.last().unwrap()["id"]
    );
    assert_eq!(
        result,
        f.restart()
            .reply_idempotent(
                &cap,
                tid,
                None,
                Some("Guest"),
                "Inserted",
                "sort-key",
                "guest:sort"
            )
            .await
            .unwrap()
    );
}
