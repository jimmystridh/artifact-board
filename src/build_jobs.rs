//! Durable Firestore build coordination and bounded Cloud Run Jobs REST dispatch.
//! No credentials are provisioned; runtime identity is kept outside scrubbed build subprocesses.
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CloudRunJobConfig {
    pub project: String,
    pub region: String,
    pub job: String,
    pub container: String,
    pub timeout_seconds: u32,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BuildInput {
    pub job_id: String,
    pub share_id: String,
    pub input_bucket: String,
    pub input_object: String,
    pub input_generation: String,
    pub output_bucket: String,
    pub output_prefix: String,
    pub expected_latest_revision: Option<String>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunRequest {
    pub method: String,
    pub url: String,
    pub body: Value,
}
fn identifier(value: &str, max: usize) -> bool {
    !value.is_empty()
        && value.len() <= max
        && value
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || c == b'-' || c == b'_')
}
fn dns_label(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 63
        && value.as_bytes()[0].is_ascii_lowercase()
        && value.as_bytes().last().unwrap().is_ascii_alphanumeric()
        && value
            .bytes()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'-')
}
fn bucket(value: &str) -> bool {
    value.len() >= 3
        && value.len() <= 63
        && value.bytes().all(|c| {
            c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, b'-' | b'_' | b'.')
        })
        && value.as_bytes()[0].is_ascii_alphanumeric()
        && value.as_bytes().last().unwrap().is_ascii_alphanumeric()
}
fn object(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 1024
        && !value.starts_with('/')
        && !value.contains(['\\', '?', '#', '%'])
        && !value.chars().any(char::is_control)
        && value
            .trim_end_matches('/')
            .split('/')
            .all(|p| !p.is_empty() && p != "." && p != "..")
}
impl CloudRunJobConfig {
    /// Construct a Google Cloud Run v2 jobs.run request. Does not send it.
    /// Requires pre-provisioned reviewed Job template and run.jobs.runWithOverrides permission.
    pub fn run_request(&self, input: &BuildInput) -> Result<RunRequest, String> {
        if !identifier(&self.project, 63)
            || !dns_label(&self.region)
            || !dns_label(&self.job)
            || !dns_label(&self.container)
        {
            return Err("Invalid operator Cloud Run resource identifier".into());
        }
        if !(1..=120).contains(&self.timeout_seconds) {
            return Err("Local proposed Cloud Run timeout must be 1..120 seconds".into());
        }
        if !identifier(&input.job_id, 128)
            || !identifier(&input.share_id, 128)
            || input
                .expected_latest_revision
                .as_deref()
                .is_some_and(|v| !identifier(v, 128))
        {
            return Err("Invalid build/share/revision identifier".into());
        }
        if !bucket(&input.input_bucket)
            || !bucket(&input.output_bucket)
            || !object(&input.input_object)
            || !object(&input.output_prefix)
            || input.input_generation.is_empty()
            || !input.input_generation.bytes().all(|c| c.is_ascii_digit())
        {
            return Err(
                "Build inputs require generation-pinned GCS objects and safe output paths".into(),
            );
        }
        let expected_prefix = format!("builds/{}/", input.job_id);
        if !input.output_prefix.starts_with(&expected_prefix) {
            return Err("Output prefix must be scoped to builds/{job_id}/".into());
        }
        let env = [
            ("ARTIFACT_JOB_ID", input.job_id.as_str()),
            ("ARTIFACT_SHARE_ID", input.share_id.as_str()),
            ("ARTIFACT_INPUT_BUCKET", input.input_bucket.as_str()),
            ("ARTIFACT_INPUT_OBJECT", input.input_object.as_str()),
            ("ARTIFACT_INPUT_GENERATION", input.input_generation.as_str()),
            ("ARTIFACT_OUTPUT_BUCKET", input.output_bucket.as_str()),
            ("ARTIFACT_OUTPUT_PREFIX", input.output_prefix.as_str()),
            (
                "ARTIFACT_EXPECTED_LATEST_REVISION",
                input.expected_latest_revision.as_deref().unwrap_or(""),
            ),
        ]
        .into_iter()
        .map(|(name, value)| json!({"name":name,"value":value}))
        .collect::<Vec<_>>();
        Ok(RunRequest {
            method: "POST".into(),
            url: format!(
                "https://run.googleapis.com/v2/projects/{}/locations/{}/jobs/{}:run",
                self.project, self.region, self.job
            ),
            body: json!({"overrides":{"taskCount":1,"timeout":format!("{}s",self.timeout_seconds),"containerOverrides":[{"name":self.container,"clearArgs":true,"env":env}]}}),
        })
    }
}

