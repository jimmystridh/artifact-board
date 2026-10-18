//! Disabled-by-default Slack delivery, enabled only after operator approval/configuration.
//! Fixed official API: https://docs.slack.dev/reference/methods/users.lookupByEmail/
//! No live Slack calls occur in tests. No raw comment content or bearer links are sent.
use crate::{core::AppError, firestore_core::FirestoreCore};
use reqwest::{Client, Method};
use serde::Serialize;
use serde_json::{Value, json};
use std::{future::Future, time::Duration};
use url::Url;

pub struct SlackAdapter {
    client: Client,
    token: String,
    workspace: String,
    base: String,
}
#[derive(Debug, Clone, Serialize)]
pub struct DeliveryOutcome {
    pub state: String,
    pub reason: String,
    pub retry_after_seconds: Option<u64>,
}
fn outcome(state: &str, reason: &str, retry: Option<u64>) -> DeliveryOutcome {
    DeliveryOutcome {
        state: state.into(),
        reason: reason.into(),
        retry_after_seconds: retry,
    }
}
fn error(code: &str, message: &str) -> AppError {
    AppError {
        status: 503,
        code: code.into(),
        message: message.into(),
    }
}
impl SlackAdapter {
    pub fn new(token: String, approved_workspace: String, enabled: bool) -> Result<Self, AppError> {
        if !enabled {
            return Err(error(
                "slack_disabled",
                "Slack delivery is disabled pending explicit operator approval",
            ));
        }
        if token.is_empty()
            || !approved_workspace.starts_with('T')
            || !approved_workspace
                .chars()
                .all(|c| c.is_ascii_alphanumeric())
        {
            return Err(error(
                "slack_unconfigured",
                "Explicit token and approved workspace ID required",
            ));
        }
        Ok(Self {
            client: Client::builder()
                .timeout(Duration::from_secs(10))
                .redirect(reqwest::redirect::Policy::none())
                .build()
                .map_err(|_| error("slack_client", "Cannot create Slack client"))?,
            token,
            workspace: approved_workspace,
            base: "https://slack.com/api".into(),
        })
    }
    /// Local fixture constructor never accepts a real token or arbitrary remote endpoint.
    pub fn fixture(endpoint: &str, workspace: &str) -> Result<Self, AppError> {
        let u = Url::parse(endpoint)
            .map_err(|_| error("invalid_fixture", "Invalid local Slack fixture URL"))?;
        if u.scheme() != "http"
            || !matches!(u.host_str(), Some("127.0.0.1" | "localhost" | "[::1]"))
            || u.path() != "/"
            || u.query().is_some()
            || u.fragment().is_some()
            || !u.username().is_empty()
            || u.password().is_some()
        {
            return Err(error(
                "invalid_fixture",
                "Slack fixture must be a localhost HTTP origin",
            ));
        }
        let mut s = Self::new("fixture-only".into(), workspace.into(), true)?;
        s.base = endpoint.trim_end_matches('/').into();
        Ok(s)
    }
    async fn request(
        &self,
        method: &str,
        args: Value,
        send: bool,
    ) -> Result<Value, DeliveryOutcome> {
        let mut request = if method == "users.lookupByEmail" {
            self.client
                .request(Method::GET, format!("{}/{method}", self.base))
                .query(&[("email", args["email"].as_str().unwrap_or(""))])
        } else {
            self.client
                .post(format!("{}/{method}", self.base))
                .json(&args)
        };
        request = request.bearer_auth(&self.token);
        let response = request.send().await.map_err(|_| {
            outcome(
                if send { "ambiguous" } else { "failed" },
                if send {
                    "Send response missing; do not retry without reconciliation"
                } else {
                    "Slack request unavailable"
                },
                None,
            )
        })?;
        if response.status().as_u16() == 429 {
            let retry = response
                .headers()
                .get("retry-after")
                .and_then(|v| v.to_str().ok())
                .and_then(|v| v.parse().ok());
            return Err(outcome(
                "failed",
                "Slack rate limit; retry only after the reported delay",
                retry,
            ));
        }
        if !response.status().is_success() {
            return Err(outcome(
                if send && response.status().is_server_error() {
                    "ambiguous"
                } else {
                    "failed"
                },
                "Slack HTTP request failed",
                None,
            ));
        }
        let value: Value = response.json().await.map_err(|_| {
            outcome(
                if send { "ambiguous" } else { "failed" },
                "Invalid Slack response",
                None,
            )
        })?;
        if value["ok"] != true {
            let code = value["error"].as_str().unwrap_or("unknown_error");
            let state =
                if send && ["internal_error", "fatal_error", "request_timeout"].contains(&code) {
                    "ambiguous"
                } else {
                    "failed"
                };
            let safe = match code {
                "users_not_found" => "No exact Slack user for verified email",
                "missing_scope" => "Slack token lacks required scope",
                "invalid_auth" | "account_inactive" => "Slack identity is inactive or invalid",
                "internal_error" | "fatal_error" | "request_timeout" => {
                    "Slack send outcome uncertain; do not retry without reconciliation"
                }
                _ => "Slack rejected request",
            };
            return Err(outcome(state, safe, None));
        }
        Ok(value)
    }
    /// `eligible` must recheck durable share lifecycle and batch lease immediately before send.
    pub async fn deliver<F, Fut>(&self, batch: &Value, eligible: F) -> DeliveryOutcome
    where
        F: FnOnce() -> Fut,
        Fut: Future<Output = Result<bool, AppError>>,
    {
        let Some(email) = batch["verified_email"].as_str().filter(|e| !e.is_empty()) else {
            return outcome("failed", "No verified email; no lookup attempted", None);
        };
        let Some(batch_id) = batch["id"].as_str() else {
            return outcome("failed", "Missing durable batch identifier", None);
        };
        let auth = match self.request("auth.test", json!({}), false).await {
            Ok(v) => v,
            Err(e) => return e,
        };
        if auth["team_id"] != self.workspace {
            return outcome(
                "failed",
                "Slack token belongs to an unapproved workspace",
                None,
            );
        }
        let lookup = match self
            .request("users.lookupByEmail", json!({"email":email}), false)
            .await
        {
            Ok(v) => v,
            Err(e) => return e,
        };
        let user = &lookup["user"];
        if user["team_id"] != self.workspace
            || user["deleted"] != false
            || user["is_bot"] == true
            || user["profile"]["email"]
                .as_str()
                .is_none_or(|e| !e.eq_ignore_ascii_case(email))
        {
            return outcome(
                "failed",
                "Slack user does not match the verified active workspace identity",
                None,
            );
        }
        let Some(uid) = user["id"].as_str().filter(|id| !id.is_empty()) else {
            return outcome("failed", "Slack user identifier missing", None);
        };
        let dm = match self
            .request(
                "conversations.open",
                json!({"users":uid,"return_im":true}),
                false,
            )
            .await
        {
            Ok(v) => v,
            Err(e) => return e,
        };
        let Some(channel) = dm["channel"]["id"]
            .as_str()
            .filter(|id| id.starts_with('D'))
        else {
            return outcome("failed", "Slack did not return a direct conversation", None);
        };
        match eligible().await {
            Ok(true) => {}
            Ok(false) => {
                return outcome(
                    "failed",
                    "Share or batch no longer eligible; no message sent",
                    None,
                );
            }
            Err(_) => {
                return outcome(
                    "failed",
                    "Eligibility could not be verified; no message sent",
                    None,
                );
            }
        }
        let count = batch["event_count"].as_u64().unwrap_or(0);
        if count == 0 {
            return outcome("failed", "Empty notification batch", None);
        }
        let result=self.request("chat.postMessage",json!({"channel":channel,"text":format!("You have {count} new comment updates on an Artifact Board share. Open Artifact Board to review them."),"mrkdwn":false,"unfurl_links":false,"unfurl_media":false,"client_msg_id":batch_id}),true).await;
        match result {
            Ok(v) if v["channel"] == channel && v["ts"].is_string() => {
                outcome("delivered", "Slack accepted the digest", None)
            }
            Ok(_) => outcome(
                "ambiguous",
                "Slack acceptance response incomplete; do not retry blindly",
                None,
            ),
            Err(e) => e,
        }
    }
    /// Called only by an explicitly configured notification job, never on web requests.
    pub async fn notify_once(&self, core: &FirestoreCore, at: i64) -> Result<Value, AppError> {
        let batches = core.claim_due_notifications(at).await?;
        let mut report = vec![];
        for batch in batches
            .as_array()
            .ok_or_else(|| error("invalid_batches", "Invalid durable batch result"))?
        {
            let bid = batch["id"]
                .as_str()
                .ok_or_else(|| error("invalid_batch", "Missing batch identifier"))?;
            let lease = batch["lease"]
                .as_str()
                .ok_or_else(|| error("invalid_batch", "Missing batch lease"))?;
            let result = self
                .deliver(batch, || core.notification_eligible(bid, lease))
                .await;
            let detail = serde_json::to_string(&result).map_err(|_| {
                error(
                    "notification_serialization",
                    "Cannot serialize notification result",
                )
            })?;
            let persisted = core
                .finish_notification(bid, lease, &result.state, Some(&detail))
                .await;
            report.push(json!({"batch_id":bid,"outcome":result,"recorded":persisted.is_ok()}));
        }
        Ok(json!(report))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{
        Json, Router,
        body::to_bytes,
        extract::{Request, State},
        http::StatusCode,
        response::{IntoResponse, Response},
        routing::any,
    };
    use std::sync::{Arc, Mutex};
    #[derive(Clone)]
    struct Fixture {
        seen: Arc<Mutex<Vec<(String, Value)>>>,
        mode: &'static str,
    }
    async fn fake(State(f): State<Fixture>, req: Request) -> Response {
        assert_eq!(req.headers()["authorization"], "Bearer fixture-only");
        let path = req.uri().path().to_string();
        let query = req.uri().query().unwrap_or("").to_string();
        let bytes = to_bytes(req.into_body(), 100_000).await.unwrap();
        let body: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
        f.seen.lock().unwrap().push((path.clone(), body));
        let v = match path.as_str() {
            "/auth.test" => {
                json!({"ok":true,"team_id":if f.mode=="wrong_workspace"{"TOTHER"}else{"TTEST"}})
            }
            "/users.lookupByEmail" => {
                assert!(query.contains("email=verified%40example.test"));
                if f.mode == "missing" {
                    json!({"ok":false,"error":"users_not_found"})
                } else {
                    json!({"ok":true,"user":{"id":"U_TEST","team_id":if f.mode=="wrong_user_workspace"{"TOTHER"}else{"TTEST"},"deleted":f.mode=="deleted","is_bot":false,"profile":{"email":if f.mode=="email_mismatch"{"other@example.test"}else{"verified@example.test"}}}})
                }
            }
            "/conversations.open" => json!({"ok":true,"channel":{"id":"D_TEST"}}),
            "/chat.postMessage" => {
                if f.mode == "rate_limit" {
                    return (StatusCode::TOO_MANY_REQUESTS, [("retry-after", "30")], "{}")
                        .into_response();
                }
                if f.mode == "timeout" {
                    tokio::time::sleep(Duration::from_millis(150)).await;
                }
                if f.mode == "ambiguous" {
                    json!({"ok":false,"error":"internal_error"})
                } else {
                    json!({"ok":true,"channel":"D_TEST","ts":"1.0"})
                }
            }
            _ => panic!("Unexpected API route"),
        };
        Json(v).into_response()
    }
    async fn run(mode: &'static str, eligible: bool) -> (DeliveryOutcome, Vec<(String, Value)>) {
        let seen = Arc::new(Mutex::new(vec![]));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!("http://{}", listener.local_addr().unwrap());
        let task = tokio::spawn(
            axum::serve(
                listener,
                Router::new().fallback(any(fake)).with_state(Fixture {
                    seen: seen.clone(),
                    mode,
                }),
            )
            .into_future(),
        );
        let mut adapter = SlackAdapter::fixture(&endpoint, "TTEST").unwrap();
        if mode == "timeout" {
            adapter.client = Client::builder()
                .timeout(Duration::from_millis(40))
                .redirect(reqwest::redirect::Policy::none())
                .build()
                .unwrap();
        }
        let result=adapter.deliver(&json!({"id":"11111111-1111-4111-8111-111111111111","verified_email":"verified@example.test","event_count":3}),||async move{Ok(eligible)}).await;
        let requests = seen.lock().unwrap().clone();
        task.abort();
        (result, requests)
    }
    #[tokio::test]
    async fn exact_lookup_private_digest_and_eligibility() {
        let (result, requests) = run("success", true).await;
        assert_eq!(result.state, "delivered");
        assert_eq!(
            requests.iter().map(|x| x.0.as_str()).collect::<Vec<_>>(),
            vec![
                "/auth.test",
                "/users.lookupByEmail",
                "/conversations.open",
                "/chat.postMessage"
            ]
        );
        let body = &requests[3].1;
        assert_eq!(body["channel"], "D_TEST");
        assert_eq!(body["unfurl_links"], false);
        assert!(
            !body["text"]
                .as_str()
                .unwrap()
                .contains("verified@example.test")
        );
        let (result, requests) = run("success", false).await;
        assert_eq!(result.state, "failed");
        assert!(!requests.iter().any(|r| r.0 == "/chat.postMessage"));
    }
    #[tokio::test]
    async fn mismatched_and_inactive_identity_never_sends() {
        for mode in [
            "wrong_workspace",
            "wrong_user_workspace",
            "email_mismatch",
            "missing",
            "deleted",
        ] {
            let (result, requests) = run(mode, true).await;
            assert_eq!(result.state, "failed");
            assert!(!requests.iter().any(|r| r.0 == "/chat.postMessage"));
        }
    }
    #[tokio::test]
    async fn uncertain_send_and_retry_after_not_retried() {
        for mode in ["ambiguous", "timeout"] {
            let (result, requests) = run(mode, true).await;
            assert_eq!(result.state, "ambiguous");
            assert_eq!(
                requests
                    .iter()
                    .filter(|r| r.0 == "/chat.postMessage")
                    .count(),
                1
            );
        }
        let (result, requests) = run("rate_limit", true).await;
        assert_eq!(result.state, "failed");
        assert_eq!(result.retry_after_seconds, Some(30));
        assert_eq!(
            requests
                .iter()
                .filter(|r| r.0 == "/chat.postMessage")
                .count(),
            1
        );
    }
    #[test]
    fn disabled_and_remote_fixtures_fail_closed() {
        assert!(SlackAdapter::new("token".into(), "TAPPROVED".into(), false).is_err());
        assert!(SlackAdapter::fixture("https://slack.com", "TAPPROVED").is_err());
    }
}
