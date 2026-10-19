//! Firestore domain service. Blob generations live in private GCS; all domain writes
//! use serializable Firestore transactions and bounded conflict retries.
use crate::cloud_store::{BlobRef, CloudStore, Collection as C, Document, Mutation, Precondition};
use crate::core::{
    AppError, Principal, comment_fingerprint, comment_mutation, comment_response_json,
    replay_comment_response, validate_comment_idempotency, validate_files,
};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    future::Future,
    time::{SystemTime, UNIX_EPOCH},
};
use uuid::Uuid;
fn err(status: u16, code: &str, message: &str) -> AppError {
    AppError {
        status,
        code: code.into(),
        message: message.into(),
    }
}
fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}
fn id() -> String {
    Uuid::new_v4().to_string()
}
fn hash(s: &str) -> String {
    format!("{:x}", Sha256::digest(s.as_bytes()))
}
fn auth(p: &Principal) -> Result<(), AppError> {
    if !p.authenticated || p.id.trim().is_empty() {
        Err(err(401, "unauthenticated", "Authentication required"))
    } else {
        Ok(())
    }
}
fn owner(p: &Principal, s: &Value) -> Result<(), AppError> {
    auth(p)?;
    if s["owner_id"] != p.id {
        Err(err(403, "forbidden", "Share belongs to another publisher"))
    } else {
        Ok(())
    }
}
fn writable(s: &Value) -> Result<(), AppError> {
    if s["state"] == "deleted" || s["state"] == "archived" {
        Err(err(
            409,
            "state_conflict",
            "Restore share before making changes",
        ))
    } else {
        Ok(())
    }
}
fn output(mut s: Value, management: bool) -> Value {
    s["ready"] = json!(s["state"] == "published");
    for k in ["idem", "notification_epoch"] {
        s.as_object_mut().unwrap().remove(k);
    }
    if !management {
        for k in ["owner_id", "capability", "version"] {
            s.as_object_mut().unwrap().remove(k);
        }
    }
    s
}
fn valid_body(b: &str) -> Result<(), AppError> {
    if b.trim().is_empty() || b.len() > 20000 {
        Err(err(
            400,
            "invalid_comment",
            "Comment must contain 1–20000 bytes",
        ))
    } else {
        Ok(())
    }
}
fn active_discussion(t: &Value) -> Result<(), AppError> {
    if t["state"] == "archived" || t["state"] == "deleted" {
        return Err(err(
            409,
            "discussion_state",
            "Restore this discussion before replying or resolving it",
        ));
    }
    Ok(())
}
fn actor(p: Option<&Principal>, name: Option<&str>) -> Result<(String, String, bool), AppError> {
    if let Some(p) = p {
        auth(p)?;
        Ok((p.id.clone(), p.display_name.clone(), true))
    } else {
        let n = name.unwrap_or("").trim();
        if n.is_empty() || n.len() > 100 {
            return Err(err(
                400,
                "guest_name_required",
                "Provide an unverified guest name (1–100 bytes)",
            ));
        }
        Ok((format!("guest:{}", id()), n.into(), false))
    }
}
#[derive(Clone)]
pub struct FirestoreCore {
    pub store: CloudStore,
}
struct Tx {
    store: CloudStore,
    token: String,
    transaction: String,
    writes: Vec<Mutation>,
}
impl Tx {
    async fn get(&self, c: C, id: &str) -> Result<Document, AppError> {
        self.store
            .get(c, id, Some(&self.transaction), &self.token)
            .await?
            .ok_or_else(|| err(404, "not_found", "Record not found"))
    }
    async fn maybe(&self, c: C, id: &str) -> Result<Option<Document>, AppError> {
        self.store
            .get(c, id, Some(&self.transaction), &self.token)
            .await
    }
    async fn query(&self, c: C, f: &str, v: &Value) -> Result<Vec<Document>, AppError> {
        self.store
            .query(c, f, v, Some(&self.transaction), &self.token)
            .await
    }
    fn create(&mut self, c: C, id: &str, data: Value) {
        self.writes.push(Mutation {
            collection: c,
            id: id.into(),
            data,
            condition: Precondition::Create,
        });
    }
    fn update(&mut self, c: C, id: &str, old: &Document, data: Value) {
        self.writes.push(Mutation {
            collection: c,
            id: id.into(),
            data,
            condition: Precondition::UpdateTime(old.update_time.clone()),
        });
    }
    async fn replay_comment(
        &self,
        sid: &str,
        operation: &str,
        receipt: Option<(&str, &str)>,
        fingerprint: &str,
    ) -> Result<Option<Value>, AppError> {
        let Some((key, scope)) = receipt else {
            return Ok(None);
        };
        let receipt_id = comment_receipt_id(sid, scope, operation, key);
        let Some(old) = self.maybe(C::Idempotency, &receipt_id).await? else {
            return Ok(None);
        };
        replay_comment_response(
            fingerprint,
            old.data["fingerprint"].as_str().unwrap_or(""),
            old.data["response_json"].as_str().unwrap_or(""),
        )
        .map(Some)
    }
    fn save_comment_receipt(
        &mut self,
        sid: &str,
        operation: &str,
        receipt: Option<(&str, &str)>,
        fingerprint: &str,
        response: &Value,
    ) -> Result<(), AppError> {
        if let Some((key, scope)) = receipt {
            let encoded = comment_response_json(response)?;
            let receipt_id = comment_receipt_id(sid, scope, operation, key);
            self.create(
                C::Idempotency,
                &receipt_id,
                json!({
                    "kind": "comment", "share_id": sid, "operation": operation,
                    "fingerprint": fingerprint, "response_json": encoded, "created_at": now(),
                }),
            );
        }
        Ok(())
    }
    async fn thread_snapshot(&self, mut thread: Value) -> Result<Value, AppError> {
        let mut comments = self
            .query(C::Comments, "thread_id", &thread["id"])
            .await?
            .into_iter()
            .map(|d| d.data)
            .collect::<Vec<_>>();
        comments.sort_by_key(|c| {
            (
                c["sequence"].as_i64().unwrap_or(0),
                c["id"].as_str().unwrap_or("").to_owned(),
            )
        });
        thread["comments"] = json!(comments.into_iter().map(comment_output).collect::<Vec<_>>());
        thread["placements"] = json!(
            self.query(C::Placements, "thread_id", &thread["id"])
                .await?
                .into_iter()
                .map(|mut d| {
                    d.data.as_object_mut().unwrap().remove("thread_id");
                    d.data
                })
                .collect::<Vec<_>>()
        );
        thread.as_object_mut().unwrap().remove("comment_count");
        Ok(thread)
    }
    async fn principal(&mut self, p: &Principal) -> Result<(), AppError> {
        auth(p)?;
        let key = hash(&p.id);
        let v = json!({"id":p.id,"display_name":p.display_name,"verified_email":p.verified_email});
        match self.maybe(C::Principals, &key).await? {
            Some(d) if d.data != v => self.update(C::Principals, &key, &d, v),
            Some(_) => {}
            None => self.create(C::Principals, &key, v),
        };
        Ok(())
    }
    async fn view(&self, cap: &str, p: Option<&Principal>) -> Result<Document, AppError> {
        let key = hash(&format!("cap:{cap}"));
        let link = self.get(C::Idempotency, &key).await?;
        let s = self
            .get(C::Shares, link.data["share_id"].as_str().unwrap_or(""))
            .await?;
        check_view(&s.data, p)?;
        Ok(s)
    }
}
fn check_view(s: &Value, p: Option<&Principal>) -> Result<(), AppError> {
    if s["state"] != "published" {
        return Err(err(404, "not_found", "Share unavailable"));
    }
    if s["policy"] == "entra-gated" {
        auth(p.ok_or_else(|| {
            err(
                401,
                "login_required",
                "Admitted authenticated viewer required",
            )
        })?)?;
    }
    Ok(())
}
impl FirestoreCore {
    pub fn new(store: CloudStore) -> Self {
        Self { store }
    }
    async fn transact<F, Fut>(&self, f: F) -> Result<Value, AppError>
    where
        F: Fn(Tx) -> Fut,
        Fut: Future<Output = Result<(Tx, Value), AppError>>,
    {
        const MAX_ATTEMPTS: usize = 8;
        for attempt in 0..MAX_ATTEMPTS {
            let token = self.store.runtime_token().await?;
            let transaction = self.store.begin_transaction(&token).await?;
            let tx = Tx {
                store: self.store.clone(),
                token: token.clone(),
                transaction: transaction.clone(),
                writes: vec![],
            };
            let result = f(tx).await;
            match result {
                Ok((tx, value)) => {
                    if tx.writes.is_empty() {
                        let _ = self.store.rollback(&transaction, &token).await;
                        return Ok(value);
                    }
                    match self
                        .store
                        .commit(&tx.writes, Some(&transaction), &token)
                        .await
                    {
                        Ok(_) => return Ok(value),
                        Err(e) => {
                            let _ = self.store.rollback(&transaction, &token).await;
                            if e.code != "storage_conflict" || attempt + 1 == MAX_ATTEMPTS {
                                return Err(e);
                            }
                        }
                    }
                }
                Err(e) => {
                    let _ = self.store.rollback(&transaction, &token).await;
                    if e.code != "storage_conflict" || attempt + 1 == MAX_ATTEMPTS {
                        return Err(e);
                    }
                }
            }
            // Pessimistic read-to-write conflicts can abort both contenders together.
            // Deterministic delays make them collide again in lockstep; jitter separates
            // their next read snapshots while retaining a bounded retry budget.
            let backoff_ms = (25_u64 << attempt).min(1000);
            let delay_ms = rand::random_range(backoff_ms..=backoff_ms * 2);
            tokio::time::sleep(std::time::Duration::from_millis(delay_ms)).await;
        }
        Err(err(
            409,
            "storage_conflict",
            "Concurrent changes; retry request",
        ))
    }
    async fn get(&self, c: C, key: &str) -> Result<Document, AppError> {
        let token = self.store.runtime_token().await?;
        self.store
            .get(c, key, None, &token)
            .await?
            .ok_or_else(|| err(404, "not_found", "Record not found"))
    }
    async fn query(&self, c: C, f: &str, v: &Value) -> Result<Vec<Document>, AppError> {
        let token = self.store.runtime_token().await?;
        self.store.query(c, f, v, None, &token).await
    }
    async fn view(&self, cap: &str, p: Option<&Principal>) -> Result<Value, AppError> {
        let link = self
            .get(C::Idempotency, &hash(&format!("cap:{cap}")))
            .await?;
        let s = self
            .get(C::Shares, link.data["share_id"].as_str().unwrap_or(""))
            .await?
            .data;
        check_view(&s, p)?;
        Ok(s)
    }
    pub async fn list_shares(&self, p: &Principal) -> Result<Value, AppError> {
        auth(p)?;
        let mut v = self
            .query(C::Shares, "owner_id", &json!(p.id))
            .await?
            .into_iter()
            .map(|d| output(d.data, true))
            .collect::<Vec<_>>();
        v.sort_by_key(|d| std::cmp::Reverse(d["created_at"].as_i64().unwrap_or(0)));
        Ok(json!(v))
    }
    pub async fn create_share(
        &self,
        p: &Principal,
        title: &str,
        kind: &str,
        policy: &str,
        key: &str,
    ) -> Result<Value, AppError> {
        auth(p)?;
        if title.trim().is_empty() || title.len() > 300 || key.is_empty() || key.len() > 200 {
            return Err(err(
                400,
                "invalid_request",
                "Title and bounded idempotency key required",
            ));
        }
        if !["static", "markdown", "react"].contains(&kind) {
            return Err(err(
                400,
                "unsupported_content",
                "Use static, markdown, or react",
            ));
        }
        if !["link-only", "entra-gated"].contains(&policy) {
            return Err(err(
                400,
                "invalid_policy",
                "Explicit link-only or entra-gated policy required",
            ));
        }
        let ikey = hash(&format!("create:{}", json!([p.id, key])));
        let sid = id();
        let cap = format!("{}{}", Uuid::new_v4().simple(), Uuid::new_v4().simple());
        self.transact(|mut tx|{let ikey=ikey.clone();let sid=sid.clone();let cap=cap.clone();async move{if let Some(old)=tx.maybe(C::Idempotency,&ikey).await?{let s=tx.get(C::Shares,old.data["share_id"].as_str().unwrap()).await?.data;if s["title"]!=title||s["kind"]!=kind||s["policy"]!=policy{return Err(err(409,"idempotency_conflict","Key already used with different share input"));}return Ok((tx,output(s,true)));}tx.principal(p).await?;let s=json!({"id":sid,"owner_id":p.id,"title":title,"kind":kind,"policy":policy,"state":"pending","capability":cap,"latest_revision":null,"created_at":now(),"version":0});tx.create(C::Shares,&sid,s.clone());tx.create(C::Idempotency,&ikey,json!({"share_id":sid}));tx.create(C::Idempotency,&hash(&format!("cap:{cap}")),json!({"share_id":sid}));Ok((tx,output(s,true)))}}).await
    }
    pub async fn get_share(&self, p: &Principal, sid: &str) -> Result<Value, AppError> {
        let mut s = self.get(C::Shares, sid).await?.data;
        owner(p, &s)?;
        s["revisions"] = self.revisions(sid).await?;
        Ok(output(s, true))
    }
    async fn revision_deleted(&self, rid: &str) -> Result<bool, AppError> {
        let token = self.store.runtime_token().await?;
        Ok(self
            .store
            .get(C::RevisionTombstones, rid, None, &token)
            .await?
            .is_some())
    }
    async fn revisions(&self, sid: &str) -> Result<Value, AppError> {
        let deleted = self
            .query(C::RevisionTombstones, "share_id", &json!(sid))
            .await?
            .into_iter()
            .map(|d| d.data["revision_id"].as_str().unwrap_or("").to_owned())
            .collect::<BTreeSet<_>>();
        let mut rows = self
            .query(C::Revisions, "share_id", &json!(sid))
            .await?
            .into_iter()
            .filter(|d| !deleted.contains(d.data["id"].as_str().unwrap_or("")))
            .map(|d| revision_output(&d.data))
            .collect::<Vec<_>>();
        rows.sort_by_key(|v| {
            (
                v["created_at"].as_i64().unwrap_or(0),
                v["id"].as_str().unwrap_or("").to_owned(),
            )
        });
        Ok(json!(rows))
    }
    pub async fn create_upload(&self, p: &Principal, sid: &str) -> Result<Value, AppError> {
        let uid = id();
        self.transact(|mut tx|{let uid=uid.clone();async move{let s=tx.get(C::Shares,sid).await?;owner(p,&s.data)?;writable(&s.data)?;let u=json!({"id":uid,"share_id":sid,"token":format!("{}{}",Uuid::new_v4().simple(),Uuid::new_v4().simple()),"expires_at":now()+3600,"revision_id":null});tx.create(C::Uploads,&uid,u.clone());Ok((tx,u))}}).await
    }
    pub async fn get_upload(&self, p: &Principal, uid: &str) -> Result<Value, AppError> {
        let mut u = self.get(C::Uploads, uid).await?.data;
        let s = self
            .get(C::Shares, u["share_id"].as_str().unwrap())
            .await?
            .data;
        owner(p, &s)?;
        u.as_object_mut().unwrap().remove("staged");
        Ok(u)
    }
    async fn blob_manifest(
        &self,
        uid: &str,
        files: &BTreeMap<String, Vec<u8>>,
    ) -> Result<Value, AppError> {
        let token = self.store.runtime_token().await?;
        let attempt = id();
        let mut entries = vec![];
        for (path, bytes) in files {
            let blob = self
                .store
                .put_blob(
                    &format!("uploads/{uid}/{attempt}/{path}"),
                    bytes.clone(),
                    &token,
                )
                .await?;
            entries.push(json!({"path":path,"size":bytes.len(),"sha256":format!("{:x}",Sha256::digest(bytes)),"name":blob.name,"generation":blob.generation}));
        }
        Ok(json!(entries))
    }
    pub async fn stage_upload(
        &self,
        p: &Principal,
        uid: &str,
        files: &BTreeMap<String, Vec<u8>>,
        entrypoint: &str,
    ) -> Result<Value, AppError> {
        // Staging contains source files; React may generate its eventual entrypoint.
        // Publication validates that the prepared output entrypoint actually exists.
        validate_files(files, files.keys().next().map(String::as_str).unwrap_or(""))?;
        let u = self.get_upload(p, uid).await?;
        writable(
            &self
                .get(C::Shares, u["share_id"].as_str().unwrap())
                .await?
                .data,
        )?;
        if u["expires_at"].as_i64().unwrap_or(0) < now() {
            return Err(err(410, "expired_upload", "Upload session expired"));
        }
        if !u["revision_id"].is_null() {
            return Err(err(409, "state_conflict", "Upload already published"));
        }
        if u["build_job_id"].is_string() {
            return Err(err(
                409,
                "upload_frozen",
                "Upload is frozen for its queued build",
            ));
        }
        if u["finalization"].is_object() {
            return Err(err(
                409,
                "upload_frozen",
                "Upload is frozen for its finalization request",
            ));
        }
        let manifest = self.blob_manifest(uid, files).await?;
        self.transact(|mut tx| {
            let manifest = manifest.clone();
            async move {
                let u = tx.get(C::Uploads, uid).await?;
                let s = tx
                    .get(C::Shares, u.data["share_id"].as_str().unwrap())
                    .await?;
                owner(p, &s.data)?;
                writable(&s.data)?;
                if u.data["expires_at"].as_i64().unwrap_or(0) < now() {
                    return Err(err(410, "expired_upload", "Upload session expired"));
                }
                if !u.data["revision_id"].is_null() {
                    return Err(err(409, "state_conflict", "Upload already published"));
                }
                if u.data["build_job_id"].is_string() {
                    return Err(err(
                        409,
                        "upload_frozen",
                        "Upload is frozen for its queued build",
                    ));
                }
                if u.data["finalization"].is_object() {
                    return Err(err(
                        409,
                        "upload_frozen",
                        "Upload is frozen for its finalization request",
                    ));
                }
                let mut v = u.data.clone();
                v["staged"] = manifest;
                v["entrypoint"] = json!(entrypoint);
                tx.update(C::Uploads, uid, &u, v);
                Ok((tx, json!({"staged":true,"upload_id":uid})))
            }
        })
        .await
    }
    pub async fn staged_files(
        &self,
        p: &Principal,
        uid: &str,
    ) -> Result<(BTreeMap<String, Vec<u8>>, String), AppError> {
        self.get_upload(p, uid).await?;
        let u = self.get(C::Uploads, uid).await?.data;
        Ok((
            self.read_manifest(&u["staged"]).await?,
            u["entrypoint"].as_str().unwrap_or("index.html").into(),
        ))
    }
    /// Reserve the exact upload source and request before any preparation can run.
    /// The global key reference and upload freeze commit together, including on retries.
    pub async fn begin_finalize(
        &self,
        p: &Principal,
        uid: &str,
        key: &str,
        entrypoint: &str,
    ) -> Result<Value, AppError> {
        crate::core::validate_finalize_key(key)?;
        let ikey = finalize_key(p, key);
        let payload = json!({"upload_id":uid,"entrypoint":entrypoint});
        self.transact(|mut tx| {
            let ikey=ikey.clone(); let payload=payload.clone();
            async move {
                let u=tx.get(C::Uploads,uid).await?;
                let sid=u.data["share_id"].as_str().unwrap();
                let s=tx.get(C::Shares,sid).await?;
                owner(p,&s.data)?; writable(&s.data)?;
                let old=tx.maybe(C::Idempotency,&ikey).await?;
                if let Some(old)=old {
                    if old.data["payload"]!=payload || old.data["owner_id"]!=p.id { return Err(err(409,"idempotency_conflict","Finalization key already binds different input")); }
                    let frozen=&u.data["finalization"];
                    if frozen["key"]!=ikey || frozen["payload"]!=payload { return Err(err(409,"idempotency_conflict","Upload finalization does not match its request")); }
                    if frozen["result"].is_null() && frozen["expected_share_version"]!=s.data["version"] { return Err(err(409,"stale_build","Share changed after finalization began")); }
                    return Ok((tx,frozen.clone()));
                }
                if u.data["finalization"].is_object() { return Err(err(409,"idempotency_conflict","Upload already binds another finalization key")); }
                if u.data["revision_id"].is_string() { return Err(err(409,"already_finalized","Upload was already finalized")); }
                if u.data["build_job_id"].is_string() { return Err(err(409,"upload_frozen","Upload already belongs to a build")); }
                if u.data["expires_at"].as_i64().unwrap_or(0)<now() { return Err(err(410,"expired_upload","Upload session expired")); }
                if !u.data["staged"].as_array().is_some_and(|v|!v.is_empty()) { return Err(err(409,"incomplete_upload","Upload files before finalization")); }
                let digest=format!("{:x}",Sha256::digest(serde_json::to_vec(&u.data["staged"]).unwrap()));
                let frozen=json!({"key":ikey,"payload":payload,"entrypoint":entrypoint,"source_manifest_sha256":digest,"expected_share_version":s.data["version"],"expected_latest_revision":s.data["latest_revision"],"result":null});
                let mut next=u.data.clone(); next["finalization"]=frozen.clone();
                tx.create(C::Idempotency,&ikey,json!({"operation":"finalize","owner_id":p.id,"payload":payload,"upload_id":uid}));
                tx.update(C::Uploads,uid,&u,next);
                Ok((tx,frozen))
            }
        }).await
    }
    async fn read_manifest(&self, manifest: &Value) -> Result<BTreeMap<String, Vec<u8>>, AppError> {
        let rows = manifest.as_array().ok_or_else(|| {
            err(
                409,
                "upload_not_staged",
                "Upload bytes have not been staged",
            )
        })?;
        let token = self.store.runtime_token().await?;
        let mut files = BTreeMap::new();
        for row in rows {
            let bytes = self
                .store
                .get_blob(
                    &BlobRef {
                        name: row["name"].as_str().unwrap_or("").into(),
                        generation: row["generation"].as_str().unwrap_or("").into(),
                    },
                    &token,
                )
                .await?;
            if json!(format!("{:x}", Sha256::digest(&bytes))) != row["sha256"] {
                return Err(err(
                    502,
                    "artifact_integrity",
                    "Stored artifact digest mismatch",
                ));
            }
            files.insert(row["path"].as_str().unwrap_or("").into(), bytes);
        }
        Ok(files)
    }
    pub async fn publish(
        &self,
        p: &Principal,
        uid: &str,
        files: &BTreeMap<String, Vec<u8>>,
        entrypoint: &str,
    ) -> Result<Value, AppError> {
        self.publish_inner(p, uid, files, entrypoint, None, None, None, None)
            .await
    }
    #[allow(clippy::too_many_arguments)]
    pub async fn publish_finalized(
        &self,
        p: &Principal,
        uid: &str,
        key: &str,
        files: &BTreeMap<String, Vec<u8>>,
        entrypoint: &str,
        warnings: &Value,
    ) -> Result<Value, AppError> {
        crate::core::validate_finalize_key(key)?;
        let ikey = finalize_key(p, key);
        self.publish_inner(
            p,
            uid,
            files,
            entrypoint,
            None,
            None,
            None,
            Some((&ikey, warnings)),
        )
        .await
    }
    pub async fn publish_expected(
        &self,
        p: &Principal,
        uid: &str,
        files: &BTreeMap<String, Vec<u8>>,
        entrypoint: &str,
        expected_latest: Option<&str>,
    ) -> Result<Value, AppError> {
        self.publish_inner(
            p,
            uid,
            files,
            entrypoint,
            Some(expected_latest),
            None,
            None,
            None,
        )
        .await
    }
    /// Atomically fence the worker lease and share lifecycle version, publish, and complete its job.
    pub async fn publish_for_job(
        &self,
        p: &Principal,
        uid: &str,
        files: &BTreeMap<String, Vec<u8>>,
        entrypoint: &str,
        job_id: &str,
        claim_id: &str,
    ) -> Result<Value, AppError> {
        self.publish_inner(
            p,
            uid,
            files,
            entrypoint,
            None,
            Some((job_id, claim_id)),
            None,
            None,
        )
        .await
    }
    /// Reject publication when any publish or lifecycle transition occurred during preparation.
    pub async fn publish_at_version(
        &self,
        p: &Principal,
        uid: &str,
        files: &BTreeMap<String, Vec<u8>>,
        entrypoint: &str,
        expected_version: u64,
    ) -> Result<Value, AppError> {
        self.publish_inner(
            p,
            uid,
            files,
            entrypoint,
            None,
            None,
            Some(expected_version),
            None,
        )
        .await
    }
    // Keep independent publication fences explicit at the transaction boundary.
    #[allow(clippy::too_many_arguments)]
    async fn publish_inner(
        &self,
        p: &Principal,
        uid: &str,
        files: &BTreeMap<String, Vec<u8>>,
        entrypoint: &str,
        expected_latest: Option<Option<&str>>,
        job: Option<(&str, &str)>,
        expected_version: Option<u64>,
        finalized: Option<(&str, &Value)>,
    ) -> Result<Value, AppError> {
        auth(p)?;
        let u = self.get_upload(p, uid).await?;
        writable(
            &self
                .get(C::Shares, u["share_id"].as_str().unwrap())
                .await?
                .data,
        )?;
        match (u["finalization"].is_object(), finalized, job) {
            (true, Some((key, _)), None) if u["finalization"]["key"] == key => {
                if u["finalization"]["result"].is_object() {
                    return Ok(u["finalization"]["result"].clone());
                }
            }
            (true, None, Some(_)) => {}
            (true, _, _) => {
                return Err(err(
                    409,
                    "upload_frozen",
                    "Upload belongs to a different finalization request",
                ));
            }
            (false, Some(_), _) => {
                return Err(err(
                    409,
                    "finalization_required",
                    "Reserve finalization before publication",
                ));
            }
            _ => {}
        }
        if job.is_none() && u["build_job_id"].is_string() {
            return Err(err(
                409,
                "upload_frozen",
                "Upload is managed by its queued build",
            ));
        }
        if u["revision_id"].is_string() {
            if let Some((jid, _)) = job {
                let j = self.get(C::BuildJobs, jid).await?;
                if j.data["upload_id"] != uid || j.data["state"] != "succeeded" {
                    return Err(err(
                        409,
                        "build_state_conflict",
                        "Build no longer owns this upload",
                    ));
                }
                return Ok(j.data["result"].clone());
            }
            let mut s = self.get_share(p, u["share_id"].as_str().unwrap()).await?;
            s["revision_id"] = u["revision_id"].clone();
            return Ok(s);
        }
        if u["expires_at"].as_i64().unwrap_or(0) < now() {
            return Err(err(410, "expired_upload", "Upload session expired"));
        }
        validate_files(files, entrypoint)?;
        let manifest = self.blob_manifest(uid, files).await?;
        let rid = id();
        let mut h = Sha256::new();
        for (path, bytes) in files {
            h.update((path.len() as u64).to_be_bytes());
            h.update(path.as_bytes());
            h.update((bytes.len() as u64).to_be_bytes());
            h.update(bytes);
        }
        let digest = format!("{:x}", h.finalize());
        self.transact(|mut tx|{
            let rid=rid.clone();let manifest=manifest.clone();let digest=digest.clone();
            async move{
                let u=tx.get(C::Uploads,uid).await?;
                let sid=u.data["share_id"].as_str().unwrap();
                let s=tx.get(C::Shares,sid).await?;
                owner(p,&s.data)?;writable(&s.data)?;
                if let Some((key,_))=finalized {
                    let frozen=&u.data["finalization"];
                    if frozen["key"]!=key { return Err(err(409,"idempotency_conflict","Upload finalization key does not match")); }
                    if frozen["result"].is_object() { return Ok((tx,frozen["result"].clone())); }
                    if frozen["expected_share_version"]!=s.data["version"] { return Err(err(409,"stale_build","Share changed after finalization began")); }
                    let digest=format!("{:x}",Sha256::digest(serde_json::to_vec(&u.data["staged"]).unwrap()));
                    if frozen["source_manifest_sha256"]!=digest { return Err(err(409,"source_changed","Frozen upload source changed")); }
                } else if job.is_none() && u.data["finalization"].is_object() { return Err(err(409,"upload_frozen","Upload belongs to its finalization request")); }
                let job_doc=if let Some((jid,claim))=job{
                    let j=tx.get(C::BuildJobs,jid).await?;
                    if j.data["upload_id"]!=uid || j.data["share_id"]!=sid || u.data["build_job_id"]!=jid{return Err(err(409,"build_state_conflict","Build does not own this upload"));}
                    if j.data["state"]=="succeeded" && u.data["revision_id"].is_string(){return Ok((tx,j.data["result"].clone()));}
                    if j.data["state"]!="running" || j.data["claim_id"]!=claim || j.data["lease_expires_at"].as_i64().unwrap_or(0)<=now(){return Err(err(409,"build_lease_lost","Build lease expired or was replaced"));}
                    if j.data["expected_share_version"].as_u64().unwrap_or(0)!=s.data["version"].as_u64().unwrap_or(0) || j.data["expected_latest_revision"]!=s.data["latest_revision"]{return Err(err(409,"stale_build","Share changed after this build was queued"));}
                    Some(j)
                }else{
                    if u.data["build_job_id"].is_string(){return Err(err(409,"upload_frozen","Upload is managed by its queued build"));}
                    None
                };
                if u.data["revision_id"].is_string(){let mut out=output(s.data,true);out["revision_id"]=u.data["revision_id"].clone();return Ok((tx,out));}
                if let Some(version)=expected_version&& s.data["version"].as_u64().unwrap_or(0)!=version{return Err(err(409,"stale_build","Share changed during content preparation"));}
                if let Some(expected)=expected_latest&& s.data["latest_revision"].as_str()!=expected{return Err(err(409,"stale_build","A newer revision was published while this build was running"));}
                if u.data["expires_at"].as_i64().unwrap_or(0)<now(){return Err(err(410,"expired_upload","Upload session expired"));}
                let origins=tx.query(C::Threads,"share_id",&json!(sid)).await?;
                let unresolved=origins.into_iter().filter(|t|t.data["resolved"]==false && t.data["state"]!="archived" && t.data["state"]!="deleted").collect::<Vec<_>>();
                if unresolved.len()>400{return Err(err(413,"placement_fanout_limit","Revision exceeds atomic placement limit of 400 unresolved threads"));}
                for t in unresolved{
                    let tid=t.data["id"].as_str().unwrap();
                    let(status,reason)=rematch(&t.data["original_anchor"],files);
                    tx.create(C::Placements,&hash(&format!("{tid}:{rid}")),json!({"thread_id":tid,"revision_id":rid,"status":status,"anchor":t.data["original_anchor"],"reason":reason,"algorithm":"conservative-v1"}));
                }
                let r=json!({"id":rid,"share_id":sid,"entrypoint":entrypoint,"digest":digest,"manifest":manifest,"created_at":now()});
                tx.create(C::Revisions,&rid,r);
                let mut next=s.data.clone();next["latest_revision"]=json!(rid);next["state"]=json!("published");next["version"]=json!(s.data["version"].as_u64().unwrap_or(0)+1);
                tx.update(C::Shares,sid,&s,next.clone());
                let mut out=output(next,true);out["revision_id"]=json!(rid);
                let mut uv=u.data.clone();uv["revision_id"]=json!(rid);
                if let Some((_,warnings))=finalized {
                    out=json!({"share_id":sid,"revision_id":rid,"viewer_url":format!("/s/{}",s.data["capability"].as_str().unwrap()),"state":"ready","warnings":warnings});
                    uv["finalization"]["result"]=out.clone();
                }
                tx.update(C::Uploads,uid,&u,uv);
                if let(Some((jid,_)),Some(j))=(job,job_doc){let mut done=j.data.clone();done["state"]=json!("succeeded");done["revision_id"]=json!(rid);done["result"]=out.clone();done["finished_at"]=json!(now());tx.update(C::BuildJobs,jid,&j,done);}
                Ok((tx,out))
            }
        }).await
    }
    pub async fn viewer_share(&self, cap: &str, p: Option<&Principal>) -> Result<Value, AppError> {
        let mut s = self.view(cap, p).await?;
        s["revisions"] = self.revisions(s["id"].as_str().unwrap()).await?;
        Ok(output(s, false))
    }
    async fn revision(
        &self,
        cap: &str,
        rid: Option<&str>,
        p: Option<&Principal>,
    ) -> Result<Value, AppError> {
        let s = self.view(cap, p).await?;
        let r = self
            .get(
                C::Revisions,
                rid.unwrap_or(s["latest_revision"].as_str().unwrap_or("")),
            )
            .await?
            .data;
        if r["share_id"] != s["id"]
            || self
                .revision_deleted(r["id"].as_str().unwrap_or(""))
                .await?
        {
            return Err(err(404, "not_found", "Revision not found"));
        }
        Ok(r)
    }
    pub async fn viewer_revision(
        &self,
        cap: &str,
        r: Option<&str>,
        p: Option<&Principal>,
    ) -> Result<Value, AppError> {
        Ok(revision_output(&self.revision(cap, r, p).await?))
    }
    pub async fn revision_files(
        &self,
        cap: &str,
        r: Option<&str>,
        p: Option<&Principal>,
    ) -> Result<BTreeMap<String, Vec<u8>>, AppError> {
        let rev = self.revision(cap, r, p).await?;
        self.read_manifest(&rev["manifest"]).await
    }
    pub async fn threads(&self, cap: &str, p: Option<&Principal>) -> Result<Value, AppError> {
        let s = self.view(cap, p).await?;
        let mut threads = self
            .query(C::Threads, "share_id", &s["id"])
            .await?
            .into_iter()
            .filter(|d| d.data["state"] != "deleted")
            .map(|d| d.data)
            .collect::<Vec<_>>();
        threads.sort_by_key(|t| {
            (
                t["created_at"].as_i64().unwrap_or(0),
                t["id"].as_str().unwrap_or("").to_string(),
            )
        });
        for t in &mut threads {
            self.hydrate(t).await?;
        }
        Ok(json!(threads))
    }
    async fn hydrate(&self, t: &mut Value) -> Result<(), AppError> {
        let mut comments = self
            .query(C::Comments, "thread_id", &t["id"])
            .await?
            .into_iter()
            .map(|mut d| {
                d.data.as_object_mut().unwrap().remove("thread_id");
                d.data
            })
            .collect::<Vec<_>>();
        comments.sort_by_key(|c| {
            (
                c["sequence"].as_i64().unwrap_or(0),
                c["id"].as_str().unwrap_or("").to_string(),
            )
        });
        for c in &mut comments {
            c.as_object_mut().unwrap().remove("sequence");
        }
        if t["state"].is_null() {
            t["state"] = json!("active");
        }
        t["origin_deleted"] = json!(
            self.revision_deleted(t["original_revision"].as_str().unwrap_or(""))
                .await?
        );
        t["comments"] = json!(comments);
        t["placements"] = json!(
            self.query(C::Placements, "thread_id", &t["id"])
                .await?
                .into_iter()
                .map(|mut d| {
                    d.data.as_object_mut().unwrap().remove("thread_id");
                    d.data
                })
                .collect::<Vec<_>>()
        );
        t.as_object_mut().unwrap().remove("comment_count");
        Ok(())
    }
    pub async fn add_thread(
        &self,
        cap: &str,
        p: Option<&Principal>,
        guest_name: Option<&str>,
        body: &str,
        revision: &str,
        anchor: Value,
    ) -> Result<Value, AppError> {
        self.add_thread_inner(cap, p, guest_name, body, revision, anchor, None)
            .await
    }
    #[allow(clippy::too_many_arguments)]
    pub async fn add_thread_idempotent(
        &self,
        cap: &str,
        p: Option<&Principal>,
        guest_name: Option<&str>,
        body: &str,
        revision: &str,
        anchor: Value,
        key: &str,
        actor_scope: &str,
    ) -> Result<Value, AppError> {
        validate_comment_idempotency(p, key, actor_scope)?;
        self.add_thread_inner(
            cap,
            p,
            guest_name,
            body,
            revision,
            anchor,
            Some((key, actor_scope)),
        )
        .await
    }
    #[allow(clippy::too_many_arguments)]
    async fn add_thread_inner(
        &self,
        cap: &str,
        p: Option<&Principal>,
        guest_name: Option<&str>,
        body: &str,
        revision: &str,
        anchor: Value,
        receipt: Option<(&str, &str)>,
    ) -> Result<Value, AppError> {
        let fingerprint = comment_fingerprint(&json!({
            "guest_name": guest_name, "body": body, "revision": revision, "anchor": anchor,
        }));
        let tid = id();
        self.transact(|mut tx| {
            let tid = tid.clone();
            let anchor = anchor.clone();
            let fingerprint = fingerprint.clone();
            async move {
                // The share state and admission check precede the receipt lookup on every retry.
                let s = tx.view(cap, p).await?;
                let sid = s.data["id"].as_str().unwrap();
                if let Some(result) = tx.replay_comment(sid, "thread", receipt, &fingerprint).await? {
                    let existing=tx.get(C::Threads,result["id"].as_str().unwrap_or("")).await?;
                    active_discussion(&existing.data)?;
                    return Ok((tx, result));
                }
                valid_body(body)?;
                if !anchor.is_object() || anchor.to_string().len() > 16384 {
                    return Err(err(400, "invalid_anchor", "Anchor must be a bounded JSON object"));
                }
                let a = actor(p, guest_name)?;
                let r = tx.get(C::Revisions, revision).await?;
                if r.data["share_id"] != s.data["id"] || tx.maybe(C::RevisionTombstones,revision).await?.is_some() {
                    return Err(err(400, "invalid_revision", "Revision must belong to this share"));
                }
                let t = json!({"id":tid,"share_id":sid,"original_revision":revision,
                    "original_anchor":anchor,"resolved":false,"state":"active","created_at":now(),"comment_count":1});
                let comment = post(&mut tx, &s.data, &tid, &a, p, body, 1).await?;
                tx.create(C::Threads, &tid, t.clone());
                let mut result = t;
                comment_mutation(&mut result, receipt, "thread", comment["id"].as_str().unwrap());
                result.as_object_mut().unwrap().remove("comment_count");
                result["comments"] = json!([comment_output(comment)]);
                result["placements"] = json!([]);
                tx.save_comment_receipt(sid, "thread", receipt, &fingerprint, &result)?;
                Ok((tx, result))
            }
        }).await
    }
    pub async fn reply(
        &self,
        cap: &str,
        tid: &str,
        p: Option<&Principal>,
        guest_name: Option<&str>,
        body: &str,
    ) -> Result<Value, AppError> {
        self.reply_inner(cap, tid, p, guest_name, body, None).await
    }
    #[allow(clippy::too_many_arguments)]
    pub async fn reply_idempotent(
        &self,
        cap: &str,
        tid: &str,
        p: Option<&Principal>,
        guest_name: Option<&str>,
        body: &str,
        key: &str,
        actor_scope: &str,
    ) -> Result<Value, AppError> {
        validate_comment_idempotency(p, key, actor_scope)?;
        self.reply_inner(cap, tid, p, guest_name, body, Some((key, actor_scope)))
            .await
    }
    async fn reply_inner(
        &self,
        cap: &str,
        tid: &str,
        p: Option<&Principal>,
        guest_name: Option<&str>,
        body: &str,
        receipt: Option<(&str, &str)>,
    ) -> Result<Value, AppError> {
        let fingerprint = comment_fingerprint(&json!({
            "guest_name": guest_name, "body": body, "thread": tid,
        }));
        self.transact(|mut tx| {
            let fingerprint = fingerprint.clone();
            async move {
                let s = tx.view(cap, p).await?;
                let sid = s.data["id"].as_str().unwrap();
                if let Some(result) = tx
                    .replay_comment(sid, "reply", receipt, &fingerprint)
                    .await?
                {
                    let existing = tx
                        .get(C::Threads, result["id"].as_str().unwrap_or(""))
                        .await?;
                    active_discussion(&existing.data)?;
                    return Ok((tx, result));
                }
                valid_body(body)?;
                let a = actor(p, guest_name)?;
                let t = tx.get(C::Threads, tid).await?;
                if t.data["share_id"] != s.data["id"] {
                    return Err(err(404, "not_found", "Thread not found"));
                }
                active_discussion(&t.data)?;
                // Firestore reads do not see buffered writes: hydrate the committed snapshot,
                // then append this transaction's exact comment before storing the receipt.
                let mut result = tx.thread_snapshot(t.data.clone()).await?;
                let seq = t.data["comment_count"].as_u64().unwrap_or(0) + 1;
                let comment = post(&mut tx, &s.data, tid, &a, p, body, seq).await?;
                comment_mutation(
                    &mut result,
                    receipt,
                    "reply",
                    comment["id"].as_str().unwrap(),
                );
                result["comments"]
                    .as_array_mut()
                    .unwrap()
                    .push(comment_output(comment));
                let mut next = t.data.clone();
                next["comment_count"] = json!(seq);
                tx.update(C::Threads, tid, &t, next);
                tx.save_comment_receipt(sid, "reply", receipt, &fingerprint, &result)?;
                Ok((tx, result))
            }
        })
        .await
    }
    pub async fn resolve(
        &self,
        p: &Principal,
        sid: &str,
        tid: &str,
        resolved: bool,
    ) -> Result<Value, AppError> {
        let mut out = self
            .transact(|mut tx| async move {
                let s = tx.get(C::Shares, sid).await?;
                owner(p, &s.data)?;
                writable(&s.data)?;
                let t = tx.get(C::Threads, tid).await?;
                if t.data["share_id"] != sid {
                    return Err(err(404, "not_found", "Thread not found"));
                }
                active_discussion(&t.data)?;
                let mut next = t.data.clone();
                next["resolved"] = json!(resolved);
                tx.update(C::Threads, tid, &t, next.clone());
                Ok((tx, next))
            })
            .await?;
        self.hydrate(&mut out).await?;
        Ok(out)
    }
    pub async fn delete_revision(
        &self,
        p: &Principal,
        sid: &str,
        rid: &str,
    ) -> Result<Value, AppError> {
        self.transact(|mut tx| async move {
            let s = tx.get(C::Shares, sid).await?;
            owner(p, &s.data)?;
            writable(&s.data)?;
            let r = tx.get(C::Revisions, rid).await?;
            if r.data["share_id"] != sid {
                return Err(err(404, "not_found", "Revision not found"));
            }
            if s.data["latest_revision"] == rid {
                return Err(err(
                    409,
                    "latest_revision",
                    "The latest revision cannot be deleted. Publish a new revision first.",
                ));
            }
            if tx.maybe(C::RevisionTombstones, rid).await?.is_none() {
                tx.create(
                    C::RevisionTombstones,
                    rid,
                    json!({"share_id":sid,"revision_id":rid,"deleted_at":now()}),
                );
            }
            Ok((tx, json!({"id":rid,"state":"deleted"})))
        })
        .await
    }
    pub async fn moderate_thread(
        &self,
        p: &Principal,
        sid: &str,
        tid: &str,
        action: &str,
    ) -> Result<Value, AppError> {
        self.transact(|mut tx| async move {
            let s = tx.get(C::Shares, sid).await?;
            owner(p, &s.data)?;
            writable(&s.data)?;
            let t = tx.get(C::Threads, tid).await?;
            if t.data["share_id"] != sid {
                return Err(err(404, "not_found", "Thread not found"));
            }
            let state = t.data["state"].as_str().unwrap_or("active");
            let next = crate::core::discussion_transition(state, action)?;
            let mut value = t.data.clone();
            value["state"] = json!(next);
            tx.update(C::Threads, tid, &t, value);
            Ok((tx, json!({"id":tid,"state":next})))
        })
        .await
    }
    pub async fn lifecycle(
        &self,
        p: &Principal,
        sid: &str,
        action: &str,
    ) -> Result<Value, AppError> {
        self.transact(|mut tx| async move {
            let s = tx.get(C::Shares, sid).await?;
            owner(p, &s.data)?;
            let state = s.data["state"].as_str().unwrap_or("");
            let next = match action {
                "unpublish" if state != "deleted" => "unpublished",
                "archive" if state != "deleted" => "archived",
                "delete" => "deleted",
                "restore" if state == "deleted" || state == "archived" => "unpublished",
                "republish" if state == "unpublished" && !s.data["latest_revision"].is_null() => {
                    "published"
                }
                _ => return Err(err(409, "state_conflict", "Invalid lifecycle transition")),
            };
            let mut v = s.data.clone();
            v["state"] = json!(next);
            v["version"] = json!(v["version"].as_u64().unwrap_or(0) + 1);
            if next != "published" {
                v["notification_epoch"] = json!(v["notification_epoch"].as_u64().unwrap_or(0) + 1);
            }
            tx.update(C::Shares, sid, &s, v.clone());
            Ok((tx, output(v, true)))
        })
        .await
    }
    pub async fn claim_due_notifications(&self, at: i64) -> Result<Value, AppError> {
        self.reconcile_notification_claims(at).await?;
        let candidates = self
            .query(C::NotificationBatches, "state", &json!("pending"))
            .await?;
        let mut claimed = vec![];
        for candidate in candidates {
            let bid = candidate.data["id"].as_str().unwrap_or("");
            let result=self.transact(|mut tx|async move{let b=tx.get(C::NotificationBatches,bid).await?;if b.data["state"]!="pending"||b.data["due_at"].as_i64().unwrap_or(i64::MAX)>at{return Ok((tx,Value::Null));}let s=tx.get(C::Shares,b.data["share_id"].as_str().unwrap()).await?;let mut next=b.data.clone();if s.data["state"]!="published"||s.data["notification_epoch"].as_u64().unwrap_or(0)!=b.data["notification_epoch"].as_u64().unwrap_or(0){next["state"]=json!("cancelled");next["error"]=json!("share unavailable");tx.update(C::NotificationBatches,bid,&b,next);return Ok((tx,Value::Null));}let lease=id();next["state"]=json!("sending");next["lease"]=json!(lease);next["lease_expires_at"]=json!(at.saturating_add(180));tx.update(C::NotificationBatches,bid,&b,next.clone());Ok((tx,json!({"id":bid,"share_id":next["share_id"],"recipient":next["recipient"],"verified_email":next["email"],"lease":lease,"lease_expires_at":next["lease_expires_at"],"event_count":next["event_count"]})))}).await?;
            if !result.is_null() {
                claimed.push(result);
            }
        }
        Ok(json!(claimed))
    }
    /// A crashed sender may have delivered. Preserve the batch/events and never requeue it.
    /// Missing deadlines on legacy sending batches are conservatively treated as expired.
    pub async fn reconcile_notification_claims(&self, at: i64) -> Result<Value, AppError> {
        let candidates = self
            .query(C::NotificationBatches, "state", &json!("sending"))
            .await?;
        let mut reconciled = vec![];
        for candidate in candidates {
            let bid = candidate.data["id"].as_str().unwrap_or("");
            let result=self.transact(|mut tx|async move{
                let b=tx.get(C::NotificationBatches,bid).await?;
                if b.data["state"]!="sending"||b.data["lease_expires_at"].as_i64().unwrap_or(0)>at{return Ok((tx,Value::Null));}
                let mut next=b.data.clone();next["state"]=json!("ambiguous");next["error"]=json!("Notification worker lease expired; delivery outcome unknown. No automatic resend.");next["finished_at"]=json!(at);
                tx.update(C::NotificationBatches,bid,&b,next.clone());
                Ok((tx,json!({"id":bid,"share_id":next["share_id"],"state":"ambiguous","event_count":next["event_count"],"error":next["error"]})))
            }).await?;
            if !result.is_null() {
                reconciled.push(result);
            }
        }
        Ok(json!(reconciled))
    }
    pub async fn notification_eligible(&self, batch: &str, lease: &str) -> Result<bool, AppError> {
        let b = self.get(C::NotificationBatches, batch).await?.data;
        let s = self
            .get(C::Shares, b["share_id"].as_str().unwrap())
            .await?
            .data;
        Ok(b["lease"] == lease
            && b["state"] == "sending"
            && b["lease_expires_at"].as_i64().unwrap_or(0) > now()
            && s["state"] == "published"
            && b["notification_epoch"].as_u64().unwrap_or(0)
                == s["notification_epoch"].as_u64().unwrap_or(0))
    }
    pub async fn finish_notification(
        &self,
        batch: &str,
        lease: &str,
        outcome: &str,
        detail: Option<&str>,
    ) -> Result<(), AppError> {
        if !["delivered", "failed", "ambiguous"].contains(&outcome) {
            return Err(err(
                400,
                "invalid_outcome",
                "Use delivered, failed, or ambiguous",
            ));
        }
        self.transact(|mut tx| async move {
            let b = tx.get(C::NotificationBatches, batch).await?;
            let s = tx
                .get(C::Shares, b.data["share_id"].as_str().unwrap())
                .await?;
            if b.data["lease"] != lease
                || b.data["state"] != "sending"
                || s.data["notification_epoch"].as_u64().unwrap_or(0)
                    != b.data["notification_epoch"].as_u64().unwrap_or(0)
            {
                return Err(err(409, "state_conflict", "Batch no longer claimed"));
            }
            let mut next = b.data.clone();
            next["state"] = json!(outcome);
            next["error"] = json!(detail);
            next["finished_at"] = json!(now());
            tx.update(C::NotificationBatches, batch, &b, next);
            Ok((tx, Value::Null))
        })
        .await?;
        Ok(())
    }
    pub async fn notification_status(&self, p: &Principal, sid: &str) -> Result<Value, AppError> {
        let s = self.get(C::Shares, sid).await?.data;
        owner(p, &s)?;
        let rows = self
            .query(C::NotificationBatches, "share_id", &json!(sid))
            .await?;
        Ok(json!(rows.into_iter().map(|d|{let b=d.data;let cancelled=["pending","sending"].contains(&b["state"].as_str().unwrap_or(""))&&b["notification_epoch"].as_u64().unwrap_or(0)!=s["notification_epoch"].as_u64().unwrap_or(0);json!({"id":b["id"],"recipient":b["recipient"],"due_at":b["due_at"],"lease_expires_at":b["lease_expires_at"],"event_count":b["event_count"],"finished_at":b["finished_at"],"state":if cancelled{json!("cancelled")}else{b["state"].clone()},"error":if cancelled{json!("share unavailable")}else{b["error"].clone()}})}).collect::<Vec<_>>()))
    }
}
fn revision_output(r: &Value) -> Value {
    json!({"id":r["id"],"entrypoint":r["entrypoint"],"digest":r["digest"],"created_at":r["created_at"]})
}
fn comment_receipt_id(sid: &str, scope: &str, operation: &str, key: &str) -> String {
    // JSON tuple encoding prevents delimiter collisions between scope/key components.
    hash(&json!(["comment-receipt-v1", sid, scope, operation, key]).to_string())
}
fn comment_output(mut comment: Value) -> Value {
    for field in ["thread_id", "sequence"] {
        comment.as_object_mut().unwrap().remove(field);
    }
    comment
}
async fn post(
    tx: &mut Tx,
    s: &Value,
    tid: &str,
    a: &(String, String, bool),
    p: Option<&Principal>,
    body: &str,
    sequence: u64,
) -> Result<Value, AppError> {
    let sid = s["id"].as_str().unwrap();
    let mut recipients = BTreeSet::new();
    recipients.insert(s["owner_id"].as_str().unwrap().to_owned());
    for d in tx.query(C::Participants, "thread_id", &json!(tid)).await? {
        if let Some(principal) = d.data["principal_id"].as_str() {
            recipients.insert(principal.to_owned());
        }
    }
    if let Some(p) = p {
        recipients.insert(p.id.clone());
        tx.principal(p).await?;
        let key = hash(&format!("participant:{tid}:{}", p.id));
        if tx.maybe(C::Participants, &key).await?.is_none() {
            tx.create(
                C::Participants,
                &key,
                json!({"id":key,"thread_id":tid,"principal_id":p.id}),
            );
        }
    }
    // Each recipient costs at most three writes. Refuse atomically rather than truncate deliveries.
    if recipients.len() > 140 {
        return Err(err(
            413,
            "notification_fanout_limit",
            "Thread exceeds atomic notification recipient limit",
        ));
    }
    let cid = id();
    let eid = id();
    let comment = json!({"id":cid,"thread_id":tid,"actor_id":a.0,"display_name":a.1,"authenticated":a.2,"body":body,"created_at":now(),"sequence":sequence});
    tx.create(C::Comments, &cid, comment.clone());
    tx.create(C::Events,&eid,json!({"id":eid,"share_id":sid,"thread_id":tid,"actor_id":a.0,"kind":"comment","created_at":now()}));
    for recipient in recipients {
        let principal = tx.maybe(C::Principals, &hash(&recipient)).await?;
        let email = if let Some(p) = p.filter(|p| p.id == recipient) {
            p.verified_email.clone()
        } else {
            principal
                .as_ref()
                .and_then(|d| d.data["verified_email"].as_str().map(str::to_owned))
        };
        let pointer_key = hash(&format!("batch:{sid}:{recipient}"));
        let pointer = tx.maybe(C::Participants, &pointer_key).await?;
        let existing = if let Some(pointer) = &pointer {
            tx.maybe(
                C::NotificationBatches,
                pointer.data["batch_id"].as_str().unwrap(),
            )
            .await?
        } else {
            None
        };
        let reuse = existing.as_ref().filter(|b| {
            b.data["state"] == "pending"
                && b.data["notification_epoch"].as_u64().unwrap_or(0)
                    == s["notification_epoch"].as_u64().unwrap_or(0)
        });
        let bid = reuse
            .map(|b| b.data["id"].as_str().unwrap().to_owned())
            .unwrap_or_else(id);
        let count = reuse
            .map(|b| b.data["event_count"].as_u64().unwrap_or(0))
            .unwrap_or(0)
            + 1;
        let has_email = email.as_deref().is_some_and(|s| !s.is_empty());
        let batch = json!({"id":bid,"share_id":sid,"recipient":recipient,"email":email.unwrap_or_default(),"due_at":now()+120,"state":if has_email{"pending"}else{"no_delivery"},"lease":null,"error":if has_email{Value::Null}else{json!("No verified notification email")},"event_count":count,"notification_epoch":s["notification_epoch"].as_u64().unwrap_or(0)});
        if let Some(old) = reuse {
            tx.update(C::NotificationBatches, &bid, old, batch);
        } else {
            tx.create(C::NotificationBatches, &bid, batch);
            let ptr = json!({"id":pointer_key,"batch_id":bid});
            if let Some(old) = pointer {
                tx.update(C::Participants, &pointer_key, &old, ptr);
            } else {
                tx.create(C::Participants, &pointer_key, ptr);
            }
        }
        tx.create(
            C::BatchEvents,
            &hash(&format!("{bid}:{eid}")),
            json!({"batch_id":bid,"event_id":eid}),
        );
    }
    Ok(comment)
}
fn normalize(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}
fn rematch(anchor: &Value, files: &BTreeMap<String, Vec<u8>>) -> (&'static str, &'static str) {
    if anchor["kind"].as_str().unwrap_or("general") == "general" {
        return ("general", "General discussion has no placement");
    }
    let path = anchor["path"]
        .as_str()
        .or_else(|| anchor["document_path"].as_str())
        .unwrap_or("index.html");
    let Some(bytes) = files.get(path) else {
        return ("unmatched", "Original document path missing");
    };
    let text = String::from_utf8_lossy(bytes);
    let quote = anchor["exact"]
        .as_str()
        .or_else(|| anchor["quote"]["exact"].as_str())
        .unwrap_or("");
    let stable = anchor["element_id"].as_str().unwrap_or("");
    if !stable.is_empty() {
        let count = text.matches(&format!("id=\"{stable}\"")).count()
            + text.matches(&format!("id='{stable}'")).count();
        if count > 1 {
            return ("unmatched", "Duplicate stable element identifier");
        }
        if count == 1
            && !quote.is_empty()
            && normalize(&text).matches(&normalize(quote)).count() == 1
        {
            return (
                "likely",
                "Stable identifier and unique compatible text; DOM validation required",
            );
        }
    }
    if !quote.is_empty() {
        let n = normalize(&text).matches(&normalize(quote)).count();
        if n == 1 {
            return (
                "likely",
                "Unique source quote; rendered DOM confirmation required",
            );
        }
        if n > 1 {
            return ("unmatched", "Quote is ambiguous");
        }
    }
    ("unmatched", "No conservative match found")
}

pub(crate) fn finalize_key(p: &Principal, key: &str) -> String {
    hash(&json!(["finalize-upload-v1", p.id, key]).to_string())
}