use crate::{
    cloud_store::{Collection as C, Document, Mutation, Precondition},
    core::{AppError, Principal},
    firestore_core::FirestoreCore,
};
use sha2::{Digest, Sha256};
fn failure(status: u16, code: &str, message: &str) -> AppError {
    AppError {
        status,
        code: code.into(),
        message: message.into(),
    }
}
fn timestamp() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}
fn bounded(message: &str) -> String {
    message
        .chars()
        .filter(|c| !c.is_control() || *c == '\n' || *c == '\t')
        .take(16_384)
        .collect()
}
fn visible(mut job: Value) -> Value {
    if let Some(map) = job.as_object_mut() {
        for key in ["claim_id", "dispatch_claim", "source_manifest_sha256"] {
            map.remove(key);
        }
    }
    job
}
fn valid_entry(entry: &str) -> bool {
    !entry.is_empty()
        && entry.len() <= 1024
        && !entry.starts_with('/')
        && !entry.contains(['\\', ':', '%', '?', '#'])
        && !entry.chars().any(char::is_control)
        && entry
            .split('/')
            .all(|p| !p.is_empty() && p != "." && p != "..")
}
async fn load(core: &FirestoreCore, id: &str, token: &str) -> Result<Document, AppError> {
    core.store
        .get(C::BuildJobs, id, None, token)
        .await?
        .ok_or_else(|| failure(404, "build_not_found", "Build job not found"))
}
async fn save(
    core: &FirestoreCore,
    id: &str,
    old: &Document,
    data: Value,
    token: &str,
) -> Result<(), AppError> {
    core.store
        .commit(
            &[Mutation {
                collection: C::BuildJobs,
                id: id.into(),
                data,
                condition: Precondition::UpdateTime(old.update_time.clone()),
            }],
            None,
            token,
        )
        .await?;
    Ok(())
}

/// Atomically freezes a staged upload and creates one durable job per upload.
/// Does not start a process or create a cloud execution.
pub async fn enqueue(
    core: &FirestoreCore,
    principal: &Principal,
    upload_id: &str,
    entrypoint: &str,
) -> Result<Value, AppError> {
    enqueue_inner(core, principal, upload_id, entrypoint, None).await
}

/// Freeze acceptance and its original receipt atomically with the durable job.
pub async fn enqueue_finalized(
    core: &FirestoreCore,
    principal: &Principal,
    upload_id: &str,
    entrypoint: &str,
    key: &str,
) -> Result<Value, AppError> {
    crate::core::validate_finalize_key(key)?;
    let key = crate::firestore_core::finalize_key(principal, key);
    enqueue_inner(core, principal, upload_id, entrypoint, Some(&key)).await
}

