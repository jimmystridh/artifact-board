use super::*;
fn principal(name: &str) -> Principal {
    Principal {
        id: name.into(),
        display_name: name.into(),
        verified_email: Some(format!("{name}@example.test")),
        authenticated: true,
    }
}
fn fixture() -> (Core, Principal, Value) {
    let c = Core::open(Path::new(":memory:")).unwrap();
    let p = principal("owner");
    let s = c
        .create_share(&p, "Test", "static", "link-only", "one")
        .unwrap();
    (c, p, s)
}
fn publish(c: &Core, p: &Principal, s: &Value, text: &str) -> Value {
    let u = c.create_upload(p, s["id"].as_str().unwrap()).unwrap();
    c.publish(
        p,
        u["id"].as_str().unwrap(),
        &BTreeMap::from([("index.html".into(), text.as_bytes().to_vec())]),
        "index.html",
    )
    .unwrap()
}
#[test]
fn ownership_and_explicit_policy() {
    let (c, p, s) = fixture();
    let other = principal("other");
    assert_eq!(
        c.get_share(&other, s["id"].as_str().unwrap())
            .unwrap_err()
            .status,
        403
    );
    assert!(c.create_share(&p, "Test", "static", "", "x").is_err());
    let mut anon = p.clone();
    anon.authenticated = false;
    assert!(c.list_shares(&anon).is_err());
    assert!(c.create_upload(&other, s["id"].as_str().unwrap()).is_err());
    assert!(
        c.viewer_share(s["capability"].as_str().unwrap(), None)
            .is_err()
    );
}
#[test]
fn create_and_publish_idempotency() {
    let (c, p, s) = fixture();
    assert_eq!(
        s,
        c.create_share(&p, "Test", "static", "link-only", "one")
            .unwrap()
    );
    assert_eq!(
        c.create_share(&p, "Other", "static", "link-only", "one")
            .unwrap_err()
            .status,
        409
    );
    let u = c.create_upload(&p, s["id"].as_str().unwrap()).unwrap();
    let files = BTreeMap::from([("index.html".into(), b"first".to_vec())]);
    let a = c
        .publish(&p, u["id"].as_str().unwrap(), &files, "index.html")
        .unwrap();
    let b = c
        .publish(&p, u["id"].as_str().unwrap(), &files, "index.html")
        .unwrap();
    assert_eq!(a["revision_id"], b["revision_id"]);
    assert_eq!(
        c.get_share(&p, s["id"].as_str().unwrap()).unwrap()["revisions"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
}
#[test]
fn gate_covers_assets_and_threads() {
    let c = Core::open(Path::new(":memory:")).unwrap();
    let p = principal("owner");
    let s = c
        .create_share(&p, "Secret", "static", "entra-gated", "s")
        .unwrap();
    publish(&c, &p, &s, "secret");
    let cap = s["capability"].as_str().unwrap();
    assert_eq!(c.viewer_share(cap, None).unwrap_err().status, 401);
    assert!(c.revision_files(cap, None, None).is_err());
    assert!(c.threads(cap, None).is_err());
    assert!(c.viewer_share(cap, Some(&p)).is_ok());
}
#[test]
fn guest_and_cross_share_protection() {
    let (c, p, s) = fixture();
    let r = publish(&c, &p, &s, "Hello");
    let cap = s["capability"].as_str().unwrap();
    let rev = r["revision_id"].as_str().unwrap();
    assert!(
        c.add_thread(cap, None, Some(" "), "hello", rev, json!({}))
            .is_err()
    );
    let a = c
        .add_thread(
            cap,
            None,
            Some("banana"),
            "hello",
            rev,
            json!({"kind":"general"}),
        )
        .unwrap();
    let b = c
        .add_thread(
            cap,
            None,
            Some("banana"),
            "hello",
            rev,
            json!({"kind":"general"}),
        )
        .unwrap();
    assert_ne!(a["comments"][0]["actor_id"], b["comments"][0]["actor_id"]);
    assert_eq!(a["comments"][0]["authenticated"], false);
    let s2 = c
        .create_share(&p, "Other", "static", "link-only", "two")
        .unwrap();
    publish(&c, &p, &s2, "Other");
    assert!(
        c.reply(
            s2["capability"].as_str().unwrap(),
            a["id"].as_str().unwrap(),
            None,
            Some("banana"),
            "intrude"
        )
        .is_err()
    );
    assert!(
        c.revision_files(
            cap,
            Some(
                c.get_share(&p, s2["id"].as_str().unwrap()).unwrap()["latest_revision"]
                    .as_str()
                    .unwrap()
            ),
            None
        )
        .is_err()
    );
    assert!(
        c.resolve(
            &principal("other"),
            s["id"].as_str().unwrap(),
            a["id"].as_str().unwrap(),
            true
        )
        .is_err()
    );
}
#[test]
fn immutable_origins_and_conservative_rematch() {
    let (c, p, s) = fixture();
    let first = publish(&c, &p, &s, "<p id='target'>Unique words</p>");
    let anchor =
        json!({"kind":"text","path":"index.html","exact":"Unique words","element_id":"target"});
    let cap = s["capability"].as_str().unwrap();
    let t = c
        .add_thread(
            cap,
            None,
            Some("banana"),
            "Review",
            first["revision_id"].as_str().unwrap(),
            anchor.clone(),
        )
        .unwrap();
    publish(&c, &p, &s, "<h1>New</h1><p id='target'>Unique words</p>");
    publish(&c, &p, &s, "Unique words Unique words");
    publish(&c, &p, &s, "Missing");
    let got = &c.threads(cap, None).unwrap()[0];
    assert_eq!(got["original_anchor"], anchor);
    assert_eq!(got["original_revision"], first["revision_id"]);
    let statuses: Vec<_> = got["placements"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| p["status"].as_str().unwrap())
        .collect();
    assert_eq!(statuses.iter().filter(|s| **s == "likely").count(), 1);
    assert_eq!(statuses.iter().filter(|s| **s == "unmatched").count(), 2);
    let c = c.lock().unwrap();
    assert!(
        c.execute(
            "UPDATE threads SET anchor='{}' WHERE id=?",
            [t["id"].as_str().unwrap()]
        )
        .is_err()
    );
    assert!(
        c.execute(
            "UPDATE files SET bytes=x'01' WHERE revision=?",
            [first["revision_id"].as_str().unwrap()]
        )
        .is_err()
    );
}
#[test]
fn lifecycle_revokes_history_and_queued_batches() {
    let (c, p, s) = fixture();
    let r = publish(&c, &p, &s, "Hello");
    let cap = s["capability"].as_str().unwrap();
    c.add_thread(
        cap,
        None,
        Some("x"),
        "Comment",
        r["revision_id"].as_str().unwrap(),
        json!({}),
    )
    .unwrap();
    let sid = s["id"].as_str().unwrap();
    c.lifecycle(&p, sid, "delete").unwrap();
    assert!(c.viewer_share(cap, None).is_err());
    assert!(
        c.revision_files(cap, Some(r["revision_id"].as_str().unwrap()), None)
            .is_err()
    );
    assert!(c.create_upload(&p, sid).is_err());
    assert!(
        c.claim_due_notifications(i64::MAX)
            .unwrap()
            .as_array()
            .unwrap()
            .is_empty()
    );
    c.lifecycle(&p, sid, "restore").unwrap();
    assert!(c.viewer_share(cap, None).is_err());
    c.lifecycle(&p, sid, "republish").unwrap();
    assert!(c.viewer_share(cap, None).is_ok());
}
#[test]
fn restart_keeps_bytes_and_discussion() {
    let d = tempfile::tempdir().unwrap();
    let path = d.path().join("core.sqlite");
    let p = principal("owner");
    let cap;
    {
        let c = Core::open(&path).unwrap();
        let s = c
            .create_share(&p, "Persist", "static", "link-only", "persist")
            .unwrap();
        let r = publish(&c, &p, &s, "Bytes persist");
        cap = s["capability"].as_str().unwrap().to_string();
        c.add_thread(
            &cap,
            None,
            Some("x"),
            "Remember",
            r["revision_id"].as_str().unwrap(),
            json!({}),
        )
        .unwrap();
    }
    let c = Core::open(&path).unwrap();
    assert_eq!(
        c.revision_files(&cap, None, None).unwrap()["index.html"],
        b"Bytes persist"
    );
    assert_eq!(
        c.threads(&cap, None).unwrap()[0]["comments"][0]["body"],
        "Remember"
    );
    assert_eq!(
        c.claim_due_notifications(i64::MAX)
            .unwrap()
            .as_array()
            .unwrap()
            .len(),
        1
    );
}
#[test]
fn durable_two_minute_recipient_debounce_and_claim_race() {
    let (c, p, s) = fixture();
    let r = publish(&c, &p, &s, "Hello");
    let cap = s["capability"].as_str().unwrap();
    let t = c
        .add_thread(
            cap,
            Some(&p),
            None,
            "First",
            r["revision_id"].as_str().unwrap(),
            json!({}),
        )
        .unwrap();
    let sid = s["id"].as_str().unwrap();
    let tid = t["id"].as_str().unwrap();
    {
        let mut c = c.lock().unwrap();
        let tx = c.transaction().unwrap();
        tx.execute("DELETE FROM batch_events", []).unwrap();
        tx.execute("DELETE FROM batches", []).unwrap();
        let a = ("guest:test".into(), "banana".into(), false);
        post(&tx, sid, tid, &a, None, "at zero", 0).unwrap();
        post(&tx, sid, tid, &a, None, "at ninety", 90).unwrap();
        tx.commit().unwrap();
    }
    assert!(
        c.claim_due_notifications(120)
            .unwrap()
            .as_array()
            .unwrap()
            .is_empty()
    );
    assert!(
        c.claim_due_notifications(209)
            .unwrap()
            .as_array()
            .unwrap()
            .is_empty()
    );
    let batches = c.claim_due_notifications(210).unwrap();
    assert_eq!(batches.as_array().unwrap().len(), 1);
    assert_eq!(batches[0]["event_count"], 2);
    let b = &batches[0];
    assert!(
        c.notification_eligible(b["id"].as_str().unwrap(), b["lease"].as_str().unwrap())
            .unwrap()
    );
    c.reply(cap, tid, None, Some("x"), "New after claim")
        .unwrap();
    assert_eq!(
        c.notification_status(&p, sid)
            .unwrap()
            .as_array()
            .unwrap()
            .len(),
        2
    );
    c.finish_notification(
        b["id"].as_str().unwrap(),
        b["lease"].as_str().unwrap(),
        "delivered",
        None,
    )
    .unwrap();
    assert!(
        c.finish_notification(
            b["id"].as_str().unwrap(),
            b["lease"].as_str().unwrap(),
            "delivered",
            None
        )
        .is_err()
    );
}
#[test]
fn paths_and_quotas_reject_invalid_upload() {
    for name in ["../x", "/absolute", "a\\b", "a/./b", "a//b", "C:x"] {
        assert!(validate_files(&BTreeMap::from([(name.into(), vec![])]), name).is_err());
    }
    assert!(
        validate_files(
            &BTreeMap::from([("A".into(), vec![]), ("a".into(), vec![])]),
            "a"
        )
        .is_err()
    );
    assert!(validate_files(&BTreeMap::from([("a".into(), vec![])]), "missing").is_err());
}

#[test]
fn fake_slack_verified_email_lookup_and_missing_address() {
    struct FakeSlack {
        lookups: Vec<String>,
        sent: Vec<String>,
    }
    impl FakeSlack {
        fn deliver(&mut self, email: &str) -> bool {
            self.lookups.push(email.into());
            if email == "owner@example.test" {
                self.sent.push("U_OWNER".into());
                true
            } else {
                false
            }
        }
    }
    let (c, p, s) = fixture();
    let r = publish(&c, &p, &s, "Hello");
    let cap = s["capability"].as_str().unwrap();
    let mut no_email = principal("participant");
    no_email.verified_email = None;
    c.add_thread(
        cap,
        Some(&no_email),
        None,
        "Comment",
        r["revision_id"].as_str().unwrap(),
        json!({}),
    )
    .unwrap();
    let statuses = c
        .notification_status(&p, s["id"].as_str().unwrap())
        .unwrap();
    assert!(
        statuses
            .as_array()
            .unwrap()
            .iter()
            .any(|b| b["state"] == "no_delivery")
    );
    let batches = c.claim_due_notifications(i64::MAX).unwrap();
    let mut fake = FakeSlack {
        lookups: vec![],
        sent: vec![],
    };
    for b in batches.as_array().unwrap() {
        assert!(
            c.notification_eligible(b["id"].as_str().unwrap(), b["lease"].as_str().unwrap())
                .unwrap()
        );
        let delivered = fake.deliver(b["verified_email"].as_str().unwrap());
        c.finish_notification(
            b["id"].as_str().unwrap(),
            b["lease"].as_str().unwrap(),
            if delivered { "delivered" } else { "failed" },
            None,
        )
        .unwrap();
    }
    assert_eq!(fake.lookups, vec!["owner@example.test"]);
    assert_eq!(fake.sent, vec!["U_OWNER"]);
}
