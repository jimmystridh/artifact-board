//! LOCAL TEST ADAPTER ONLY: production persistence is Firestore + GCS.
//! This SQLite implementation is not a production Cloud Run repository.
//! Local prototype policies: authenticated principals are transport-validated;
//! archive/unpublish/delete deny all viewers, restore returns to unpublished.
//! No permanent purge or production identity/tenant policy is implemented.
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    path::Path,
    sync::Mutex,
    time::{SystemTime, UNIX_EPOCH},
};
use uuid::Uuid;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Principal {
    pub id: String,
    pub display_name: String,
    pub verified_email: Option<String>,
    pub authenticated: bool,
}
#[derive(Debug, Clone, Serialize)]
pub struct AppError {
    pub status: u16,
    pub code: String,
    pub message: String,
}
impl std::fmt::Display for AppError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.code, self.message)
    }
}
impl std::error::Error for AppError {}
impl From<rusqlite::Error> for AppError {
    fn from(_: rusqlite::Error) -> Self {
        err(500, "database_error", "Database operation failed")
    }
}
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
fn token() -> String {
    format!("{}{}", Uuid::new_v4().simple(), Uuid::new_v4().simple())
}
fn auth(p: &Principal) -> Result<(), AppError> {
    if !p.authenticated || p.id.trim().is_empty() {
        Err(err(401, "unauthenticated", "Authentication required"))
    } else {
        Ok(())
    }
}
pub struct Core {
    pub conn: Mutex<Connection>,
}
impl Core {
    pub fn open(path: &Path) -> Result<Self, AppError> {
        let c = Connection::open(path)?;
        c.busy_timeout(std::time::Duration::from_secs(5))?;
        c.execute_batch(r#"
 PRAGMA foreign_keys=ON; PRAGMA journal_mode=WAL;
 CREATE TABLE IF NOT EXISTS principals(id TEXT PRIMARY KEY,name TEXT NOT NULL,email TEXT);
 CREATE TABLE IF NOT EXISTS shares(id TEXT PRIMARY KEY,owner TEXT NOT NULL,title TEXT NOT NULL,kind TEXT NOT NULL,policy TEXT NOT NULL,state TEXT NOT NULL,capability TEXT UNIQUE NOT NULL,latest TEXT,created INTEGER NOT NULL,idem TEXT NOT NULL,UNIQUE(owner,idem));
 CREATE TABLE IF NOT EXISTS uploads(id TEXT PRIMARY KEY,share TEXT NOT NULL REFERENCES shares(id),owner TEXT NOT NULL,token TEXT NOT NULL,expires INTEGER NOT NULL,revision TEXT);
 CREATE TABLE IF NOT EXISTS upload_staging(upload TEXT PRIMARY KEY REFERENCES uploads(id),files BLOB NOT NULL);
 CREATE TABLE IF NOT EXISTS finalize_requests(owner TEXT NOT NULL,key TEXT NOT NULL,upload TEXT UNIQUE NOT NULL REFERENCES uploads(id),entrypoint TEXT NOT NULL,source_digest TEXT NOT NULL,share_version INTEGER NOT NULL,result TEXT,PRIMARY KEY(owner,key));
 CREATE TABLE IF NOT EXISTS finalize_attempts(upload TEXT PRIMARY KEY REFERENCES uploads(id),claim TEXT NOT NULL,lease_until INTEGER NOT NULL,error TEXT);
 CREATE TABLE IF NOT EXISTS share_versions(share TEXT PRIMARY KEY REFERENCES shares(id),version INTEGER NOT NULL);
 CREATE TABLE IF NOT EXISTS revisions(id TEXT PRIMARY KEY,share TEXT NOT NULL REFERENCES shares(id),entrypoint TEXT NOT NULL,digest TEXT NOT NULL,manifest TEXT NOT NULL,created INTEGER NOT NULL);
 CREATE TABLE IF NOT EXISTS deleted_revisions(revision TEXT PRIMARY KEY REFERENCES revisions(id),deleted INTEGER NOT NULL);
 CREATE TABLE IF NOT EXISTS thread_states(thread TEXT PRIMARY KEY REFERENCES threads(id),state TEXT NOT NULL);
 CREATE TABLE IF NOT EXISTS files(revision TEXT NOT NULL REFERENCES revisions(id),path TEXT NOT NULL,bytes BLOB NOT NULL,PRIMARY KEY(revision,path));
 CREATE TABLE IF NOT EXISTS threads(id TEXT PRIMARY KEY,share TEXT NOT NULL REFERENCES shares(id),revision TEXT NOT NULL REFERENCES revisions(id),anchor TEXT NOT NULL,resolved INTEGER NOT NULL DEFAULT 0,created INTEGER NOT NULL);
 CREATE TABLE IF NOT EXISTS comment_receipts(share TEXT NOT NULL REFERENCES shares(id),actor_scope TEXT NOT NULL,operation TEXT NOT NULL,request_key TEXT NOT NULL,fingerprint TEXT NOT NULL,response_json TEXT NOT NULL,created INTEGER NOT NULL,PRIMARY KEY(share,actor_scope,operation,request_key));
 CREATE TABLE IF NOT EXISTS comments(id TEXT PRIMARY KEY,thread TEXT NOT NULL REFERENCES threads(id),actor TEXT NOT NULL,name TEXT NOT NULL,verified INTEGER NOT NULL,body TEXT NOT NULL,created INTEGER NOT NULL);
 CREATE TABLE IF NOT EXISTS participants(thread TEXT NOT NULL REFERENCES threads(id),principal TEXT NOT NULL REFERENCES principals(id),PRIMARY KEY(thread,principal));
 CREATE TABLE IF NOT EXISTS placements(thread TEXT NOT NULL REFERENCES threads(id),revision TEXT NOT NULL REFERENCES revisions(id),status TEXT NOT NULL,anchor TEXT NOT NULL,reason TEXT NOT NULL,PRIMARY KEY(thread,revision));
 CREATE TABLE IF NOT EXISTS events(id TEXT PRIMARY KEY,share TEXT NOT NULL,thread TEXT NOT NULL,actor TEXT NOT NULL,kind TEXT NOT NULL,created INTEGER NOT NULL);
 CREATE TABLE IF NOT EXISTS batches(id TEXT PRIMARY KEY,share TEXT NOT NULL,recipient TEXT NOT NULL,email TEXT NOT NULL,due INTEGER NOT NULL,state TEXT NOT NULL,lease TEXT,error TEXT);
 CREATE UNIQUE INDEX IF NOT EXISTS pending_batch ON batches(share,recipient) WHERE state='pending';
 CREATE TABLE IF NOT EXISTS batch_events(batch TEXT NOT NULL REFERENCES batches(id),event TEXT NOT NULL REFERENCES events(id),PRIMARY KEY(batch,event));
 CREATE TRIGGER IF NOT EXISTS immutable_revision BEFORE UPDATE ON revisions BEGIN SELECT RAISE(ABORT,'immutable revision'); END;
 CREATE TRIGGER IF NOT EXISTS immutable_file BEFORE UPDATE ON files BEGIN SELECT RAISE(ABORT,'immutable artifact'); END;
 CREATE TRIGGER IF NOT EXISTS immutable_origin BEFORE UPDATE OF revision,anchor ON threads BEGIN SELECT RAISE(ABORT,'immutable thread origin'); END;
 PRAGMA user_version=1;
 "#)?;
        Ok(Self {
            conn: Mutex::new(c),
        })
    }
    fn lock(&self) -> Result<std::sync::MutexGuard<'_, Connection>, AppError> {
        self.conn
            .lock()
            .map_err(|_| err(500, "database_busy", "Database unavailable"))
    }
    pub fn list_shares(&self, p: &Principal) -> Result<Value, AppError> {
        auth(p)?;
        let c = self.lock()?;
        let mut q = c.prepare("SELECT id FROM shares WHERE owner=? ORDER BY created DESC,id")?;
        let ids = q
            .query_map([&p.id], |r| r.get::<_, String>(0))?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(json!(
            ids.iter()
                .map(|s| share(&c, s, true))
                .collect::<Result<Vec<_>, _>>()?
        ))
    }
    pub fn create_share(
        &self,
        p: &Principal,
        title: &str,
        kind: &str,
        policy: &str,
        idempotency_key: &str,
    ) -> Result<Value, AppError> {
        auth(p)?;
        if title.trim().is_empty()
            || title.len() > 300
            || idempotency_key.is_empty()
            || idempotency_key.len() > 200
        {
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
        let mut c = self.lock()?;
        let tx = c.transaction()?;
        save_principal(&tx, p)?;
        let old: Option<String> = tx
            .query_row(
                "SELECT id FROM shares WHERE owner=? AND idem=?",
                params![p.id, idempotency_key],
                |r| r.get(0),
            )
            .optional()?;
        let sid = if let Some(s) = old {
            let v = share(&tx, &s, true)?;
            if v["title"] != title || v["kind"] != kind || v["policy"] != policy {
                return Err(err(
                    409,
                    "idempotency_conflict",
                    "Key already used with different share input",
                ));
            }
            s
        } else {
            let s = id();
            tx.execute(
                "INSERT INTO shares VALUES(?,?,?,?,?,'pending',?,NULL,?,?)",
                params![
                    s,
                    p.id,
                    title,
                    kind,
                    policy,
                    token(),
                    now(),
                    idempotency_key
                ],
            )?;
            s
        };
        let out = share(&tx, &sid, true)?;
        tx.commit()?;
        Ok(out)
    }
    pub fn get_share(&self, p: &Principal, sid: &str) -> Result<Value, AppError> {
        let c = self.lock()?;
        owner(&c, p, sid)?;
        let mut out = share(&c, sid, true)?;
        let mut q = c.prepare(
            "SELECT id,entrypoint,digest,created FROM revisions WHERE share=? AND id NOT IN (SELECT revision FROM deleted_revisions) ORDER BY created,id",
        )?;
        let rows=q.query_map([sid],|r|Ok(json!({"id":r.get::<_,String>(0)?,"entrypoint":r.get::<_,String>(1)?,"digest":r.get::<_,String>(2)?,"created_at":r.get::<_,i64>(3)?})))?.collect::<Result<Vec<_>,_>>()?;
        out["revisions"] = json!(rows);
        Ok(out)
    }
    pub fn create_upload(&self, p: &Principal, sid: &str) -> Result<Value, AppError> {
        let c = self.lock()?;
        owner(&c, p, sid)?;
        writable(&c, sid)?;
        let uid = id();
        c.execute(
            "INSERT INTO uploads VALUES(?,?,?,?,?,NULL)",
            params![uid, sid, p.id, token(), now() + 3600],
        )?;
        upload(&c, &uid)
    }
    pub fn get_upload(&self, p: &Principal, uid: &str) -> Result<Value, AppError> {
        auth(p)?;
        let c = self.lock()?;
        let v = upload(&c, uid)?;
        owner(&c, p, v["share_id"].as_str().unwrap())?;
        Ok(v)
    }
    /// Staging and freezing share the same durable transaction boundary.
    pub fn stage_upload(
        &self,
        p: &Principal,
        uid: &str,
        files: &BTreeMap<String, Vec<u8>>,
        _entrypoint: &str,
    ) -> Result<Value, AppError> {
        validate_files(files, files.keys().next().map(String::as_str).unwrap_or(""))?;
        let mut c = self.lock()?;
        let tx = c.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let u = upload(&tx, uid)?;
        let sid = u["share_id"].as_str().unwrap();
        owner(&tx, p, sid)?;
        writable(&tx, sid)?;
        if u["expires_at"].as_i64().unwrap_or(0) < now() {
            return Err(err(410, "expired_upload", "Upload session expired"));
        }
        if u["revision_id"].is_string() {
            return Err(err(
                409,
                "already_finalized",
                "Upload was already finalized",
            ));
        }
        let frozen: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM finalize_requests WHERE upload=?)",
            [uid],
            |r| r.get(0),
        )?;
        if frozen {
            return Err(err(
                409,
                "upload_frozen",
                "Upload is frozen for its finalization request",
            ));
        }
        tx.execute("INSERT INTO upload_staging(upload,files) VALUES(?,?) ON CONFLICT(upload) DO UPDATE SET files=excluded.files",params![uid,serde_json::to_vec(files).unwrap()])?;
        tx.commit()?;
        Ok(json!({"staged":true,"upload_id":uid}))
    }
    pub fn staged_files(
        &self,
        p: &Principal,
        uid: &str,
    ) -> Result<(BTreeMap<String, Vec<u8>>, String), AppError> {
        let c = self.lock()?;
        let u = upload(&c, uid)?;
        owner(&c, p, u["share_id"].as_str().unwrap())?;
        let bytes: Option<Vec<u8>> = c
            .query_row(
                "SELECT files FROM upload_staging WHERE upload=?",
                [uid],
                |r| r.get(0),
            )
            .optional()?;
        let files =
            serde_json::from_slice(&bytes.ok_or_else(|| {
                err(409, "incomplete_upload", "Upload files before finalization")
            })?)
            .map_err(|_| err(500, "storage_error", "Invalid staging record"))?;
        Ok((files, "index.html".into()))
    }
    pub fn begin_finalize(
        &self,
        p: &Principal,
        uid: &str,
        key: &str,
        entrypoint: &str,
    ) -> Result<Value, AppError> {
        validate_finalize_key(key)?;
        let mut c = self.lock()?;
        let tx = c.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let u = upload(&tx, uid)?;
        let sid = u["share_id"].as_str().unwrap();
        owner(&tx, p, sid)?;
        writable(&tx, sid)?;
        let old: Option<(String,String,i64,Option<String>)>=tx.query_row("SELECT upload,entrypoint,share_version,result FROM finalize_requests WHERE owner=? AND key=?",params![p.id,key],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?))).optional()?;
        if let Some((old_uid, old_entry, version, result)) = old {
            if old_uid != uid || old_entry != entrypoint {
                return Err(err(
                    409,
                    "idempotency_conflict",
                    "Finalization key already binds different input",
                ));
            }
            let result = result
                .map(|s| {
                    serde_json::from_str::<Value>(&s)
                        .map_err(|_| err(500, "storage_error", "Invalid finalization receipt"))
                })
                .transpose()?;
            if let Some(failure) = finalization_failure(&tx, uid)? {
                return Err(failure);
            }
            if result.is_none() && version != share_version(&tx, sid)? {
                return Err(err(
                    409,
                    "stale_build",
                    "Share changed after finalization began",
                ));
            }
            return Ok(
                json!({"entrypoint":entrypoint,"expected_share_version":version,"result":result}),
            );
        }
        let frozen: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM finalize_requests WHERE upload=?)",
            [uid],
            |r| r.get(0),
        )?;
        if frozen {
            return Err(err(
                409,
                "idempotency_conflict",
                "Upload already binds another finalization key",
            ));
        }
        if u["revision_id"].is_string() {
            return Err(err(
                409,
                "already_finalized",
                "Upload was already finalized",
            ));
        }
        if u["expires_at"].as_i64().unwrap_or(0) < now() {
            return Err(err(410, "expired_upload", "Upload session expired"));
        }
        let bytes: Option<Vec<u8>> = tx
            .query_row(
                "SELECT files FROM upload_staging WHERE upload=?",
                [uid],
                |r| r.get(0),
            )
            .optional()?;
        let bytes = bytes
            .ok_or_else(|| err(409, "incomplete_upload", "Upload files before finalization"))?;
        let digest = format!("{:x}", Sha256::digest(&bytes));
        let version = share_version(&tx, sid)?;
        tx.execute("INSERT INTO finalize_requests(owner,key,upload,entrypoint,source_digest,share_version,result) VALUES(?,?,?,?,?,?,NULL)",params![p.id,key,uid,entrypoint,digest,version])?;
        tx.commit()?;
        Ok(json!({"entrypoint":entrypoint,"expected_share_version":version,"result":null}))
    }
    pub fn publish(
        &self,
        p: &Principal,
        uid: &str,
        files: &BTreeMap<String, Vec<u8>>,
        entrypoint: &str,
    ) -> Result<Value, AppError> {
        self.publish_inner(p, uid, files, entrypoint, None, None)
    }
    #[allow(clippy::too_many_arguments)]
    pub fn publish_finalized(
        &self,
        p: &Principal,
        uid: &str,
        key: &str,
        files: &BTreeMap<String, Vec<u8>>,
        entrypoint: &str,
        warnings: &Value,
    ) -> Result<Value, AppError> {
        self.publish_inner(p, uid, files, entrypoint, Some((key, warnings)), None)
    }
    #[allow(clippy::too_many_arguments)]
    pub fn publish_claimed(
        &self,
        p: &Principal,
        uid: &str,
        key: &str,
        claim: &str,
        files: &BTreeMap<String, Vec<u8>>,
        entrypoint: &str,
        warnings: &Value,
    ) -> Result<Value, AppError> {
        self.publish_inner(
            p,
            uid,
            files,
            entrypoint,
            Some((key, warnings)),
            Some(claim),
        )
    }
    #[allow(clippy::too_many_arguments)]
    fn publish_inner(
        &self,
        p: &Principal,
        uid: &str,
        files: &BTreeMap<String, Vec<u8>>,
        entrypoint: &str,
        finalized: Option<(&str, &Value)>,
        claim: Option<&str>,
    ) -> Result<Value, AppError> {
        auth(p)?;
        let mut c = self.lock()?;
        let tx = c.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let u = upload(&tx, uid)?;
        let sid = u["share_id"].as_str().unwrap();
        owner(&tx, p, sid)?;
        writable(&tx, sid)?;
        let reservation: Option<(String, i64, Option<String>, String)> = tx
            .query_row(
                "SELECT key,share_version,result,source_digest FROM finalize_requests WHERE upload=? AND owner=?",
                params![uid, p.id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )
            .optional()?;
        match (&reservation, finalized) {
            (Some((reserved_key, version, result, source_digest)), Some((key, _)))
                if reserved_key == key =>
            {
                if let Some(result) = result {
                    return serde_json::from_str(result)
                        .map_err(|_| err(500, "storage_error", "Invalid finalization receipt"));
                }
                if let Some(failure) = finalization_failure(&tx, uid)? {
                    return Err(failure);
                }
                check_finalize_claim(&tx, uid, claim)?;
                if *version != share_version(&tx, sid)? {
                    return Err(err(
                        409,
                        "stale_build",
                        "Share changed after finalization began",
                    ));
                }
                let bytes: Vec<u8> = tx.query_row(
                    "SELECT files FROM upload_staging WHERE upload=?",
                    [uid],
                    |r| r.get(0),
                )?;
                if *source_digest != format!("{:x}", Sha256::digest(&bytes)) {
                    return Err(err(409, "source_changed", "Frozen upload source changed"));
                }
            }
            (Some(_), _) => {
                return Err(err(
                    409,
                    "upload_frozen",
                    "Upload belongs to a different finalization request",
                ));
            }
            (None, Some(_)) => {
                return Err(err(
                    409,
                    "finalization_required",
                    "Reserve finalization before publication",
                ));
            }
            (None, None) => {}
        }
        if let Some(r) = u["revision_id"].as_str() {
            let mut s = share(&tx, sid, true)?;
            s["revision_id"] = json!(r);
            return Ok(s);
        }
        if u["expires_at"].as_i64().unwrap() < now() {
            return Err(err(410, "expired_upload", "Upload session expired"));
        }
        validate_files(files, entrypoint)?;
        let rid = id();
        let mut h = Sha256::new();
        let mut manifest = vec![];
        for (path, bytes) in files {
            h.update((path.len() as u64).to_be_bytes());
            h.update(path.as_bytes());
            h.update((bytes.len() as u64).to_be_bytes());
            h.update(bytes);
            manifest.push(json!({"path":path,"size":bytes.len(),"sha256":format!("{:x}",Sha256::digest(bytes))}));
        }
        tx.execute(
            "INSERT INTO revisions VALUES(?,?,?,?,?,?)",
            params![
                rid,
                sid,
                entrypoint,
                format!("{:x}", h.finalize()),
                json!(manifest).to_string(),
                now()
            ],
        )?;
        for (path, bytes) in files {
            tx.execute("INSERT INTO files VALUES(?,?,?)", params![rid, path, bytes])?;
        }
        tx.execute(
            "UPDATE uploads SET revision=? WHERE id=?",
            params![rid, uid],
        )?;
        tx.execute(
            "UPDATE shares SET latest=?,state='published' WHERE id=?",
            params![rid, sid],
        )?;
        bump_share_version(&tx, sid)?;
        let origins = {
            let mut q = tx.prepare("SELECT id,anchor FROM threads WHERE share=? AND resolved=0 AND id NOT IN (SELECT thread FROM thread_states WHERE state IN ('archived','deleted'))")?;

            q.query_map([sid], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
            })?
            .collect::<Result<Vec<_>, _>>()?
        };
        for (t, a) in origins {
            let anchor: Value = serde_json::from_str(&a).unwrap_or(json!({}));
            let (status, reason) = rematch(&anchor, files);
            tx.execute(
                "INSERT INTO placements VALUES(?,?,?,?,?)",
                params![t, rid, status, a, reason],
            )?;
        }
        let mut out = share(&tx, sid, true)?;
        out["revision_id"] = json!(rid);
        if let Some((key, warnings)) = finalized {
            out = json!({"share_id":sid,"revision_id":rid,"viewer_url":format!("/s/{}",out["capability"].as_str().unwrap()),"state":"ready","warnings":warnings});
            tx.execute(
                "UPDATE finalize_requests SET result=? WHERE owner=? AND key=? AND upload=?",
                params![out.to_string(), p.id, key, uid],
            )?;
        }
        tx.commit()?;
        Ok(out)
    }
    pub fn viewer_share(&self, capability: &str, p: Option<&Principal>) -> Result<Value, AppError> {
        let c = self.lock()?;
        let sid = view(&c, capability, p)?;
        let mut v = share(&c, &sid, false)?;
        let mut q = c.prepare(
            "SELECT id,entrypoint,created FROM revisions WHERE share=? AND id NOT IN (SELECT revision FROM deleted_revisions) ORDER BY created,rowid",
        )?;
        let rows=q.query_map([&sid],|r|Ok(json!({"id":r.get::<_,String>(0)?,"entrypoint":r.get::<_,String>(1)?,"created_at":r.get::<_,i64>(2)?})))?.collect::<Result<Vec<_>,_>>()?;
        v["revisions"] = json!(rows);
        Ok(v)
    }
    pub fn viewer_revision(
        &self,
        capability: &str,
        revision: Option<&str>,
        p: Option<&Principal>,
    ) -> Result<Value, AppError> {
        let c = self.lock()?;
        let sid = view(&c, capability, p)?;
        let rid = match revision {
            Some(r) => r.to_string(),
            None => c.query_row("SELECT latest FROM shares WHERE id=?", [&sid], |r| r.get(0))?,
        };
        c.query_row("SELECT id,entrypoint,digest,created FROM revisions WHERE id=? AND share=? AND id NOT IN (SELECT revision FROM deleted_revisions)",params![rid,sid],|r|Ok(json!({"id":r.get::<_,String>(0)?,"entrypoint":r.get::<_,String>(1)?,"digest":r.get::<_,String>(2)?,"created_at":r.get::<_,i64>(3)?}))).optional()?.ok_or_else(||err(404,"not_found","Revision not found"))
    }
    pub fn revision_files(
        &self,
        capability: &str,
        revision: Option<&str>,
        p: Option<&Principal>,
    ) -> Result<BTreeMap<String, Vec<u8>>, AppError> {
        let c = self.lock()?;
        let sid = view(&c, capability, p)?;
        let rid = match revision {
            Some(r) => r.to_string(),
            None => c.query_row("SELECT latest FROM shares WHERE id=?", [&sid], |r| r.get(0))?,
        };
        let exists: bool = c.query_row(
            "SELECT EXISTS(SELECT 1 FROM revisions WHERE id=? AND share=? AND id NOT IN (SELECT revision FROM deleted_revisions))",
            params![rid, sid],
            |r| r.get(0),
        )?;
        if !exists {
            return Err(err(404, "not_found", "Revision not found"));
        }
        let mut q = c.prepare("SELECT path,bytes FROM files WHERE revision=?")?;
        let rows = q
            .query_map([rid], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, Vec<u8>>(1)?))
            })?
            .collect::<Result<BTreeMap<_, _>, _>>()?;
        Ok(rows)
    }
    pub fn threads(&self, capability: &str, p: Option<&Principal>) -> Result<Value, AppError> {
        let c = self.lock()?;
        let sid = view(&c, capability, p)?;
        thread_list(&c, &sid)
    }
    pub fn add_thread(
        &self,
        capability: &str,
        p: Option<&Principal>,
        guest_name: Option<&str>,
        body: &str,
        revision: &str,
        anchor: Value,
    ) -> Result<Value, AppError> {
        self.add_thread_inner(capability, p, guest_name, body, revision, anchor, None)
    }
    /// The transport supplies an authenticated principal or signed guest-cookie scope.
    /// Receipts remain available for the lifetime of the retained comment data.
    #[allow(clippy::too_many_arguments)]
    pub fn add_thread_idempotent(
        &self,
        capability: &str,
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
            capability,
            p,
            guest_name,
            body,
            revision,
            anchor,
            Some((key, actor_scope)),
        )
    }
    #[allow(clippy::too_many_arguments)]
    fn add_thread_inner(
        &self,
        capability: &str,
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
        let mut c = self.lock()?;
        // Serialize writers before taking the replay snapshot, including other Core instances.
        let tx = c.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let sid = view(&tx, capability, p)?;
        if let Some(result) = replay_comment(&tx, &sid, "thread", receipt, &fingerprint)? {
            active_thread(&tx, result["id"].as_str().unwrap_or(""))?;
            return Ok(result);
        }
        valid_body(body)?;
        if anchor.to_string().len() > 16384 || !anchor.is_object() {
            return Err(err(
                400,
                "invalid_anchor",
                "Anchor must be a bounded JSON object",
            ));
        }
        let valid: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM revisions WHERE id=? AND share=? AND id NOT IN (SELECT revision FROM deleted_revisions))",
            params![revision, sid],
            |r| r.get(0),
        )?;
        if !valid {
            return Err(err(
                400,
                "invalid_revision",
                "Revision must belong to this share",
            ));
        }
        let actor = actor(&tx, p, guest_name)?;
        let tid = id();
        tx.execute(
            "INSERT INTO threads VALUES(?,?,?,?,0,?)",
            params![tid, sid, revision, anchor.to_string(), now()],
        )?;
        let comment_id = post(&tx, &sid, &tid, &actor, p, body, now())?;
        let mut result = thread(&tx, &tid)?;
        comment_mutation(&mut result, receipt, "thread", &comment_id);
        save_comment_receipt(&tx, &sid, "thread", receipt, &fingerprint, &result)?;
        tx.commit()?;
        Ok(result)
    }
    pub fn reply(
        &self,
        capability: &str,
        tid: &str,
        p: Option<&Principal>,
        guest_name: Option<&str>,
        body: &str,
    ) -> Result<Value, AppError> {
        self.reply_inner(capability, tid, p, guest_name, body, None)
    }
    #[allow(clippy::too_many_arguments)]
    pub fn reply_idempotent(
        &self,
        capability: &str,
        tid: &str,
        p: Option<&Principal>,
        guest_name: Option<&str>,
        body: &str,
        key: &str,
        actor_scope: &str,
    ) -> Result<Value, AppError> {
        validate_comment_idempotency(p, key, actor_scope)?;
        self.reply_inner(
            capability,
            tid,
            p,
            guest_name,
            body,
            Some((key, actor_scope)),
        )
    }
    fn reply_inner(
        &self,
        capability: &str,
        tid: &str,
        p: Option<&Principal>,
        guest_name: Option<&str>,
        body: &str,
        receipt: Option<(&str, &str)>,
    ) -> Result<Value, AppError> {
        let fingerprint = comment_fingerprint(&json!({
            "guest_name": guest_name, "body": body, "thread": tid,
        }));
        let mut c = self.lock()?;
        let tx = c.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let sid = view(&tx, capability, p)?;
        if let Some(result) = replay_comment(&tx, &sid, "reply", receipt, &fingerprint)? {
            active_thread(&tx, result["id"].as_str().unwrap_or(""))?;
            return Ok(result);
        }
        valid_body(body)?;
        thread_belongs(&tx, &sid, tid)?;
        active_thread(&tx, tid)?;
        let a = actor(&tx, p, guest_name)?;
        let comment_id = post(&tx, &sid, tid, &a, p, body, now())?;
        let mut result = thread(&tx, tid)?;
        comment_mutation(&mut result, receipt, "reply", &comment_id);
        save_comment_receipt(&tx, &sid, "reply", receipt, &fingerprint, &result)?;
        tx.commit()?;
        Ok(result)
    }
    pub fn resolve(
        &self,
        p: &Principal,
        sid: &str,
        tid: &str,
        resolved: bool,
    ) -> Result<Value, AppError> {
        let c = self.lock()?;
        owner(&c, p, sid)?;
        writable(&c, sid)?;
        thread_belongs(&c, sid, tid)?;
        active_thread(&c, tid)?;
        c.execute(
            "UPDATE threads SET resolved=? WHERE id=?",
            params![resolved, tid],
        )?;
        thread(&c, tid)
    }
    pub fn delete_revision(&self, p: &Principal, sid: &str, rid: &str) -> Result<Value, AppError> {
        let mut c = self.lock()?;
        let tx = c.transaction_with_behavior(TransactionBehavior::Immediate)?;
        owner(&tx, p, sid)?;
        writable(&tx, sid)?;
        let exists: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM revisions WHERE id=? AND share=?)",
            params![rid, sid],
            |r| r.get(0),
        )?;
        if !exists {
            return Err(err(404, "not_found", "Revision not found"));
        }
        let latest: Option<String> =
            tx.query_row("SELECT latest FROM shares WHERE id=?", [sid], |r| r.get(0))?;
        if latest.as_deref() == Some(rid) {
            return Err(err(
                409,
                "latest_revision",
                "The latest revision cannot be deleted. Publish a new revision first.",
            ));
        }
        tx.execute(
            "INSERT OR IGNORE INTO deleted_revisions VALUES(?,?)",
            params![rid, now()],
        )?;
        tx.commit()?;
        Ok(json!({"id":rid,"state":"deleted"}))
    }
    pub fn moderate_thread(
        &self,
        p: &Principal,
        sid: &str,
        tid: &str,
        action: &str,
    ) -> Result<Value, AppError> {
        let mut c = self.lock()?;
        let tx = c.transaction_with_behavior(TransactionBehavior::Immediate)?;
        owner(&tx, p, sid)?;
        writable(&tx, sid)?;
        thread_belongs(&tx, sid, tid)?;
        let state = thread_state(&tx, tid)?;
        let next = discussion_transition(&state, action)?;
        tx.execute("INSERT INTO thread_states VALUES(?,?) ON CONFLICT(thread) DO UPDATE SET state=excluded.state",params![tid,next])?;
        tx.commit()?;
        Ok(json!({"id":tid,"state":next}))
    }
    pub fn lifecycle(&self, p: &Principal, sid: &str, action: &str) -> Result<Value, AppError> {
        let mut c = self.lock()?;
        let tx = c.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        owner(&tx, p, sid)?;
        let state: String =
            tx.query_row("SELECT state FROM shares WHERE id=?", [sid], |r| r.get(0))?;
        let next = match action {
            "unpublish" if state != "deleted" => "unpublished",
            "archive" if state != "deleted" => "archived",
            "delete" => "deleted",
            "restore" if state == "deleted" || state == "archived" => "unpublished",
            "republish" if state == "unpublished" => {
                let latest: Option<String> =
                    tx.query_row("SELECT latest FROM shares WHERE id=?", [sid], |r| r.get(0))?;
                if latest.is_none() {
                    return Err(err(409, "state_conflict", "No ready revision to republish"));
                }
                "published"
            }
            _ => return Err(err(409, "state_conflict", "Invalid lifecycle transition")),
        };
        tx.execute("UPDATE shares SET state=? WHERE id=?", params![next, sid])?;
        bump_share_version(&tx, sid)?;
        if next != "published" {
            tx.execute("UPDATE batches SET state='cancelled',error='share unavailable' WHERE share=? AND state IN ('pending','sending')",[sid])?;
        }
        let out = share(&tx, sid, true)?;
        tx.commit()?;
        Ok(out)
    }
    /// Atomically claim only quiet, currently published batches. An arrival after claim creates a new batch.
    pub fn claim_due_notifications(&self, at: i64) -> Result<Value, AppError> {
        let mut c = self.lock()?;
        let tx = c.transaction()?;
        let rows = {
            let mut q=tx.prepare("SELECT b.id,b.share,b.recipient,b.email FROM batches b JOIN shares s ON s.id=b.share WHERE b.state='pending' AND b.due<=? AND s.state='published' ORDER BY b.due")?;

            q.query_map([at], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, String>(3)?,
                ))
            })?
            .collect::<Result<Vec<_>, _>>()?
        };
        let mut result = vec![];
        for (b, s, r, e) in rows {
            let lease = token();
            tx.execute(
                "UPDATE batches SET state='sending',lease=? WHERE id=?",
                params![lease, b],
            )?;
            let n: i64 = tx.query_row(
                "SELECT COUNT(*) FROM batch_events WHERE batch=?",
                [&b],
                |r| r.get(0),
            )?;
            result.push(json!({"id":b,"share_id":s,"recipient":r,"verified_email":e,"lease":lease,"event_count":n}));
        }
        tx.commit()?;
        Ok(json!(result))
    }
    /// Adapters must recheck this immediately before sending; no real adapter is configured.
    pub fn notification_eligible(&self, batch: &str, lease: &str) -> Result<bool, AppError> {
        let c = self.lock()?;
        Ok(c.query_row("SELECT EXISTS(SELECT 1 FROM batches b JOIN shares s ON s.id=b.share WHERE b.id=? AND b.lease=? AND b.state='sending' AND s.state='published')",params![batch,lease],|r|r.get(0))?)
    }
    pub fn finish_notification(
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
        let c = self.lock()?;
        if c.execute(
            "UPDATE batches SET state=?,error=? WHERE id=? AND lease=? AND state='sending'",
            params![outcome, detail, batch, lease],
        )? != 1
        {
            return Err(err(409, "state_conflict", "Batch no longer claimed"));
        }
        Ok(())
    }
    pub fn notification_status(&self, p: &Principal, sid: &str) -> Result<Value, AppError> {
        let c = self.lock()?;
        owner(&c, p, sid)?;
        let mut q = c.prepare(
            "SELECT id,recipient,due,state,error FROM batches WHERE share=? ORDER BY due",
        )?;
        let rows=q.query_map([sid],|r|Ok(json!({"id":r.get::<_,String>(0)?,"recipient":r.get::<_,String>(1)?,"due_at":r.get::<_,i64>(2)?,"state":r.get::<_,String>(3)?,"error":r.get::<_,Option<String>>(4)?})))?.collect::<Result<Vec<_>,_>>()?;
        Ok(json!(rows))
    }
}
/// Scope is trusted transport input, never a guest display name or request field.
/// Keep authenticated scopes bound to the actual principal even for direct store callers.
pub(crate) fn validate_comment_idempotency(
    p: Option<&Principal>,
    key: &str,
    scope: &str,
) -> Result<(), AppError> {
    if key.trim().is_empty()
        || key.len() > 200
        || key.chars().any(char::is_control)
        || scope.trim().is_empty()
        || scope.len() > 512
        || scope.chars().any(char::is_control)
    {
        return Err(err(
            400,
            "invalid_idempotency_key",
            "A bounded idempotency key and actor scope are required",
        ));
    }
    if let Some(p) = p {
        auth(p)?;
        if scope != format!("auth:{}", p.id) {
            return Err(err(
                403,
                "invalid_actor_scope",
                "Actor scope must match the authenticated principal",
            ));
        }
    } else if !scope.starts_with("guest:") || scope.len() <= "guest:".len() {
        return Err(err(
            400,
            "invalid_actor_scope",
            "A signed guest actor scope is required",
        ));
    }
    Ok(())
}
pub(crate) fn comment_fingerprint(payload: &Value) -> String {
    // serde_json's sorted object keys give stable structural JSON without trimming any input.
    format!("{:x}", Sha256::digest(payload.to_string().as_bytes()))
}
pub(crate) fn comment_mutation(
    response: &mut Value,
    receipt: Option<(&str, &str)>,
    operation: &str,
    comment_id: &str,
) {
    if let Some((key, _)) = receipt {
        response["_mutation"] = json!({
            "idempotency_key": key, "operation": operation, "comment_id": comment_id,
        });
    }
}
pub(crate) fn comment_response_json(response: &Value) -> Result<String, AppError> {
    let encoded = response.to_string();
    // Store JSON as a string rather than an indexed nested map in Firestore. Leave ample
    // headroom below its 1 MiB document limit; both adapters enforce the same bound.
    if encoded.len() > 512 * 1024 {
        return Err(err(
            413,
            "comment_response_limit",
            "Thread exceeds the durable comment response limit",
        ));
    }
    Ok(encoded)
}
pub(crate) fn replay_comment_response(
    fingerprint: &str,
    old_fingerprint: &str,
    response: &str,
) -> Result<Value, AppError> {
    if old_fingerprint != fingerprint {
        return Err(err(
            409,
            "idempotency_conflict",
            "Key already used with different comment input",
        ));
    }
    serde_json::from_str(response).map_err(|_| {
        err(
            500,
            "invalid_comment_receipt",
            "Stored comment response is unavailable",
        )
    })
}
fn replay_comment(
    c: &Connection,
    sid: &str,
    operation: &str,
    receipt: Option<(&str, &str)>,
    fingerprint: &str,
) -> Result<Option<Value>, AppError> {
    let Some((key, scope)) = receipt else {
        return Ok(None);
    };
    let old: Option<(String, String)> = c.query_row(
        "SELECT fingerprint,response_json FROM comment_receipts WHERE share=? AND actor_scope=? AND operation=? AND request_key=?",
        params![sid, scope, operation, key], |r| Ok((r.get(0)?, r.get(1)?)),
    ).optional()?;
    old.map(|(old_fingerprint, response)| {
        replay_comment_response(fingerprint, &old_fingerprint, &response)
    })
    .transpose()
}
fn save_comment_receipt(
    c: &Connection,
    sid: &str,
    operation: &str,
    receipt: Option<(&str, &str)>,
    fingerprint: &str,
    response: &Value,
) -> Result<(), AppError> {
    if let Some((key, scope)) = receipt {
        let encoded = comment_response_json(response)?;
        c.execute(
            "INSERT INTO comment_receipts VALUES(?,?,?,?,?,?,?)",
            params![sid, scope, operation, key, fingerprint, encoded, now()],
        )?;
    }
    Ok(())
}
fn save_principal(c: &Connection, p: &Principal) -> Result<(), AppError> {
    auth(p)?;
    c.execute("INSERT INTO principals VALUES(?,?,?) ON CONFLICT(id) DO UPDATE SET name=excluded.name,email=excluded.email",params![p.id,p.display_name,p.verified_email])?;
    Ok(())
}
fn owner(c: &Connection, p: &Principal, sid: &str) -> Result<(), AppError> {
    auth(p)?;
    let o: Option<String> = c
        .query_row("SELECT owner FROM shares WHERE id=?", [sid], |r| r.get(0))
        .optional()?;
    match o {
        Some(o) if o == p.id => Ok(()),
        Some(_) => Err(err(403, "forbidden", "Share belongs to another publisher")),
        None => Err(err(404, "not_found", "Share not found")),
    }
}
fn writable(c: &Connection, sid: &str) -> Result<(), AppError> {
    let s: String = c.query_row("SELECT state FROM shares WHERE id=?", [sid], |r| r.get(0))?;
    if s == "deleted" || s == "archived" {
        Err(err(
            409,
            "state_conflict",
            "Restore share before making changes",
        ))
    } else {
        Ok(())
    }
}
fn share(c: &Connection, sid: &str, management: bool) -> Result<Value, AppError> {
    let mut v=c.query_row("SELECT id,title,kind,policy,state,latest,created,capability,owner FROM shares WHERE id=?",[sid],|r|Ok(json!({"id":r.get::<_,String>(0)?,"title":r.get::<_,String>(1)?,"kind":r.get::<_,String>(2)?,"policy":r.get::<_,String>(3)?,"state":r.get::<_,String>(4)?,"latest_revision":r.get::<_,Option<String>>(5)?,"created_at":r.get::<_,i64>(6)?,"capability":r.get::<_,String>(7)?,"owner_id":r.get::<_,String>(8)?}))).optional()?.ok_or_else(||err(404,"not_found","Share not found"))?;
    v["ready"] = json!(v["state"] == "published");
    if !management {
        v.as_object_mut().unwrap().remove("capability");
        v.as_object_mut().unwrap().remove("owner_id");
    }
    Ok(v)
}
fn upload(c: &Connection, uid: &str) -> Result<Value, AppError> {
    c.query_row("SELECT id,share,token,expires,revision FROM uploads WHERE id=?",[uid],|r|Ok(json!({"id":r.get::<_,String>(0)?,"share_id":r.get::<_,String>(1)?,"token":r.get::<_,String>(2)?,"expires_at":r.get::<_,i64>(3)?,"revision_id":r.get::<_,Option<String>>(4)?}))).optional()?.ok_or_else(||err(404,"not_found","Upload session not found"))
}
fn view(c: &Connection, cap: &str, p: Option<&Principal>) -> Result<String, AppError> {
    let row: Option<(String, String, String)> = c
        .query_row(
            "SELECT id,policy,state FROM shares WHERE capability=?",
            [cap],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .optional()?;
    let (sid, policy, state) = row.ok_or_else(|| err(404, "not_found", "Share unavailable"))?;
    if state != "published" {
        return Err(err(404, "not_found", "Share unavailable"));
    }
    if policy == "entra-gated" {
        auth(p.ok_or_else(|| {
            err(
                401,
                "login_required",
                "Admitted authenticated viewer required",
            )
        })?)?;
    }
    Ok(sid)
}
fn valid_body(body: &str) -> Result<(), AppError> {
    if body.trim().is_empty() || body.len() > 20000 {
        Err(err(
            400,
            "invalid_comment",
            "Comment must contain 1–20000 bytes",
        ))
    } else {
        Ok(())
    }
}
fn actor(
    c: &Connection,
    p: Option<&Principal>,
    name: Option<&str>,
) -> Result<(String, String, bool), AppError> {
    if let Some(p) = p {
        auth(p)?;
        save_principal(c, p)?;
        return Ok((p.id.clone(), p.display_name.clone(), true));
    }
    let name = name.unwrap_or("").trim();
    if name.is_empty() || name.len() > 100 {
        return Err(err(
            400,
            "guest_name_required",
            "Provide an unverified guest name (1–100 bytes)",
        ));
    }
    Ok((format!("guest:{}", id()), name.into(), false))
}
fn thread_belongs(c: &Connection, sid: &str, tid: &str) -> Result<(), AppError> {
    let exists: bool = c.query_row(
        "SELECT EXISTS(SELECT 1 FROM threads WHERE id=? AND share=?)",
        params![tid, sid],
        |r| r.get(0),
    )?;
    if exists {
        Ok(())
    } else {
        Err(err(404, "not_found", "Thread not found"))
    }
}
fn post(
    c: &Connection,
    sid: &str,
    tid: &str,
    a: &(String, String, bool),
    p: Option<&Principal>,
    body: &str,
    at: i64,
) -> Result<String, AppError> {
    let comment_id = id();
    c.execute(
        "INSERT INTO comments VALUES(?,?,?,?,?,?,?)",
        params![comment_id, tid, a.0, a.1, a.2, body, at],
    )?;
    if let Some(p) = p {
        c.execute(
            "INSERT OR IGNORE INTO participants VALUES(?,?)",
            params![tid, p.id],
        )?;
    }
    let eid = id();
    c.execute(
        "INSERT INTO events VALUES(?,?,?,?,'comment',?)",
        params![eid, sid, tid, a.0, at],
    )?;
    let recipients = {
        let mut q=c.prepare("SELECT DISTINCT p.id,p.email FROM principals p WHERE (p.id=(SELECT owner FROM shares WHERE id=?) OR p.id IN (SELECT principal FROM participants WHERE thread=?))")?;

        q.query_map(params![sid, tid], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, Option<String>>(1)?))
        })?
        .collect::<Result<Vec<_>, _>>()?
    };
    for (r, email) in recipients {
        let old: Option<String> = c
            .query_row(
                "SELECT id FROM batches WHERE share=? AND recipient=? AND state='pending'",
                params![sid, r],
                |r| r.get(0),
            )
            .optional()?;
        let bid = old.unwrap_or_else(id);
        if email.as_deref().unwrap_or("").is_empty() {
            c.execute("INSERT INTO batches VALUES(?,?,?,'',?,'no_delivery',NULL,'No verified notification email') ON CONFLICT(id) DO UPDATE SET state='no_delivery',error='No verified notification email'",params![bid,sid,r,at+120])?;
            c.execute(
                "INSERT OR IGNORE INTO batch_events VALUES(?,?)",
                params![bid, eid],
            )?;
            continue;
        }
        c.execute("INSERT INTO batches VALUES(?,?,?,?,?,'pending',NULL,NULL) ON CONFLICT(id) DO UPDATE SET due=excluded.due,email=excluded.email",params![bid,sid,r,email,at+120])?;
        c.execute(
            "INSERT OR IGNORE INTO batch_events VALUES(?,?)",
            params![bid, eid],
        )?;
    }
    Ok(comment_id)
}
pub(crate) fn discussion_transition(state: &str, action: &str) -> Result<&'static str, AppError> {
    match (state, action) {
        ("active" | "archived", "archive") => Ok("archived"),
        ("active" | "archived", "restore") => Ok("active"),
        ("archived" | "deleted", "delete") => Ok("deleted"),
        (_, "archive" | "restore" | "delete") => Err(err(
            409,
            "discussion_state",
            "Only archived discussions can be deleted. Deleted discussions cannot be restored.",
        )),
        _ => Err(err(400, "invalid_action", "Unknown discussion action")),
    }
}
fn thread_state(c: &Connection, tid: &str) -> Result<String, AppError> {
    Ok(c.query_row(
        "SELECT state FROM thread_states WHERE thread=?",
        [tid],
        |r| r.get(0),
    )
    .optional()?
    .unwrap_or_else(|| "active".into()))
}
fn active_thread(c: &Connection, tid: &str) -> Result<(), AppError> {
    if thread_state(c, tid)? != "active" {
        return Err(err(
            409,
            "discussion_state",
            "Restore this discussion before replying or resolving it",
        ));
    }
    Ok(())
}
fn thread(c: &Connection, tid: &str) -> Result<Value, AppError> {
    let mut v=c.query_row("SELECT id,share,revision,anchor,resolved,created FROM threads WHERE id=?",[tid],|r|{let a:String=r.get(3)?;Ok(json!({"id":r.get::<_,String>(0)?,"share_id":r.get::<_,String>(1)?,"original_revision":r.get::<_,String>(2)?,"original_anchor":serde_json::from_str::<Value>(&a).unwrap_or(json!({})),"resolved":r.get::<_,bool>(4)?,"created_at":r.get::<_,i64>(5)?}))})?;
    let mut q=c.prepare("SELECT id,actor,name,verified,body,created FROM comments WHERE thread=? ORDER BY created,rowid")?;
    let comments=q.query_map([tid],|r|Ok(json!({"id":r.get::<_,String>(0)?,"actor_id":r.get::<_,String>(1)?,"display_name":r.get::<_,String>(2)?,"authenticated":r.get::<_,bool>(3)?,"body":r.get::<_,String>(4)?,"created_at":r.get::<_,i64>(5)?})))?.collect::<Result<Vec<_>,_>>()?;
    let mut q = c.prepare("SELECT revision,status,anchor,reason FROM placements WHERE thread=?")?;
    let placements=q.query_map([tid],|r|{let a:String=r.get(2)?;Ok(json!({"revision_id":r.get::<_,String>(0)?,"status":r.get::<_,String>(1)?,"anchor":serde_json::from_str::<Value>(&a).unwrap_or(json!({})),"reason":r.get::<_,String>(3)?,"algorithm":"conservative-v1"}))})?.collect::<Result<Vec<_>,_>>()?;
    v["state"] = json!(thread_state(c, tid)?);
    v["origin_deleted"] = json!(c.query_row(
        "SELECT EXISTS(SELECT 1 FROM deleted_revisions WHERE revision=?)",
        [&v["original_revision"].as_str().unwrap_or("")],
        |r| r.get::<_, bool>(0)
    )?);
    v["comments"] = json!(comments);
    v["placements"] = json!(placements);
    Ok(v)
}
fn thread_list(c: &Connection, sid: &str) -> Result<Value, AppError> {
    let mut q = c.prepare("SELECT id FROM threads WHERE share=? AND id NOT IN (SELECT thread FROM thread_states WHERE state='deleted') ORDER BY created,rowid")?;
    let ids = q
        .query_map([sid], |r| r.get::<_, String>(0))?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(json!(
        ids.iter()
            .map(|id| thread(c, id))
            .collect::<Result<Vec<_>, _>>()?
    ))
}
pub fn validate_files(files: &BTreeMap<String, Vec<u8>>, entrypoint: &str) -> Result<(), AppError> {
    if files.is_empty()
        || files.len() > 512
        || files.values().map(|v| v.len()).sum::<usize>() > 32 * 1024 * 1024
    {
        return Err(err(
            413,
            "quota_exceeded",
            "Local limit: 512 files and 32 MiB total",
        ));
    }
    let mut names = std::collections::HashSet::new();
    for path in files.keys() {
        if path.is_empty()
            || path.len() > 500
            || path.starts_with('/')
            || path.contains('\\')
            || path.contains(':')
            || path.chars().any(|c| c.is_control())
            || path
                .split('/')
                .any(|p| p == ".." || p == "." || p.is_empty())
            || !names.insert(path.to_lowercase())
        {
            return Err(err(
                400,
                "invalid_manifest",
                "Paths must be unique, relative, normalized, and traversal-free",
            ));
        }
    }
    if !files.contains_key(entrypoint) {
        return Err(err(400, "invalid_manifest", "Entrypoint is missing"));
    }
    Ok(())
}
fn normalize(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}
fn rematch(anchor: &Value, files: &BTreeMap<String, Vec<u8>>) -> (&'static str, &'static str) {
    let kind = anchor["kind"].as_str().unwrap_or("general");
    if kind == "general" {
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
        let count = text.matches(&format!("id=\"{}\"", stable)).count()
            + text.matches(&format!("id='{}'", stable)).count();
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

#[cfg(test)]
#[path = "core_tests.rs"]
mod core_tests;

pub fn validate_finalize_key(key: &str) -> Result<(), AppError> {
    if key.trim().is_empty() || key.len() > 200 || key.chars().any(char::is_control) {
        return Err(err(
            400,
            "invalid_idempotency_key",
            "A finalization idempotency key of 1–200 bytes is required",
        ));
    }
    Ok(())
}
fn share_version(c: &Connection, sid: &str) -> Result<i64, AppError> {
    Ok(c.query_row(
        "SELECT version FROM share_versions WHERE share=?",
        [sid],
        |r| r.get(0),
    )
    .optional()?
    .unwrap_or(0))
}
fn bump_share_version(c: &Connection, sid: &str) -> Result<(), AppError> {
    c.execute("INSERT INTO share_versions(share,version) VALUES(?,1) ON CONFLICT(share) DO UPDATE SET version=version+1", [sid])?;
    Ok(())
}

fn finalization_failure(c: &Connection, uid: &str) -> Result<Option<AppError>, AppError> {
    let raw: Option<Option<String>> = c
        .query_row(
            "SELECT error FROM finalize_attempts WHERE upload=?",
            [uid],
            |r| r.get(0),
        )
        .optional()?;
    raw.flatten()
        .map(|raw| {
            let v: Value = serde_json::from_str(&raw)
                .map_err(|_| err(500, "storage_error", "Invalid finalization failure receipt"))?;
            Ok(AppError {
                status: v["status"].as_u64().unwrap_or(500) as u16,
                code: v["code"].as_str().unwrap_or("preparation_failed").into(),
                message: v["message"].as_str().unwrap_or("Preparation failed").into(),
            })
        })
        .transpose()
}
fn check_finalize_claim(c: &Connection, uid: &str, claim: Option<&str>) -> Result<(), AppError> {
    let current: Option<(String, i64)> = c
        .query_row(
            "SELECT claim,lease_until FROM finalize_attempts WHERE upload=?",
            [uid],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    match (current, claim) {
        (None, None) => Ok(()),
        (Some((current, lease)), Some(claim)) if current == claim && lease > now() => Ok(()),
        _ => Err(err(
            409,
            "finalization_lease_lost",
            "Finalization is running in another attempt; retry the original request",
        )),
    }
}
impl Core {
    /// A bounded durable claim prevents overlapping source builds from publishing
    /// after another attempt has recorded a definitive failure or taken over.
    pub fn claim_finalize(&self, p: &Principal, uid: &str, key: &str) -> Result<Value, AppError> {
        let mut c = self.lock()?;
        let tx = c.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let u = upload(&tx, uid)?;
        let sid = u["share_id"].as_str().unwrap();
        owner(&tx, p, sid)?;
        writable(&tx, sid)?;
        let request:Option<(i64,Option<String>)>=tx.query_row("SELECT share_version,result FROM finalize_requests WHERE owner=? AND key=? AND upload=?",params![p.id,key,uid],|r|Ok((r.get(0)?,r.get(1)?))).optional()?;
        let (version, result) = request.ok_or_else(|| {
            err(
                409,
                "idempotency_conflict",
                "Finalization request does not match",
            )
        })?;
        if let Some(result) = result {
            return Ok(
                json!({"result":serde_json::from_str::<Value>(&result).map_err(|_|err(500,"storage_error","Invalid finalization receipt"))?}),
            );
        }
        if let Some(failure) = finalization_failure(&tx, uid)? {
            return Err(failure);
        }
        if version != share_version(&tx, sid)? {
            return Err(err(
                409,
                "stale_build",
                "Share changed after finalization began",
            ));
        }
        if u["expires_at"].as_i64().unwrap_or(0) < now() {
            return Err(err(410, "expired_upload", "Upload session expired"));
        }
        let lease: Option<i64> = tx
            .query_row(
                "SELECT lease_until FROM finalize_attempts WHERE upload=?",
                [uid],
                |r| r.get(0),
            )
            .optional()?;
        if lease.is_some_and(|lease| lease > now()) {
            return Err(err(
                409,
                "finalize_in_progress",
                "This finalization is still running; retry the original request",
            ));
        }
        let claim = id();
        let lease = now() + 300;
        tx.execute("INSERT INTO finalize_attempts(upload,claim,lease_until,error) VALUES(?,?,?,NULL) ON CONFLICT(upload) DO UPDATE SET claim=excluded.claim,lease_until=excluded.lease_until,error=NULL",params![uid,claim,lease])?;
        tx.commit()?;
        Ok(json!({"claim_id":claim,"lease_expires_at":lease,"result":null}))
    }
    /// Only the current live attempt can commit a terminal failure. Transient
    /// busy/unavailable outcomes release its claim without ending the request.
    pub fn record_finalize_failure(
        &self,
        p: &Principal,
        uid: &str,
        key: &str,
        claim: &str,
        failure: &AppError,
    ) -> Result<Value, AppError> {
        let mut c = self.lock()?;
        let tx = c.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let u = upload(&tx, uid)?;
        let sid = u["share_id"].as_str().unwrap();
        owner(&tx, p, sid)?;
        writable(&tx, sid)?;
        let request:Option<(i64,Option<String>)>=tx.query_row("SELECT share_version,result FROM finalize_requests WHERE owner=? AND key=? AND upload=?",params![p.id,key,uid],|r|Ok((r.get(0)?,r.get(1)?))).optional()?;
        let (version, result) = request.ok_or_else(|| {
            err(
                409,
                "idempotency_conflict",
                "Finalization request does not match",
            )
        })?;
        if let Some(result) = result {
            return serde_json::from_str(&result)
                .map_err(|_| err(500, "storage_error", "Invalid finalization receipt"));
        }
        if let Some(old) = finalization_failure(&tx, uid)? {
            return Err(old);
        }
        check_finalize_claim(&tx, uid, Some(claim))?;
        if version != share_version(&tx, sid)? {
            return Err(err(
                409,
                "stale_build",
                "Share changed after finalization began",
            ));
        }
        let terminal = (400..500).contains(&failure.status);
        let stored = terminal.then(|| serde_json::to_string(failure).unwrap());
        tx.execute(
            "UPDATE finalize_attempts SET lease_until=0,error=? WHERE upload=? AND claim=?",
            params![stored, uid, claim],
        )?;
        tx.commit()?;
        Err(failure.clone())
    }
}