async fn enqueue_inner(
    core: &FirestoreCore,
    principal: &Principal,
    upload_id: &str,
    entrypoint: &str,
    finalized_key: Option<&str>,
) -> Result<Value, AppError> {
    if !valid_entry(entrypoint) {
        return Err(failure(
            400,
            "invalid_entrypoint",
            "Entrypoint must be a safe relative path",
        ));
    }
    let upload = core.get_upload(principal, upload_id).await?;
    let share_id = upload["share_id"]
        .as_str()
        .ok_or_else(|| failure(500, "invalid_upload", "Upload share missing"))?;
    let job_id = format!("build-{upload_id}");
    let token = core.store.runtime_token().await?;
    for _ in 0..4 {
        let transaction = core.store.begin_transaction(&token).await?;
        let result: Result<Value,AppError> = async {
            let u=core.store.get(C::Uploads,upload_id,Some(&transaction),&token).await?.ok_or_else(||failure(404,"upload_not_found","Upload not found"))?;
            let s=core.store.get(C::Shares,share_id,Some(&transaction),&token).await?.ok_or_else(||failure(404,"share_not_found","Share not found"))?;
            if !principal.authenticated || s.data["owner_id"]!=principal.id || u.data["share_id"]!=share_id { return Err(failure(403,"forbidden","Publisher does not own this upload")); }
            if s.data["kind"]!="react" { return Err(failure(400,"wrong_build_kind","Durable source builds require React source")); }
            if matches!(s.data["state"].as_str(),Some("deleted"|"archived")) { return Err(failure(409,"state_conflict","Restore share before building")); }
            let frozen=&u.data["finalization"];
            if let Some(key)=finalized_key {
                if frozen["key"]!=key || frozen["entrypoint"]!=entrypoint { return Err(failure(409,"idempotency_conflict","Upload finalization does not match its request")); }
                if frozen["result"].is_object() { return Ok(frozen["result"].clone()); }
                if frozen["expected_share_version"]!=s.data["version"] || frozen["expected_latest_revision"]!=s.data["latest_revision"] { return Err(failure(409,"stale_build","Share changed after finalization began")); }
            } else if frozen.is_object() { return Err(failure(409,"upload_frozen","Upload belongs to its finalization request")); }
            let old=core.store.get(C::BuildJobs,&job_id,Some(&transaction),&token).await?;
            if let Some(old)=old {
                if old.data["owner_id"]!=principal.id { return Err(failure(403,"forbidden","Build belongs to another publisher")); }
                if old.data["entrypoint"]!=entrypoint { return Err(failure(409,"build_already_queued","This upload already has a build with a different entrypoint")); }
                return Ok(visible(old.data));
            }
            if u.data["expires_at"].as_i64().unwrap_or(0)<timestamp() { return Err(failure(410,"expired_upload","Upload session expired")); }
            if u.data["revision_id"].is_string() { return Err(failure(409,"already_published","Upload has already been published")); }
            if !u.data["staged"].as_array().is_some_and(|a| !a.is_empty()) { return Err(failure(409,"upload_not_staged","Upload files before queuing build")); }
            let digest=format!("{:x}",Sha256::digest(serde_json::to_vec(&u.data["staged"]).unwrap()));
            if finalized_key.is_some() && frozen["source_manifest_sha256"]!=digest { return Err(failure(409,"source_changed","Frozen upload source changed")); }
            let job=json!({"id":job_id,"owner_id":principal.id,"share_id":share_id,"upload_id":upload_id,"entrypoint":entrypoint,"state":"queued","attempt":0,"created_at":timestamp(),"updated_at":timestamp(),"expected_latest_revision":s.data["latest_revision"],"expected_share_version":s.data["version"],"source_manifest_sha256":digest,"lease_expires_at":null,"result":null,"error":null});
            let mut upload_next=u.data.clone(); upload_next["build_job_id"]=json!(job_id);
            let result=if finalized_key.is_some() {
                let mut receipt=json!({"share_id":share_id,"state":"queued","job_id":job_id,"status_url":format!("/api/build-jobs/{job_id}")});
                if std::env::var("ARTIFACT_BUILD_JOB_NAME").is_err() { receipt["operator_notice"]=json!("Build queued durably. A configured Cloud Run Job dispatcher is required."); }
                upload_next["finalization"]["result"]=receipt.clone();
                receipt
            } else { visible(job.clone()) };
            core.store.commit(&[
                Mutation{collection:C::BuildJobs,id:job_id.clone(),data:job,condition:Precondition::Create},
                Mutation{collection:C::Uploads,id:upload_id.into(),data:upload_next,condition:Precondition::UpdateTime(u.update_time)},
            ],Some(&transaction),&token).await?;
            Ok(result)
        }.await;
        let _ = core.store.rollback(&transaction, &token).await;
        match result {
            Err(e) if e.status == 409 && e.code == "storage_conflict" => continue,
            other => return other,
        }
    }
    Err(failure(
        409,
        "build_conflict",
        "Upload changed while queuing; retry finalization",
    ))
}

