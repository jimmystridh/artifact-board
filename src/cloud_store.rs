//! Production storage primitives for the selected Cloud Run + Firestore + GCS architecture.
//! Used by FirestoreCore application dispatch. Core remains a local-test reference adapter.
//! Callers must apply authorization and domain invariants before using this repository.
//! Official REST contracts: https://docs.cloud.google.com/firestore/docs/reference/rest/v1/projects.databases.documents/commit
//! https://docs.cloud.google.com/firestore/docs/reference/rest/v1/Write
//! https://docs.cloud.google.com/storage/docs/json_api/v1/objects/insert
//! Runtime tokens come only from attached Cloud Run identity; no credentials are provisioned,
//! persisted, or logged. Explicit localhost emulators use a fixed test-only identity.
use crate::core::AppError;
use reqwest::{Client, Method};
use serde_json::{Value, json};
use std::time::Duration;
use url::Url;

#[derive(Clone, Copy, Debug)]
pub enum Collection {
    OAuthGrants,
    Principals,
    BuildJobs,
    Participants,
    BatchEvents,
    Shares,
    Uploads,
    Revisions,
    RevisionTombstones,
    Threads,
    Comments,
    Placements,
    Events,
    NotificationBatches,
    Idempotency,
}
impl Collection {
    fn name(self) -> &'static str {
        match self {
            Self::OAuthGrants => "oauth_grants",
            Self::Principals => "principals",
            Self::BuildJobs => "build_jobs",
            Self::Participants => "participants",
            Self::BatchEvents => "batch_events",
            Self::Shares => "shares",
            Self::Uploads => "uploads",
            Self::Revisions => "revisions",
            Self::RevisionTombstones => "revision_tombstones",
            Self::Threads => "threads",
            Self::Comments => "comments",
            Self::Placements => "placements",
            Self::Events => "events",
            Self::NotificationBatches => "notification_batches",
            Self::Idempotency => "idempotency",
        }
    }
    fn immutable(self) -> bool {
        matches!(
            self,
            Self::BatchEvents
                | Self::Revisions
                | Self::RevisionTombstones
                | Self::Comments
                | Self::Placements
                | Self::Events
                | Self::Idempotency
        )
    }
}
#[derive(Clone, Debug)]
pub struct Document {
    pub data: Value,
    pub update_time: String,
}
#[derive(Clone, Debug)]
pub enum Precondition {
    Create,
    UpdateTime(String),
}
#[derive(Clone, Debug)]
pub struct Mutation {
    pub collection: Collection,
    pub id: String,
    pub data: Value,
    pub condition: Precondition,
}
#[derive(Clone, Debug)]
pub struct BlobRef {
    pub name: String,
    pub generation: String,
}
#[derive(Clone)]
pub struct CloudStore {
    is_emulator: bool,
    client: Client,
    firestore: String,
    storage: String,
    project: String,
    database: String,
    bucket: String,
}
fn error(status: u16, code: &str, message: &str) -> AppError {
    AppError {
        status,
        code: code.into(),
        message: message.into(),
    }
}
fn segment(s: &str) -> Result<(), AppError> {
    if s.is_empty()
        || s.len() > 200
        || !s
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "-_.()".contains(c))
        || s == "."
        || s == ".."
    {
        Err(error(
            400,
            "invalid_storage_id",
            "Storage identifiers must be bounded single path segments",
        ))
    } else {
        Ok(())
    }
}
fn wire(v: &Value) -> Value {
    match v {
        Value::Null => json!({"nullValue":null}),
        Value::Bool(b) => json!({"booleanValue":b}),
        Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                json!({"integerValue":i.to_string()})
            } else {
                json!({"doubleValue":n})
            }
        }
        Value::String(s) => json!({"stringValue":s}),
        Value::Array(a) => json!({"arrayValue":{"values":a.iter().map(wire).collect::<Vec<_>>()}}),
        Value::Object(o) => {
            json!({"mapValue":{"fields":o.iter().map(|(k,v)|(k.clone(),wire(v))).collect::<serde_json::Map<_,_>>()}})
        }
    }
}
fn unwire(v: &Value) -> Result<Value, AppError> {
    if v.get("nullValue").is_some() {
        return Ok(Value::Null);
    }
    if let Some(b) = v.get("booleanValue") {
        return Ok(b.clone());
    }
    if let Some(s) = v.get("stringValue") {
        return Ok(s.clone());
    }
    if let Some(n) = v.get("doubleValue") {
        return Ok(n.clone());
    }
    if let Some(i) = v["integerValue"].as_str() {
        return i
            .parse::<i64>()
            .map(|i| json!(i))
            .map_err(|_| error(502, "invalid_storage_response", "Invalid integer"));
    }
    if let Some(a) = v.get("arrayValue") {
        return Ok(json!(
            a["values"]
                .as_array()
                .unwrap_or(&vec![])
                .iter()
                .map(unwire)
                .collect::<Result<Vec<_>, _>>()?
        ));
    }
    if let Some(m) = v.get("mapValue") {
        let mut o = serde_json::Map::new();
        if let Some(fields) = m["fields"].as_object() {
            for (k, v) in fields {
                o.insert(k.clone(), unwire(v)?);
            }
        }
        return Ok(Value::Object(o));
    }
    Err(error(
        502,
        "invalid_storage_response",
        "Unsupported Firestore value",
    ))
}
impl CloudStore {
    pub fn new(project: &str, database: &str, bucket: &str) -> Result<Self, AppError> {
        segment(project)?;
        segment(database)?;
        segment(bucket)?;
        Ok(Self {
            is_emulator: false,
            client: Client::builder()
                .timeout(Duration::from_secs(30))
                .redirect(reqwest::redirect::Policy::none())
                .build()
                .map_err(|_| error(500, "http_client", "Cannot initialize storage client"))?,
            firestore: "https://firestore.googleapis.com".into(),
            storage: "https://storage.googleapis.com".into(),
            project: project.into(),
            database: database.into(),
            bucket: bucket.into(),
        })
    }
    /// Explicit local-emulator mode. Never forwards runtime identity to arbitrary endpoints.
    pub fn emulator(
        project: &str,
        database: &str,
        bucket: &str,
        firestore_url: &str,
        storage_url: &str,
    ) -> Result<Self, AppError> {
        let mut result = Self::new(project, database, bucket)?;
        for endpoint in [firestore_url, storage_url] {
            let u = Url::parse(endpoint)
                .map_err(|_| error(400, "invalid_emulator", "Invalid emulator endpoint"))?;
            if u.scheme() != "http"
                || !matches!(u.host_str(), Some("127.0.0.1" | "localhost" | "[::1]"))
                || u.path() != "/"
                || u.query().is_some()
                || u.fragment().is_some()
                || !u.username().is_empty()
                || u.password().is_some()
            {
                return Err(error(
                    400,
                    "invalid_emulator",
                    "Only explicit localhost HTTP emulator origins allowed",
                ));
            }
        }
        result.is_emulator = true;
        result.firestore = firestore_url.trim_end_matches('/').into();
        result.storage = storage_url.trim_end_matches('/').into();
        Ok(result)
    }
    /// Equality query with deterministic document-id pagination, including inside a transaction.
    /// Refuses more than 10,000 records rather than silently returning incomplete activity.
    pub async fn query(
        &self,
        collection: Collection,
        field: &str,
        equals: &Value,
        transaction: Option<&str>,
        token: &str,
    ) -> Result<Vec<Document>, AppError> {
        if field.is_empty() || !field.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
            return Err(error(
                400,
                "invalid_query",
                "Use a simple domain field name",
            ));
        }
        let mut result = Vec::new();
        let mut cursor: Option<String> = None;
        loop {
            let mut q = json!({"from":[{"collectionId":collection.name()}],"where":{"fieldFilter":{"field":{"fieldPath":field},"op":"EQUAL","value":wire(equals)}},"orderBy":[{"field":{"fieldPath":"__name__"},"direction":"ASCENDING"}],"limit":200});
            if let Some(ref cursor) = cursor {
                q["startAt"] = json!({"values":[{"referenceValue":cursor}],"before":false});
            }
            let mut body = json!({"structuredQuery":q});
            if let Some(t) = transaction {
                body["transaction"] = json!(t)
            }
            let response = self
                .json(Method::POST, self.endpoint(":runQuery"), token, Some(body))
                .await?;
            let entries = response.as_array().ok_or_else(|| {
                error(
                    502,
                    "invalid_storage_response",
                    "Query response must be an array",
                )
            })?;
            let mut count = 0;
            for row in entries {
                if let Some(v) = row.get("document") {
                    let data = unwire(&json!({"mapValue":{"fields":v["fields"]}}))?;
                    let update_time = v["updateTime"]
                        .as_str()
                        .ok_or_else(|| {
                            error(502, "invalid_storage_response", "Query version missing")
                        })?
                        .to_string();
                    cursor = Some(
                        v["name"]
                            .as_str()
                            .ok_or_else(|| {
                                error(502, "invalid_storage_response", "Document name missing")
                            })?
                            .to_string(),
                    );
                    result.push(Document { data, update_time });
                    count += 1;
                }
            }
            if count < 200 {
                return Ok(result);
            }
            if result.len() >= 10000 {
                return Err(error(
                    413,
                    "query_limit",
                    "Query exceeds prototype limit; add domain pagination",
                ));
            }
        }
    }
    /// Cloud Run attached service-account identity. No environment secret fallback.
    /// This method is only invoked by the running application, never during setup.
    pub async fn runtime_token(&self) -> Result<String, AppError> {
        if self.is_emulator {
            return Ok("owner".into());
        }
        let response=self.client.get("http://metadata.google.internal/computeMetadata/v1/instance/service-accounts/default/token")
            .header("Metadata-Flavor","Google").timeout(Duration::from_secs(3)).send().await
            .map_err(|_|error(503,"runtime_identity_unavailable","Cloud Run metadata identity unavailable"))?;
        if !response.status().is_success()
            || response
                .headers()
                .get("Metadata-Flavor")
                .and_then(|v| v.to_str().ok())
                != Some("Google")
        {
            return Err(error(
                503,
                "runtime_identity_unavailable",
                "Metadata identity response rejected",
            ));
        }
        let value: Value = response.json().await.map_err(|_| {
            error(
                503,
                "runtime_identity_unavailable",
                "Invalid metadata identity response",
            )
        })?;
        if value["token_type"].as_str() != Some("Bearer") {
            return Err(error(
                503,
                "runtime_identity_unavailable",
                "Unexpected runtime token type",
            ));
        }
        value["access_token"]
            .as_str()
            .filter(|v| !v.is_empty())
            .map(str::to_owned)
            .ok_or_else(|| {
                error(
                    503,
                    "runtime_identity_unavailable",
                    "Runtime access token missing",
                )
            })
    }
    fn root(&self) -> String {
        format!("projects/{}/databases/{}", self.project, self.database)
    }
    fn document_name(&self, c: Collection, id: &str) -> Result<String, AppError> {
        segment(id)?;
        Ok(format!("{}/documents/{}/{}", self.root(), c.name(), id))
    }
    fn endpoint(&self, suffix: &str) -> String {
        format!("{}/v1/{}/documents{}", self.firestore, self.root(), suffix)
    }
    async fn json(
        &self,
        method: Method,
        url: String,
        token: &str,
        body: Option<Value>,
    ) -> Result<Value, AppError> {
        if token.is_empty() {
            return Err(error(
                401,
                "missing_cloud_identity",
                "Runtime service identity required",
            ));
        }
        let mut req = self.client.request(method, url).bearer_auth(token);
        if let Some(body) = body {
            req = req.json(&body)
        }
        let response = req.send().await.map_err(|_| {
            error(
                503,
                "storage_unavailable",
                "Storage request failed; outcome may be uncertain",
            )
        })?;
        let status = response.status();
        if !status.is_success() {
            let code = response
                .json::<Value>()
                .await
                .ok()
                .and_then(|v| v["error"]["status"].as_str().map(str::to_owned));
            if matches!(
                code.as_deref(),
                Some("ABORTED" | "FAILED_PRECONDITION" | "ALREADY_EXISTS")
            ) {
                return Err(error(
                    409,
                    "storage_conflict",
                    "Transaction changed; reread and revalidate before retry",
                ));
            }
            return Err(storage_error(status.as_u16()));
        }
        response
            .json()
            .await
            .map_err(|_| error(502, "invalid_storage_response", "Invalid storage JSON"))
    }
    pub async fn begin_transaction(&self, token: &str) -> Result<String, AppError> {
        let v = self
            .json(
                Method::POST,
                self.endpoint(":beginTransaction"),
                token,
                Some(json!({"options":{"readWrite":{}}})),
            )
            .await?;
        v["transaction"].as_str().map(str::to_owned).ok_or_else(|| {
            error(
                502,
                "invalid_storage_response",
                "Transaction identifier missing",
            )
        })
    }
    pub async fn get(
        &self,
        c: Collection,
        id: &str,
        transaction: Option<&str>,
        token: &str,
    ) -> Result<Option<Document>, AppError> {
        let name = self.document_name(c, id)?;
        let v = if let Some(t) = transaction {
            // POST avoids emulator's unsupported bytes-valued GET query parameter.
            // This is also the official production batchGet transaction contract.
            let rows = self
                .json(
                    Method::POST,
                    self.endpoint(":batchGet"),
                    token,
                    Some(json!({"documents":[name],"transaction":t})),
                )
                .await?;
            let rows = rows.as_array().ok_or_else(|| {
                error(
                    502,
                    "invalid_storage_response",
                    "Batch read response must be an array",
                )
            })?;
            if rows.iter().any(|row| row["missing"] == name) {
                return Ok(None);
            }
            rows.iter()
                .find_map(|row| row.get("found"))
                .cloned()
                .ok_or_else(|| {
                    error(502, "invalid_storage_response", "Batch read result missing")
                })?
        } else {
            let url = format!("{}/v1/{}", self.firestore, name);
            match self.json(Method::GET, url, token, None).await {
                Ok(v) => v,
                Err(e) if e.status == 404 => return Ok(None),
                Err(e) => return Err(e),
            }
        };
        let data = unwire(&json!({"mapValue":{"fields":v["fields"]}}))?;
        let update_time = v["updateTime"]
            .as_str()
            .ok_or_else(|| error(502, "invalid_storage_response", "Version token missing"))?
            .to_string();
        Ok(Some(Document { data, update_time }))
    }
    fn commit_body(
        &self,
        mutations: &[Mutation],
        transaction: Option<&str>,
    ) -> Result<Value, AppError> {
        if mutations.is_empty() || mutations.len() > 450 {
            return Err(error(
                400,
                "invalid_transaction",
                "Use 1–450 bounded writes",
            ));
        }
        let mut writes = vec![];
        for m in mutations {
            if !m.data.is_object() || m.data.to_string().len() > 700_000 {
                return Err(error(
                    413,
                    "record_too_large",
                    "Metadata records must be bounded objects; artifacts belong in GCS",
                ));
            }
            let name = self.document_name(m.collection, &m.id)?;
            let condition = match &m.condition {
                Precondition::Create => json!({"exists":false}),
                Precondition::UpdateTime(t) => {
                    if m.collection.immutable() {
                        return Err(error(
                            409,
                            "immutable_record",
                            "Append-only collections cannot be overwritten",
                        ));
                    }
                    if t.is_empty() {
                        return Err(error(400, "version_required", "CAS update time required"));
                    }
                    json!({"updateTime":t})
                }
            };
            writes.push(json!({"update":{"name":name,"fields":wire(&m.data)["mapValue"]["fields"]},"currentDocument":condition}));
        }
        let mut body = json!({"writes":writes});
        if let Some(t) = transaction {
            body["transaction"] = json!(t)
        }
        Ok(body)
    }
    /// Atomic Firestore commit. ABORTED/FAILED_PRECONDITION requires reread + domain revalidation,
    /// never a blind retry of a stale share publication or notification claim.
    pub async fn commit(
        &self,
        mutations: &[Mutation],
        transaction: Option<&str>,
        token: &str,
    ) -> Result<Value, AppError> {
        let body = self.commit_body(mutations, transaction)?;
        self.json(Method::POST, self.endpoint(":commit"), token, Some(body))
            .await
    }
    pub async fn rollback(&self, transaction: &str, token: &str) -> Result<(), AppError> {
        self.json(
            Method::POST,
            self.endpoint(":rollback"),
            token,
            Some(json!({"transaction":transaction})),
        )
        .await?;
        Ok(())
    }
    /// Private immutable object upload; a collision is a conflict, never an overwrite.
    /// Upload all revision objects first, then atomically CAS the Firestore share/latest pointer.
    /// A failed metadata commit may leave an unreferenced object, never a partial visible revision.
    pub async fn put_blob(
        &self,
        name: &str,
        bytes: Vec<u8>,
        token: &str,
    ) -> Result<BlobRef, AppError> {
        valid_blob(name)?;
        if bytes.len() > 32 * 1024 * 1024 {
            return Err(error(
                413,
                "blob_too_large",
                "Local prototype upload bound is 32 MiB",
            ));
        }
        if token.is_empty() {
            return Err(error(
                401,
                "missing_cloud_identity",
                "Runtime service identity required",
            ));
        }
        let mut u = Url::parse(&format!(
            "{}/upload/storage/v1/b/{}/o",
            self.storage, self.bucket
        ))
        .unwrap();
        u.query_pairs_mut()
            .append_pair("uploadType", "media")
            .append_pair("name", name)
            .append_pair("ifGenerationMatch", "0");
        let res = self
            .client
            .post(u)
            .bearer_auth(token)
            .header("content-type", "application/octet-stream")
            .body(bytes)
            .send()
            .await
            .map_err(|_| {
                error(
                    503,
                    "storage_unavailable",
                    "Upload outcome uncertain; inspect object before retry",
                )
            })?;
        if !res.status().is_success() {
            return Err(storage_error(res.status().as_u16()));
        }
        let v: Value = res
            .json()
            .await
            .map_err(|_| error(502, "invalid_storage_response", "Invalid blob metadata"))?;
        let generation = v["generation"]
            .as_str()
            .filter(|g| g.chars().all(|c| c.is_ascii_digit()) && !g.is_empty())
            .ok_or_else(|| error(502, "invalid_storage_response", "Blob generation missing"))?
            .to_owned();
        Ok(BlobRef {
            name: name.into(),
            generation,
        })
    }
    /// No public/signed URL is issued: authorized transport proxies the pinned generation.
    pub async fn get_blob(&self, blob: &BlobRef, token: &str) -> Result<Vec<u8>, AppError> {
        valid_blob(&blob.name)?;
        if token.is_empty() {
            return Err(error(
                401,
                "missing_cloud_identity",
                "Runtime service identity required",
            ));
        }
        if blob.generation.is_empty() || !blob.generation.chars().all(|c| c.is_ascii_digit()) {
            return Err(error(
                400,
                "invalid_generation",
                "Pinned numeric generation required",
            ));
        }
        let mut u =
            Url::parse(&format!("{}/storage/v1/b/{}/o/", self.storage, self.bucket)).unwrap();
        u.path_segments_mut()
            .unwrap()
            .pop_if_empty()
            .push(&blob.name);
        u.query_pairs_mut()
            .append_pair("alt", "media")
            .append_pair("generation", &blob.generation);
        let mut res = self
            .client
            .get(u)
            .bearer_auth(token)
            .send()
            .await
            .map_err(|_| error(503, "storage_unavailable", "Artifact read failed"))?;
        if !res.status().is_success() {
            return Err(storage_error(res.status().as_u16()));
        }
        let mut out = Vec::new();
        while let Some(chunk) = res
            .chunk()
            .await
            .map_err(|_| error(503, "storage_unavailable", "Artifact stream interrupted"))?
        {
            if out.len() + chunk.len() > 32 * 1024 * 1024 {
                return Err(error(413, "blob_too_large", "Artifact exceeds read bound"));
            }
            out.extend_from_slice(&chunk);
        }
        Ok(out)
    }
}
fn storage_error(status: u16) -> AppError {
    match status {
        404 => error(404, "not_found", "Storage record not found"),
        409 | 412 => error(
            409,
            "storage_conflict",
            "Version changed or immutable object already exists; reread before retry",
        ),
        401 | 403 => error(
            503,
            "cloud_identity_denied",
            "Runtime identity lacks storage access",
        ),
        429 => error(
            503,
            "storage_throttled",
            "Storage throttled; use bounded backoff",
        ),
        _ => error(
            503,
            "storage_unavailable",
            "Storage failed; inspect operation before retry",
        ),
    }
}
fn valid_blob(name: &str) -> Result<(), AppError> {
    if name.is_empty()
        || name.len() > 700
        || name.starts_with('/')
        || name.contains('\\')
        || name.chars().any(|c| c.is_control())
        || name
            .split('/')
            .any(|s| s.is_empty() || s == ".." || s == ".")
    {
        return Err(error(
            400,
            "invalid_blob_name",
            "Blob key must be a normalized relative path",
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    #[ignore = "Requires explicitly started localhost Firestore emulator"]
    async fn firestore_emulator_transaction_contract() {
        let host = std::env::var("ARTIFACT_TEST_FIRESTORE_URL")
            .expect("Explicit localhost emulator URL required");
        let store = CloudStore::emulator(
            "demo-artifact-board",
            "(default)",
            "fixture-bucket",
            &host,
            &host,
        )
        .unwrap();
        let token = store.runtime_token().await.unwrap();
        let id = uuid::Uuid::new_v4().to_string();
        let create = Mutation {
            collection: Collection::Shares,
            id: id.clone(),
            data: json!({"id":id,"owner_id":"contract-test","state":"pending"}),
            condition: Precondition::Create,
        };
        store
            .commit(std::slice::from_ref(&create), None, &token)
            .await
            .unwrap();
        assert_eq!(
            store
                .commit(&[create], None, &token)
                .await
                .unwrap_err()
                .status,
            409
        );
        let first = store
            .get(Collection::Shares, &id, None, &token)
            .await
            .unwrap()
            .unwrap();
        let tx = store.begin_transaction(&token).await.unwrap();
        let read = store
            .get(Collection::Shares, &id, Some(&tx), &token)
            .await
            .unwrap()
            .unwrap();
        let update = Mutation {
            collection: Collection::Shares,
            id: id.clone(),
            data: json!({"id":id,"owner_id":"contract-test","state":"published"}),
            condition: Precondition::UpdateTime(read.update_time),
        };
        store.commit(&[update], Some(&tx), &token).await.unwrap();
        assert_eq!(
            store
                .get(Collection::Shares, &id, None, &token)
                .await
                .unwrap()
                .unwrap()
                .data["state"],
            "published"
        );
        let stale = Mutation {
            collection: Collection::Shares,
            id: id.clone(),
            data: json!({"state":"bad"}),
            condition: Precondition::UpdateTime(first.update_time),
        };
        assert_eq!(
            store
                .commit(&[stale], None, &token)
                .await
                .unwrap_err()
                .status,
            409
        );
        let found = store
            .query(
                Collection::Shares,
                "owner_id",
                &json!("contract-test"),
                None,
                &token,
            )
            .await
            .unwrap();
        assert!(found.iter().any(|d| d.data["id"] == id));
        let tx = store.begin_transaction(&token).await.unwrap();
        store.rollback(&tx, &token).await.unwrap();
        let owner = uuid::Uuid::new_v4().to_string();
        let pages = (0..205)
            .map(|n| Mutation {
                collection: Collection::Shares,
                id: format!("{owner}-{n:03}"),
                data: json!({"id":format!("{owner}-{n:03}"),"owner_id":owner}),
                condition: Precondition::Create,
            })
            .collect::<Vec<_>>();
        store.commit(&pages, None, &token).await.unwrap();
        let rows = store
            .query(Collection::Shares, "owner_id", &json!(owner), None, &token)
            .await
            .unwrap();
        assert_eq!(
            rows.len(),
            205,
            "Document-name cursor must cross the 200-record page boundary"
        );
    }
    #[tokio::test]
    async fn emulator_runtime_identity_is_local_only() {
        let store = CloudStore::emulator(
            "project",
            "(default)",
            "bucket",
            "http://127.0.0.1:1",
            "http://localhost:2",
        )
        .unwrap();
        assert_eq!(store.runtime_token().await.unwrap(), "owner");
        assert!(
            CloudStore::emulator(
                "project",
                "(default)",
                "bucket",
                "http://attacker.example",
                "http://localhost:2"
            )
            .is_err()
        );
        assert!(
            !CloudStore::new("project", "(default)", "bucket")
                .unwrap()
                .is_emulator
        );
    }
    #[test]
    fn firestore_values_roundtrip() {
        let v = json!({"name":"banana","n":5,"flag":true,"nested":{"arr":[null,"x",1.5]}});
        assert_eq!(unwire(&wire(&v)).unwrap(), v);
    }
    #[test]
    fn safe_cas_and_immutable_writes() {
        let c = CloudStore::new("test-project", "(default)", "test-bucket").unwrap();
        let m = Mutation {
            collection: Collection::Shares,
            id: "share-1".into(),
            data: json!({"latest":"rev-2"}),
            condition: Precondition::UpdateTime("2026-10-18T00:00:00Z".into()),
        };
        let b = c
            .commit_body(std::slice::from_ref(&m), Some("txn"))
            .unwrap();
        assert_eq!(
            b["writes"][0]["currentDocument"]["updateTime"],
            "2026-10-18T00:00:00Z"
        );
        assert_eq!(b["transaction"], "txn");
        let mut immutable = m.clone();
        immutable.collection = Collection::Revisions;
        assert!(c.commit_body(&[immutable], None).is_err());
        let mut invalid = m;
        invalid.id = "../outside".into();
        assert!(c.commit_body(&[invalid], None).is_err());
    }
    #[tokio::test]
    async fn fake_http_checks_atomic_commit_and_gcs_precondition() {
        use axum::{
            Router,
            body::to_bytes,
            extract::{Request, State},
            response::IntoResponse,
            routing::any,
        };
        use std::sync::{Arc, Mutex};
        type SeenRequests = Arc<Mutex<Vec<(String, Value)>>>;
        let seen = Arc::new(Mutex::new(Vec::<(String, Value)>::new()));
        async fn fake(State(seen): State<SeenRequests>, req: Request) -> impl IntoResponse {
            let path = req.uri().to_string();
            assert_eq!(req.headers()["authorization"], "Bearer fixture-only");
            let body = to_bytes(req.into_body(), 1024 * 1024).await.unwrap();
            seen.lock().unwrap().push((
                path.clone(),
                serde_json::from_slice(&body).unwrap_or(Value::Null),
            ));
            if path.contains("upload/storage") {
                axum::Json(json!({"generation":"123"}))
            } else {
                axum::Json(json!({"commitTime":"2026-10-18T00:00:00Z"}))
            }
        }
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let task = tokio::spawn(
            axum::serve(
                listener,
                Router::new().fallback(any(fake)).with_state(seen.clone()),
            )
            .into_future(),
        );
        let mut c = CloudStore::new("project", "(default)", "bucket").unwrap();
        c.firestore = format!("http://{addr}");
        c.storage = c.firestore.clone();
        let m = Mutation {
            collection: Collection::Comments,
            id: "comment-1".into(),
            data: json!({"body":"hello"}),
            condition: Precondition::Create,
        };
        c.commit(&[m], None, "fixture-only").await.unwrap();
        let blob = c
            .put_blob(
                "shares/s/revisions/r/index.html",
                b"hello".to_vec(),
                "fixture-only",
            )
            .await
            .unwrap();
        assert_eq!(blob.generation, "123");
        let requests = seen.lock().unwrap();
        assert!(requests[0].0.contains("documents:commit"));
        assert_eq!(
            requests[0].1["writes"][0]["currentDocument"]["exists"],
            false
        );
        assert!(requests[1].0.contains("ifGenerationMatch=0"));
        task.abort();
    }
}
