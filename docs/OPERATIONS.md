# Deployment and operations contract

This is a configuration/runbook artifact. No deployment commands have been executed. The Dockerfile is unbuilt here because no usable container runtime is available.

## Cloud Run service

Use `ARTIFACT_STORE=firestore`, `GOOGLE_CLOUD_PROJECT`, `ARTIFACT_BUCKET` and optional `ARTIFACT_DATABASE` (default `(default)`). `PORT` follows Cloud Run's container contract. `ARTIFACT_ORIGIN` must be the exact public HTTPS origin. The binary refuses local SQLite and synthetic fixtures when `K_SERVICE` or `CLOUD_RUN_JOB` is present. Client-supplied host headers are not used to construct security policy.

Firestore stores principals, shares, idempotency references, uploads, revision metadata, threads, comments, immutable placements, participants, events, batch events, notification batches and build jobs. GCS contains all source/asset bytes. Writes use immutable object generation creation and reads pin generations. No public bucket or public object URLs are returned. No instance disk is authoritative.

## Entra

Operator configuration: `ARTIFACT_ENTRA_TENANT` (UUID), `ARTIFACT_ENTRA_CLIENT_ID`, `ARTIFACT_ENTRA_CLIENT_SECRET`, and `ARTIFACT_SESSION_KEY` (at least64 characters of securely provisioned signing material). This project does not generate/provision these credentials. Configure the exact `/auth/callback` HTTPS URI through a separately approved Entra app registration.

Login uses authorization code with PKCE, signed browser-bound state, nonce, fixed Microsoft endpoints, RS256/JWKS validation, issuer/audience/expiry/not-before and stable tenant/object identity. Publisher sessions remain valid for one hour. Signing-key rotation invalidates all sessions/tickets; logout clears the browser cookie and does not provide central stolen-token revocation. Current tenant configuration is rechecked for management. Session key must be shared consistently across instances and protected by deployment secret management.

`ARTIFACT_GATED_TENANT` is deliberately separate. Without an explicit admitted tenant policy, production gated views fail closed. Setting it equal to the configured tenant admits signed-in tenant identities; external/guest admission policy still requires product review. Verified notification email acquisition is unresolved: raw Entra email/preferred_username claims are not silently treated as verified. Real users therefore receive a recorded no-delivery state until an approved verification source is integrated.

MCP production delegated-agent credential/OAuth provisioning remains open. Current HTTP/MCP use the authenticated publisher session. Do not distribute cookie/session secrets as a substitute for a bounded agent-delegation design.

## React worker

API finalization of React on Firestore creates a deterministic durable build job and freezes the upload manifest. Configure `ARTIFACT_BUILD_JOB_NAME`, `ARTIFACT_BUILD_JOB_REGION`, optional `ARTIFACT_BUILD_JOB_CONTAINER` (default worker) to dispatch an already provisioned Cloud Run Job. No unrequested resource creation occurs. If absent, job remains visibly queued with an operator notice.

Job template uses the same binary, `ARTIFACT_STORE=firestore` and GCS/Firestore runtime configuration. Dispatch overrides only `ARTIFACT_BUILD_JOB_ID`. The worker claims a durable lease, checks immutable source manifest, builds in temporary storage and atomically checks owner/lifecycle/share version/lease before revision promotion and success. Duplicate dispatch or worker replay never reexecutes a claimed job. Ambiguous launch results are recorded and not blindly retried. Expired claims become timed-out on authorized status polling or reconciliation. Recommend job maxRetries0 because reexecution is intentionally explicit; validate final Cloud Run resource limits and timeout with an authorized real deployment.

Default `ARTIFACT_BUILD_MODE=container` is fail-closed pending a usable isolated source-builder adapter. The explicitly authorized `trusted-host` option executes source on that worker as its OS user. Environment scrubbing does not isolate metadata service, filesystem, process resources or the network. Uploaded code may reach the worker's runtime identity. Use only trusted publishers/source, least-privilege worker identity, no application/Slack/Entra secrets, restricted egress and an approved deployment threat model. This mode is not a substitute for sandbox isolation. The runtime's Job container isolates it from the API process, but does not stop source from accessing its own Job permissions.

## Notifications

Canonical comment mutations and recipient-specific batches are transactional. Relevant comment activity resets due time to activity+120 seconds; owner matches all comments, participants their own threads, including own posts under the local proposal. Missing verified email records no-delivery. New events while a batch sends form a new batch. Lifecycle changes invalidate pending/sending batches through an epoch.

`notifications::SlackAdapter` is disabled unless an operator explicitly enables and supplies an approved workspace/token. It verifies token workspace, users.lookupByEmail exact email/workspace/account status, then opens a DM and sends only a count summary. It rechecks eligibility immediately before sending. No body or private artifact capability is included in the current digest; richer links/content remain a product/privacy choice.

A configured Cloud Run notification Job or protected scheduler must invoke `notify_once` regularly. No scheduler or credentials were created here; there is no always-running in-process timer assumed across scale-to-zero. API exposes owner-only batch status. Rate limits preserve retry-after detail; unknown send outcomes and expired sending leases become ambiguous and are never automatically resent. Operator-reviewed retry/reconciliation policy remains a release gate. Exact verified-email acquisition, workspace/destination, content and event-preference choices must be approved before live delivery.

## Storage/recovery/retention

Firestore and GCS backups must be configured together and recovery tested before production. Metadata refers to immutable object generations; losing either side can break original context. No backups or lifecycle purge rules were provisioned. Restore metadata+objects into an isolated environment and validate content digests/anchors before cutover.

Delete is a recoverable tombstone; no permanent purge, source-retention duration or cleanup schedule is silently chosen. Temporary build directories clean up after terminal local execution. Durable staged-source/object retention, orphan objects after failed CAS, quota monitoring and backup retention need operator policy and cleanup implementation before a sustained production pilot.

## Security and rollout gates

- Actual browser QA of iframe sandbox/CORS/tickets, React interaction, navigation, selection, mobile and hostile content
- Real Entra login and tenant/admission matrix, plus signer secret management and token revocation expectations
- Real GCS IAM/generation semantics and Cloud Run service/Job identity/egress/resource configuration
- Real authorized Slack test, verified-address acquisition and scheduler/retry policy
- Explicit lifecycle/moderation/defaults, numeric quotas and operational retention/recovery
- Container image build and vulnerability/dependency audit

Do not call the app production-ready until these gates are closed. Current tests distinguish real emulator, fake external transport and unrun integration stages.

The notification executable entry is `ARTIFACT_NOTIFICATION_WORKER=1`, with `ARTIFACT_SLACK_DELIVERY_ENABLED=true`, `ARTIFACT_SLACK_TOKEN` and `ARTIFACT_SLACK_WORKSPACE` supplied only after explicit live-delivery authorization and secure provisioning. This entry was not run with real credentials. It prints only a processed-batch count, not payloads or capabilities.