pub async fn status(
    core: &FirestoreCore,
    principal: &Principal,
    job_id: &str,
) -> Result<Value, AppError> {
    if !principal.authenticated {
        return Err(failure(401, "unauthenticated", "Authentication required"));
    }
    let token = core.store.runtime_token().await?;
    let job = load(core, job_id, &token).await?;
    if job.data["owner_id"] != principal.id {
        return Err(failure(
            403,
            "forbidden",
            "Build belongs to another publisher",
        ));
    }
    core.get_share(principal, job.data["share_id"].as_str().unwrap_or(""))
        .await?;
    // Authenticated polling also reconciles an expired worker after API scale-to-zero.
    reconcile_expired(core, job_id, timestamp()).await
}

pub struct CloudRunDispatcher {
    pub config: CloudRunJobConfig,
    client: reqwest::Client,
    endpoint: Option<String>,
}
impl CloudRunDispatcher {
    pub fn production(config: CloudRunJobConfig) -> Result<Self, AppError> {
        Ok(Self {
            config,
            client: reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(15))
                .redirect(reqwest::redirect::Policy::none())
                .build()
                .map_err(|_| failure(500, "http_client", "Cannot create Cloud Run client"))?,
            endpoint: None,
        })
    }
    /// Explicit local fake endpoint. Never forward a runtime credential to this endpoint.
    pub fn emulator(config: CloudRunJobConfig, origin: &str) -> Result<Self, AppError> {
        let u = url::Url::parse(origin)
            .map_err(|_| failure(400, "invalid_emulator", "Invalid Cloud Run emulator URL"))?;
        if u.scheme() != "http"
            || !matches!(u.host_str(), Some("127.0.0.1" | "localhost" | "[::1]"))
            || u.path() != "/"
            || u.query().is_some()
            || u.fragment().is_some()
            || !u.username().is_empty()
            || u.password().is_some()
        {
            return Err(failure(
                400,
                "invalid_emulator",
                "Cloud Run fake endpoint must be a localhost HTTP origin",
            ));
        }
        let mut out = Self::production(config)?;
        out.endpoint = Some(origin.trim_end_matches('/').into());
        Ok(out)
    }
    pub fn request_for_job(&self, job_id: &str) -> Result<RunRequest, AppError> {
        if !identifier(job_id, 180)
            || !identifier(&self.config.project, 63)
            || !dns_label(&self.config.region)
            || !dns_label(&self.config.job)
            || !dns_label(&self.config.container)
            || !(1..=120).contains(&self.config.timeout_seconds)
        {
            return Err(failure(
                400,
                "invalid_job_config",
                "Invalid bounded Cloud Run Job configuration",
            ));
        }
        Ok(RunRequest {
            method: "POST".into(),
            url: format!(
                "{}/v2/projects/{}/locations/{}/jobs/{}:run",
                self.endpoint
                    .as_deref()
                    .unwrap_or("https://run.googleapis.com"),
                self.config.project,
                self.config.region,
                self.config.job
            ),
            body: json!({"overrides":{"taskCount":1,"timeout":format!("{}s",self.config.timeout_seconds),"containerOverrides":[{"name":self.config.container,"clearArgs":true,"env":[{"name":"ARTIFACT_BUILD_JOB_ID","value":job_id}]}]}}),
        })
    }
    async fn run(&self, job_id: &str, token: &str) -> Result<String, AppError> {
        let r = self.request_for_job(job_id)?;
        let credential = if self.endpoint.is_some() {
            "local-fixture-no-credential"
        } else {
            token
        };
        if credential.is_empty() {
            return Err(failure(
                503,
                "runtime_identity_missing",
                "Cloud Run runtime identity missing",
            ));
        }
        let mut response = self
            .client
            .post(r.url)
            .bearer_auth(credential)
            .json(&r.body)
            .send()
            .await
            .map_err(|_| {
                failure(
                    503,
                    "dispatch_ambiguous",
                    "Cloud Run request outcome is uncertain; inspect durable job before retry",
                )
            })?;
        if !response.status().is_success() {
            return Err(failure(
                503,
                "dispatch_ambiguous",
                "Cloud Run dispatch was not confirmed; reconcile before retry",
            ));
        }
        let mut body = Vec::new();
        while let Some(chunk) = response
            .chunk()
            .await
            .map_err(|_| failure(503, "dispatch_ambiguous", "Cloud Run response interrupted"))?
        {
            if body.len() + chunk.len() > 64 * 1024 {
                return Err(failure(
                    502,
                    "dispatch_ambiguous",
                    "Cloud Run response exceeds bound",
                ));
            }
            body.extend_from_slice(&chunk);
        }
        let v: Value = serde_json::from_slice(&body)
            .map_err(|_| failure(502, "dispatch_ambiguous", "Invalid Cloud Run response"))?;
        let name = v["name"]
            .as_str()
            .filter(|v| {
                v.len() <= 512
                    && v.starts_with(&format!(
                        "projects/{}/locations/{}/operations/",
                        self.config.project, self.config.region
                    ))
            })
            .ok_or_else(|| {
                failure(
                    502,
                    "dispatch_ambiguous",
                    "Cloud Run operation name missing or unexpected",
                )
            })?;
        Ok(name.into())
    }
}

