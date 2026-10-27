use crate::{
    auth::{error, now},
    core::{AppError, Principal},
    ingest,
    server::Shared,
};
use axum::{
    Json,
    body::to_bytes,
    extract::{Path, Request, State},
    http::header,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;

const AUDIENCE: &str = "artifact-upload-intent";
const MAX_BYTES: u64 = 32 * 1024 * 1024;

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Format {
    File,
    Zip,
}
impl Format {
    fn content_type(&self) -> &'static str {
        match self {
            Self::File => "application/octet-stream",
            Self::Zip => "application/zip",
        }
    }
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Intent {
    pub upload_id: String,
    pub format: Format,
    pub path: String,
    pub size_bytes: u64,
    pub sha256: String,
}
#[derive(Clone, Serialize, Deserialize)]
struct Ticket {
    aud: String,
    exp: u64,
    publisher: String,
    upload_id: String,
    format: Format,
    path: String,
    size_bytes: u64,
    sha256: String,
}
pub async fn create(s: &Shared, p: &Principal, intent: Intent) -> Result<Value, AppError> {
    let limit = match intent.format {
        Format::File => 16 * 1024 * 1024,
        Format::Zip => MAX_BYTES,
    };
    if intent.size_bytes > limit
        || intent.sha256.len() != 64
        || !intent.sha256.bytes().all(|c| c.is_ascii_hexdigit())
    {
        return Err(error(
            400,
            "invalid_upload_intent",
            "Provide a bounded file size and a SHA-256 hex digest.",
        ));
    }
    ingest::validate_files(BTreeMap::from([(intent.path.clone(), Vec::new())])).map_err(|_| {
        error(
            400,
            "invalid_upload_intent",
            "Provide a safe relative file path.",
        )
    })?;
    let u = s.core.get_upload(p, &intent.upload_id).await?;
    let share = s
        .core
        .get_share(p, u["share_id"].as_str().unwrap_or(""))
        .await?;
    if share["state"] == "archived" || share["state"] == "deleted" {
        return Err(error(
            409,
            "state_conflict",
            "This share does not accept uploads.",
        ));
    }
    if !u["revision_id"].is_null() || u["build_job_id"].is_string() {
        return Err(error(
            409,
            "upload_frozen",
            "This upload has already been finalized.",
        ));
    }
    let exp = (now() + 600).min(u["expires_at"].as_u64().unwrap_or(0));
    if exp <= now() {
        return Err(error(410, "expired_upload", "Upload session expired."));
    }
    let ticket = Ticket {
        aud: AUDIENCE.into(),
        exp,
        publisher: p.id.clone(),
        upload_id: intent.upload_id,
        format: intent.format,
        path: intent.path,
        size_bytes: intent.size_bytes,
        sha256: intent.sha256.to_ascii_lowercase(),
    };
    let url = format!(
        "{}/api/upload-intents/{}",
        s.auth.config.origin.trim_end_matches('/'),
        s.auth.sign(&ticket)
    );
    Ok(
        json!({"upload_id":ticket.upload_id,"upload_url":url,"method":"PUT",
        "headers":{"Content-Type":ticket.format.content_type()},"expires_at":exp,
        "size_bytes":ticket.size_bytes,"sha256":ticket.sha256,
        "instructions":"PUT the raw file bytes to upload_url using the returned headers. No Authorization header or cookies. Keep this URL private. Then call finalize_upload using the original upload_id and a stable idempotency key."}),
    )
}
pub async fn receive(
    State(s): State<Shared>,
    Path(token): Path<String>,
    req: Request,
) -> Result<Json<Value>, AppError> {
    let ticket: Ticket = s
        .auth
        .decode(&token, AUDIENCE)
        .filter(|t: &Ticket| t.exp > now() && t.size_bytes <= MAX_BYTES)
        .ok_or_else(|| {
            error(
                403,
                "invalid_upload_intent",
                "Upload URL is invalid or expired.",
            )
        })?;
    if req
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        != Some(ticket.format.content_type())
    {
        return Err(error(
            400,
            "invalid_content_type",
            "Use the Content-Type returned by the upload intent.",
        ));
    }
    let body = to_bytes(req.into_body(), ticket.size_bytes as usize + 1)
        .await
        .map_err(|_| error(413, "upload_size", "Upload exceeds the signed size."))?;
    if body.len() as u64 != ticket.size_bytes
        || format!("{:x}", Sha256::digest(&body)) != ticket.sha256
    {
        return Err(error(
            400,
            "upload_mismatch",
            "File size or SHA-256 does not match the upload intent.",
        ));
    }
    let p = Principal {
        id: ticket.publisher,
        display_name: String::new(),
        verified_email: None,
        authenticated: true,
    };
    let files = match ticket.format {
        Format::File => BTreeMap::from([(ticket.path, body.to_vec())]),
        Format::Zip => tokio::task::spawn_blocking(move || ingest::extract_zip(&body))
            .await
            .map_err(|_| error(500, "invalid_archive", "Archive worker stopped."))?
            .map_err(|e| error(400, "invalid_archive", &e))?,
    };
    s.core
        .stage_upload(
            &p,
            &ticket.upload_id,
            &files,
            files.keys().next().map(String::as_str).unwrap_or(""),
        )
        .await?;
    Ok(Json(
        json!({"id":ticket.upload_id,"state":"uploaded","file_count":files.len()}),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{auth::Config, server};
    #[tokio::test]
    async fn expired_and_other_audience_tickets_cannot_upload() {
        let dir = tempfile::tempdir().unwrap();
        let s = server::app(
            &dir.path().join("test.sqlite"),
            dir.path().join("stage"),
            Config {
                dev: true,
                origin: "http://127.0.0.1:4317".into(),
                tenant: None,
                client: None,
                secret: None,
                gated_tenant: None,
                signing_key: None,
            },
        )
        .unwrap();
        for (aud, exp) in [(AUDIENCE, now() - 1), ("artifact-session", now() + 600)] {
            let token = s.auth.sign(&Ticket {
                aud: aud.into(),
                exp,
                publisher: "dev-owner".into(),
                upload_id: "missing".into(),
                format: Format::File,
                path: "index.html".into(),
                size_bytes: 0,
                sha256: format!("{:x}", Sha256::digest([])),
            });
            let result = receive(
                State(s.clone()),
                Path(token),
                Request::builder()
                    .header("content-type", "application/octet-stream")
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await;
            assert_eq!(result.unwrap_err().status, 403);
        }
    }
}
