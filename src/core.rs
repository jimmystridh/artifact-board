//! LOCAL TEST ADAPTER ONLY: production persistence is Firestore + GCS.
//! This SQLite implementation is not a production Cloud Run repository.
//! Local prototype policies: authenticated principals are transport-validated;
//! archive/unpublish/delete deny all viewers, restore returns to unpublished.
//! No permanent purge or production identity/tenant policy is implemented.
use rusqlite::{Connection, OptionalExtension, params};
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
 CREATE TABLE IF NOT EXISTS revisions(id TEXT PRIMARY KEY,share TEXT NOT NULL REFERENCES shares(id),entrypoint TEXT NOT NULL,digest TEXT NOT NULL,manifest TEXT NOT NULL,created INTEGER NOT NULL);
 CREATE TABLE IF NOT EXISTS files(revision TEXT NOT NULL REFERENCES revisions(id),path TEXT NOT NULL,bytes BLOB NOT NULL,PRIMARY KEY(revision,path));
 CREATE TABLE IF NOT EXISTS threads(id TEXT PRIMARY KEY,share TEXT NOT NULL REFERENCES shares(id),revision TEXT NOT NULL REFERENCES revisions(id),anchor TEXT NOT NULL,resolved INTEGER NOT NULL DEFAULT 0,created INTEGER NOT NULL);
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
            "SELECT id,entrypoint,digest,created FROM revisions WHERE share=? ORDER BY created,id",
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
    pub fn publish(
        &self,
        p: &Principal,
        uid: &str,
        files: &BTreeMap<String, Vec<u8>>,
        entrypoint: &str,
    ) -> Result<Value, AppError> {
        auth(p)?;
        let mut c = self.lock()?;
        let tx = c.transaction()?;
        let u = upload(&tx, uid)?;
        let sid = u["share_id"].as_str().unwrap();
        owner(&tx, p, sid)?;
        writable(&tx, sid)?;
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
        let origins = {
            let mut q = tx.prepare("SELECT id,anchor FROM threads WHERE share=? AND resolved=0")?;

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
        tx.commit()?;
        Ok(out)
    }
    pub fn viewer_share(&self, capability: &str, p: Option<&Principal>) -> Result<Value, AppError> {
        let c = self.lock()?;
        let sid = view(&c, capability, p)?;
        let mut v = share(&c, &sid, false)?;
        let mut q = c.prepare(
            "SELECT id,entrypoint,created FROM revisions WHERE share=? ORDER BY created,rowid",
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
        c.query_row("SELECT id,entrypoint,digest,created FROM revisions WHERE id=? AND share=?",params![rid,sid],|r|Ok(json!({"id":r.get::<_,String>(0)?,"entrypoint":r.get::<_,String>(1)?,"digest":r.get::<_,String>(2)?,"created_at":r.get::<_,i64>(3)?}))).optional()?.ok_or_else(||err(404,"not_found","Revision not found"))
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
            "SELECT EXISTS(SELECT 1 FROM revisions WHERE id=? AND share=?)",
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
        valid_body(body)?;
        if anchor.to_string().len() > 16384 || !anchor.is_object() {
            return Err(err(
                400,
                "invalid_anchor",
                "Anchor must be a bounded JSON object",
            ));
        }
        let mut c = self.lock()?;
        let tx = c.transaction()?;
        let sid = view(&tx, capability, p)?;
        let valid: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM revisions WHERE id=? AND share=?)",
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
        post(&tx, &sid, &tid, &actor, p, body, now())?;
        let result = thread(&tx, &tid)?;
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
        valid_body(body)?;
        let mut c = self.lock()?;
        let tx = c.transaction()?;
        let sid = view(&tx, capability, p)?;
        thread_belongs(&tx, &sid, tid)?;
        let a = actor(&tx, p, guest_name)?;
        post(&tx, &sid, tid, &a, p, body, now())?;
        let result = thread(&tx, tid)?;
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
        c.execute(
            "UPDATE threads SET resolved=? WHERE id=?",
            params![resolved, tid],
        )?;
        thread(&c, tid)
    }
    pub fn lifecycle(&self, p: &Principal, sid: &str, action: &str) -> Result<Value, AppError> {
        let mut c = self.lock()?;
        let tx = c.transaction()?;
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
) -> Result<(), AppError> {
    c.execute(
        "INSERT INTO comments VALUES(?,?,?,?,?,?,?)",
        params![id(), tid, a.0, a.1, a.2, body, at],
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
    Ok(())
}
fn thread(c: &Connection, tid: &str) -> Result<Value, AppError> {
    let mut v=c.query_row("SELECT id,share,revision,anchor,resolved,created FROM threads WHERE id=?",[tid],|r|{let a:String=r.get(3)?;Ok(json!({"id":r.get::<_,String>(0)?,"share_id":r.get::<_,String>(1)?,"original_revision":r.get::<_,String>(2)?,"original_anchor":serde_json::from_str::<Value>(&a).unwrap_or(json!({})),"resolved":r.get::<_,bool>(4)?,"created_at":r.get::<_,i64>(5)?}))})?;
    let mut q=c.prepare("SELECT id,actor,name,verified,body,created FROM comments WHERE thread=? ORDER BY created,rowid")?;
    let comments=q.query_map([tid],|r|Ok(json!({"id":r.get::<_,String>(0)?,"actor_id":r.get::<_,String>(1)?,"display_name":r.get::<_,String>(2)?,"authenticated":r.get::<_,bool>(3)?,"body":r.get::<_,String>(4)?,"created_at":r.get::<_,i64>(5)?})))?.collect::<Result<Vec<_>,_>>()?;
    let mut q = c.prepare("SELECT revision,status,anchor,reason FROM placements WHERE thread=?")?;
    let placements=q.query_map([tid],|r|{let a:String=r.get(2)?;Ok(json!({"revision_id":r.get::<_,String>(0)?,"status":r.get::<_,String>(1)?,"anchor":serde_json::from_str::<Value>(&a).unwrap_or(json!({})),"reason":r.get::<_,String>(3)?,"algorithm":"conservative-v1"}))})?.collect::<Result<Vec<_>,_>>()?;
    v["comments"] = json!(comments);
    v["placements"] = json!(placements);
    Ok(v)
}
fn thread_list(c: &Connection, sid: &str) -> Result<Value, AppError> {
    let mut q = c.prepare("SELECT id FROM threads WHERE share=? ORDER BY created,rowid")?;
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