/// Claim dispatch exactly once locally; ambiguous external outcomes are persisted, never blindly retried.
pub async fn dispatch(
    core: &FirestoreCore,
    job_id: &str,
    dispatcher: &CloudRunDispatcher,
) -> Result<Value, AppError> {
    dispatcher.request_for_job(job_id)?;
    let token = core.store.runtime_token().await?;
    let old = load(core, job_id, &token).await?;
    if old.data["state"] != "queued" {
        return Ok(visible(old.data));
    }
    let mut next = old.data.clone();
    next["state"] = json!("dispatching");
    next["dispatch_claim"] = json!(uuid::Uuid::new_v4().to_string());
    next["updated_at"] = json!(timestamp());
    next["dispatch_deadline"] = json!(timestamp() + 180);
    match save(core, job_id, &old, next, &token).await {
        Ok(()) => {}
        // Another retry already claimed dispatch. Observe its durable state;
        // the losing caller must neither launch again nor fail acceptance.
        Err(e) if e.code == "storage_conflict" => {
            return Ok(visible(load(core, job_id, &token).await?.data));
        }
        Err(e) => return Err(e),
    }
    let outcome = dispatcher.run(job_id, &token).await;
    for _ in 0..4 {
        let old = load(core, job_id, &token).await?;
        let mut next = old.data.clone();
        next["updated_at"] = json!(timestamp());
        match &outcome {
            Ok(operation) => {
                next["operation"] = json!(operation);
                next["dispatch_state"] = json!("accepted");
            }
            Err(error) => {
                next["dispatch_state"] = json!("ambiguous");
                next["dispatch_error"] = json!(bounded(&error.message));
            }
        }
        match save(core, job_id, &old, next.clone(), &token).await {
            Ok(()) => return Ok(visible(next)),
            Err(e) if e.status == 409 => continue,
            Err(e) => return Err(e),
        }
    }
    Err(failure(
        409,
        "dispatch_record_conflict",
        "Dispatch outcome must be reconciled before another launch",
    ))
}

/// Run only in the separately launched worker process. Runtime credentials remain outside the scrubbed build subprocess.
pub async fn run_worker(
    core: &FirestoreCore,
    job_id: &str,
    build_mode: &str,
) -> Result<Value, AppError> {
    let token = core.store.runtime_token().await?;
    let old = load(core, job_id, &token).await?;
    if !matches!(old.data["state"].as_str(), Some("queued" | "dispatching")) {
        return Ok(visible(old.data));
    }
    let claim = uuid::Uuid::new_v4().to_string();
    let mut running = old.data.clone();
    running["state"] = json!("running");
    running["claim_id"] = json!(claim);
    running["attempt"] = json!(old.data["attempt"].as_u64().unwrap_or(0) + 1);
    running["lease_expires_at"] = json!(timestamp() + 180);
    running["started_at"] = json!(timestamp());
    running["updated_at"] = json!(timestamp());
    save(core, job_id, &old, running.clone(), &token).await?;
    let result: Result<Value, AppError> = async {
        let principal = Principal {
            id: running["owner_id"].as_str().unwrap_or("").into(),
            display_name: "Build worker".into(),
            verified_email: None,
            authenticated: true,
        };
        let uid = running["upload_id"].as_str().unwrap_or("");
        core.get_upload(&principal, uid).await?;
        let upload = core
            .store
            .get(C::Uploads, uid, None, &token)
            .await?
            .ok_or_else(|| failure(404, "upload_not_found", "Upload not found"))?
            .data;
        let digest = format!(
            "{:x}",
            Sha256::digest(serde_json::to_vec(&upload["staged"]).unwrap())
        );
        if running["source_manifest_sha256"] != digest {
            return Err(failure(
                409,
                "source_changed",
                "Staged source changed after build was queued",
            ));
        }
        let (files, _) = core.staged_files(&principal, uid).await?;
        let entry = running["entrypoint"]
            .as_str()
            .unwrap_or("index.html")
            .to_string();
        let mode = build_mode.to_string();
        let prepared = tokio::task::spawn_blocking(move || {
            crate::ingest::prepare_with_mode("react", files, &entry, &mode)
        })
        .await
        .map_err(|_| {
            failure(
                500,
                "build_worker_stopped",
                "Build execution thread stopped",
            )
        })?
        .map_err(|e| {
            failure(
                422,
                if e.contains("deadline") || e.contains("timed out") {
                    "build_timed_out"
                } else {
                    "build_failed"
                },
                &bounded(&e),
            )
        })?;
        let mut warnings_saved = false;
        for _ in 0..4 {
            let before_publish = load(core, job_id, &token).await?;
            if before_publish.data["state"] != "running" || before_publish.data["claim_id"] != claim
            {
                return Err(failure(
                    409,
                    "build_claim_lost",
                    "Build no longer owns promotion",
                ));
            }
            let mut with_warnings = before_publish.data.clone();
            with_warnings["warnings"] = json!(prepared.warnings);
            match save(core, job_id, &before_publish, with_warnings, &token).await {
                Ok(()) => {
                    warnings_saved = true;
                    break;
                }
                Err(e) if e.status == 409 => continue,
                Err(e) => return Err(e),
            }
        }
        if !warnings_saved {
            return Err(failure(
                409,
                "build_conflict",
                "Build state changed before promotion",
            ));
        }
        // Domain transaction verifies this live claim, frozen share version, and lifecycle before promotion.
        let mut published = core
            .publish_for_job(
                &principal,
                uid,
                &prepared.files,
                &prepared.entrypoint,
                job_id,
                &claim,
            )
            .await?;
        published["warnings"] = json!(prepared.warnings);
        Ok(published)
    }
    .await;
    let fresh_token = core.store.runtime_token().await?;
    for _ in 0..4 {
        let old = load(core, job_id, &fresh_token).await?;
        if old.data["claim_id"] != claim {
            return Err(failure(
                409,
                "build_claim_lost",
                "Build claim changed; output was not promoted by this worker",
            ));
        }
        if old.data["state"] == "succeeded" {
            return Ok(visible(old.data));
        }
        if old.data["state"] != "running" {
            return Ok(visible(old.data));
        }
        let mut done = old.data.clone();
        done["updated_at"] = json!(timestamp());
        done["finished_at"] = json!(timestamp());
        match &result {
            Ok(value) => {
                done["state"] = json!("succeeded");
                done["result"] = value.clone();
            }
            Err(e) => {
                done["state"] = json!(if e.code == "build_timed_out" {
                    "timed_out"
                } else {
                    "failed"
                });
                done["error"] = json!({"code":e.code,"message":bounded(&e.message)});
            }
        }
        match save(core, job_id, &old, done.clone(), &fresh_token).await {
            Ok(()) => return Ok(visible(done)),
            Err(e) if e.status == 409 => continue,
            Err(e) => return Err(e),
        }
    }
    Err(failure(
        409,
        "build_completion_conflict",
        "Worker completion needs durable reconciliation",
    ))
}

/// Explicit reconciler marks expired claims terminal; it never reruns uploaded code.
/// Invoke from an authenticated durable scheduler/dispatcher, not a sleeping API task.
pub async fn reconcile_expired(
    core: &FirestoreCore,
    job_id: &str,
    at: i64,
) -> Result<Value, AppError> {
    let token = core.store.runtime_token().await?;
    let old = load(core, job_id, &token).await?;
    let expired = match old.data["state"].as_str() {
        Some("running") => old.data["lease_expires_at"]
            .as_i64()
            .is_some_and(|t| t < at),
        Some("dispatching") => old.data["dispatch_deadline"]
            .as_i64()
            .is_some_and(|t| t < at),
        _ => false,
    };
    if !expired {
        return Ok(visible(old.data));
    }
    let mut data = old.data.clone();
    data["state"] = json!("timed_out");
    data["finished_at"] = json!(at);
    data["updated_at"] = json!(at);
    data["error"] = json!({"code":"worker_expired","message":"No completion before the durable worker deadline. Source will not be retried automatically."});
    save(core, job_id, &old, data.clone(), &token).await?;
    Ok(visible(data))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn config() -> CloudRunJobConfig {
        CloudRunJobConfig {
            project: "demo-project".into(),
            region: "europe-west1".into(),
            job: "artifact-builder".into(),
            container: "builder".into(),
            timeout_seconds: 120,
        }
    }
    fn input() -> BuildInput {
        BuildInput {
            job_id: "job-123".into(),
            share_id: "share-123".into(),
            input_bucket: "artifact-input".into(),
            input_object: "uploads/upload-123/source.zip".into(),
            input_generation: "42".into(),
            output_bucket: "artifact-output".into(),
            output_prefix: "builds/job-123/dist/".into(),
            expected_latest_revision: Some("revision-1".into()),
        }
    }
    #[test]
    fn bounded_scoped_request() {
        let request = config().run_request(&input()).unwrap();
        assert_eq!(
            request.url,
            "https://run.googleapis.com/v2/projects/demo-project/locations/europe-west1/jobs/artifact-builder:run"
        );
        assert_eq!(request.body["overrides"]["taskCount"], 1);
        assert_eq!(request.body["overrides"]["timeout"], "120s");
        assert_eq!(
            request.body["overrides"]["containerOverrides"][0]["env"]
                .as_array()
                .unwrap()
                .len(),
            8
        );
        assert!(!request.body.to_string().contains("TOKEN"));
    }
    #[test]
    fn rejects_url_escape_and_unpinned_inputs() {
        let mut c = config();
        c.project = "demo/../../foreign".into();
        assert!(c.run_request(&input()).is_err());
        let mut i = input();
        i.input_generation.clear();
        assert!(config().run_request(&i).is_err());
        let mut i = input();
        i.output_prefix = "builds/another-job/dist/".into();
        assert!(config().run_request(&i).is_err());
        let mut i = input();
        i.input_object = "../secret".into();
        assert!(config().run_request(&i).is_err());
    }
    #[test]
    fn rejects_unbounded_timeout() {
        let mut c = config();
        c.timeout_seconds = 0;
        assert!(c.run_request(&input()).is_err());
        c.timeout_seconds = 121;
        assert!(c.run_request(&input()).is_err());
    }
}
