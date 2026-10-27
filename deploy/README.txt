Artifact Board restricted pilot

Project: dev-innov-cm
Region: europe-north2
Origin: https://artifact-board-pilot-134099336412.europe-north2.run.app
Entra registration: spiris-visma-jira-mcp (single tenant)
Tenant: c166b9c4-5053-4eec-9665-aba0782d0804
Client: c7baacf9-7f62-40bb-8c51-fbd8d77507f3
Callback: origin + /auth/callback; existing registration callbacks are preserved.

The user approved US$20/month and scale-to-zero with minimal idle costs. The API
has zero minimum instances, one maximum instance and request-based CPU. Jobs have
no recurring scheduler, one task and zero retries. No Slack delivery is enabled.
These settings are not a hard billing cap. The named Firestore database does not
receive the default database's free quota; actual operations/storage are billable.
Billing Budget alerts are not configured (API unavailable during initial check).

The Dockerfile pins official base-image digests, upgrades runtime Debian packages
and pins npm 12.2.0 plus patched brace-expansion/undici bundles. Cargo.lock pins the JWT library to a patched release. Native
AMD64 Cloud Build avoids an observed local QEMU/GCC crash. Build sources and logs
use separate private Stockholm data/source and build-log buckets. The build identity can read that bucket and
publish to the selected Artifact Registry repository and write its dedicated build
logs; its access to the source/data bucket is read-only and it has no API secrets.

Build with the reviewed deploy/cloudbuild.json and .gcloudignore. Set _IMAGE to a
unique artifact-board-pilot tag in the approved regional registry. Specify
--region=europe-north2 and --gcs-source-staging-dir pointing into the private pilot
bucket. Record the Cloud Build ID, source digest and published image digest.

Before rollout, scan the exact image and boot it as UID 10001 in a bounded local
container. Apply only an immutable image digest:

uv run --no-project python deploy/deploy-pilot.py \
  --image europe-north2-docker.pkg.dev/dev-innov-cm/cloud-run-source-deploy/artifact-board-pilot@sha256:DIGEST \
  --approved-budget-usd 20

The script provisions a protected named Firestore database, a private uniform GCS
bucket, separate API/build/notification identities, database-conditional Firestore
access and bucket-scoped immutable object create/read permissions. The API alone
receives Secret Manager access to the session signer and existing Entra credential.
It deploys build/notification jobs and validates the service before deploying it.
The notification job stays disabled and unscheduled. React builds are on demand,
explicit trusted-host mode, for trusted publishers/source only. Uploaded build code
can access that worker's runtime identity; hostile-source isolation is unresolved.

No Git commits or source pushes are implied by deployment. Record the base commit,
exact dirty source manifest and configuration alongside image/service/job revisions.

This is a pilot. Real Entra guest/admission coverage, comprehensive hostile browser
acceptance, verified Slack email acquisition/delivery,
proactive reconciliation, backup/restore rehearsal and production operational
policies remain separate acceptance gates.

For subsequent rollbacks, retain the previous service revision/image/configuration
and check data-schema compatibility before shifting traffic. The first deployment
has no prior application revision; deny public invocation to stop access if needed.
Do not delete Firestore receipts or pinned GCS generations to force retries.

Default request-log storage excludes only this service to avoid retaining bearer
review/upload/callback URLs. App/audit logs remain enabled. Prior test-request
entries were not purged; organization-level sinks still need production review.

MCP OAuth uses the same Entra browser callback; no new app secret is needed.
Run `codex mcp login artifact-board` and approve the connection after signing in.
OAuth metadata and dynamic public-client registration accept loopback /callback
URLs only. Client names are untrusted and the browser always requests explicit
approval. Authorization codes expire after two minutes and are single-use with
S256 PKCE and exact redirect/client binding. Access lasts 15 minutes. Rotating
refresh credentials have a 30-day absolute lifetime; reuse revokes the family.
Each API request checks durable grant revocation. Owners can disconnect a single
connection at /oauth/connections. Credentials work only at /mcp and artifact
upload endpoints, never as browser sessions or upstream Entra credentials.
Grant storage contains only code/refresh hashes, identity and lifecycle metadata.
Token replies are no-store; request URLs remain excluded from default logs.
Expired grants are denied immediately but require eventual metadata retention
cleanup. The pilot supports public desktop clients, not arbitrary remote callback
URLs or confidential OAuth clients.

OAuth rollout (2026-10-19): only API revision artifact-board-pilot-00003-wt9 was
updated. Image digest sha256:638a36f1f1f664a52ca371ecf548c9a3404c21e572c3bd8272614706d5ea7ab8.
The previous API revision 00002-kvj remains available for rollback. Returning to
that revision disables OAuth endpoints and bearer support but preserves grant
metadata. Builder/notification jobs keep their previous pinned image because
this rollout changes API authorization only. OAuth signer rotation invalidates
client registrations as well as access/refresh credentials; clients must then
register again. No Git commit or source push was made.

Review introduction rollout (2026-10-19): API revision artifact-board-pilot-00004-jgb
adds a small first-visit coachmark above the review launcher. The panel stays
collapsed until requested. Opening it, Got it, closing the note, or Escape saves
only a browser preference (artifact-board:review-intro:v1); no tracking API or
user identity is recorded. Clearing browser storage allows the introduction to
appear again. Storage failures never block reviewing. Existing report revisions
and upload contents are unchanged. Min/max remain 0/1. Previous OAuth-enabled
revision 00003-wt9 is retained for rollback. Native cloud image build and smoke,
57 JavaScript tests, 82 Rust tests, strict Clippy, and desktop/mobile browser
checks passed. Sixteen Rust tests requiring external fixtures were not rerun for
this presentation-only change. No Git commit or source push was made.

Dashboard agent help rollout (2026-10-19): API revision artifact-board-pilot-00005-zjc
adds Codex add/login commands, browser sign-in instructions, a sample publishing
prompt, a collapsible MCP URL for other public OAuth clients, and connection
management/documentation links. Commands use the configured application origin.
Copy controls preserve literal commands and provide a manual-copy fallback. The
help remains visible on mobile. Validated by 59 JavaScript tests, 5 local HTTP
tests, strict Clippy, immutable image smoke/configured-origin checks, and actual
Chrome desktop/mobile inspection and copying. Existing image scan gate passed.
Min/max remain 0/1; no IAM, Entra app, job or artifact revision changed. Previous
revision 00004-jgb is retained. No Git commit or source push was made.

Comment controls rollout (2026-10-19): API revision artifact-board-pilot-00006-77k
adds a reload-scoped Hide everything with comments action, a launcher badge
counting comments and replies, and persistent in-document markers while the
panel is collapsed. General comments save document paths, fragment routes,
window and nested scroll positions; their location buttons restore that context.
Position anchors retain original revision scope and exact retry payloads.
Validation: 65 JavaScript tests, all 7 HTTP tests including real Firestore
emulator coverage, strict Clippy, formatting, exact image smoke, and browser
interaction/visual checks on an isolated local fixture and deployed existing
link-only fixture. The user's remote Chrome tunnel dropped; verification used
a separate headless browser. Image scan gate passed with the existing unfixed
OS advisory baseline. Min/max remain 0/1. No user report revision, IAM, Entra
app, worker job, Git commit, or source push changed. Previous revision
00005-zjc is retained for rollback.

Revision history, discussion lifecycle and timestamps rollout (2026-10-19):
API revision artifact-board-pilot-00009-q48 serves 100% of traffic. Version history supports earlier
revision viewing and owner-only removal of older revisions; latest is protected.
Immutable revision data is retained privately, with separate deletion tombstones
denying old viewer, content and signed frame URLs. Owners can archive/restore
discussions and delete archived discussions with confirmation. Archived/deleted
discussions are excluded from active counts and markers; writes/replays cannot
resurrect them. Hide review tools is an outlined button with reload guidance.
The composer collapses until requested and restores saved drafts. Comments and
replies show relative times under 24 hours, otherwise local absolute dates/times,
with exact timestamps/time zones on hover and semantic time elements. Minute
updates preserve drafts and stop when review tools are disabled.

Validation: all 100 Rust contract tests including real Firestore, 77 JavaScript
tests, strict Clippy/formatting, native immutable image smoke including exact
asset hashes and deletion flows, desktop/mobile browser checks, and deployed
asset/health/history inspection. Timestamp-only changes additionally passed six
local HTTP tests; external backend contracts were not repeated for presentation
changes. User Chrome remote sharing had dropped; browser checks used an isolated
headless browser with local fixtures plus the existing deployed acceptance share.
The build worker now uses the same backend release (image sha256:1e4443654b9a87c57d050340529eddc4b4ddda50288cede2d42a6500f4b7c304);
its remaining spec is unchanged. Later API-only images add copy/timestamps.
Notification job, IAM, Entra configuration and scheduling remain unchanged.
Min/max remain 0/1. Both requested design reports retain one gated revision.
Rollback to 00008-zgs is compatible with the new lifecycle state; do not roll
back to pre-feature code after deletions, as it does not check tombstones.
No Git commit or source push was made.

Agent connection guides rollout (2026-10-19): API revision artifact-board-pilot-00010-h2s adds an agent
picker for Codex, Claude Code, Claude web and ChatGPT web, tailored commands
and browser steps, copy controls, a shared trusted-origin MCP URL, workspace
notes and official documentation links. Web clients use automatic registration
(DCR), with S256 PKCE and existing explicit human consent. Registration accepts
only existing desktop loopback callbacks, Claude's exact HTTPS auth callback,
and ChatGPT's exact stable callback or bounded callback-ID route. Exact redirect
binding at authorization and token exchange remains unchanged. No arbitrary
HTTPS redirect domains or client metadata document fetching was enabled.

Validation: 79 JavaScript tests, 18 HTTP/OAuth tests including real Firestore,
strict Clippy/formatting, desktop/mobile guide switching and actual command
copying, exact image asset and registration checks, and live registration/asset
verification. Existing Codex MCP authentication works after rollout. Real Claude
and ChatGPT account connections were not performed; their documented DCR flows
were exercised against the authorization server in tests. Application scans
passed the existing gate. Only API image/config-version changed; min/max remain
0/1, with runtime identities, Entra app, secrets, jobs, IAM and schedules unchanged.
Previous revision 00009-q48 is retained; reverting would disable new web-client
registrations. No Git commit or source push was made.

Resolved discussion deletion rollout (2026-10-19): API revision artifact-board-pilot-00011-9cp removes
the comment Archive action and Archived tab. Authors resolve comments, then
optionally delete the resolved discussion and its replies with confirmation.
Reopen remains available. Unresolved deletion is rejected by both storage
backends; deleted discussions cannot be reopened or replayed from saved comment
receipts. Existing archived records are presented under Resolved and can be
reopened or deleted. The launcher count and document markers include only open
discussions. Whole-share lifecycle controls are unchanged.

Validation: 80 JavaScript tests, 10 HTTP tests including SQLite and real local
Firestore legacy archive compatibility, strict Clippy/formatting, image scan
gate and immutable image smoke tests. Live health, exact JS/CSS assets, resolved
tab, removed archived tab and revision picker verified. Build
1543cef7-8aa2-41e3-8785-ff7fa03a74d5. Only API image/config-version changed;
scaling remains 0/1 and identities, secret references, jobs, IAM and schedules
are unchanged. No test comments or revisions were added to user documents.
No Git commit or source push was made for this change.

Signed upload intent rollout (2026-10-27): API revision artifact-board-pilot-00012-np5 adds the
create_upload_intent MCP tool and POST /api/uploads/{id}/intent API. The agent
sends upload ID, format, relative path, exact byte count and SHA-256. The result
contains a signed PUT URL, method, headers and expiry. External curl/Python PUTs
send the raw file or ZIP without OAuth bearer tokens, cookies, or file bytes in
MCP. The capability is audience-separated, hash/size/session/owner-bound and
expires after at most ten minutes. It only stages bytes; finalization remains
owner-authenticated with existing stable receipts and build handling. Existing
path/archive/quota/lifecycle/frozen-upload validation still applies. Legacy
inline upload remains available. This is an application-signed API URL, not a
GCS presign; no new cloud permissions or resources were required. See
docs/AGENT-PUBLISHING.md for the agent workflow and examples.

Validation: 46 unit tests, 12 HTTP tests including real local Firestore, 8 OAuth
tests, 80 JavaScript tests, strict Clippy/formatting, curl and Python direct PUT
smoke, and immutable image smoke. Other full-suite contracts passed on the
rerun; an initial existing React concurrency test failed with build_conflict,
then passed. A stale web-client assertion expecting 15 tools was updated to 16.
The initial image scan found CVE-2026-109869 in the existing npm undici pin;
upgraded 6.28.1 to upstream-patched 6.29.1 and rebuilt. Final scan gate passed:
zero critical and zero fixable high/critical findings. The exact deployed image
passed signed upload/publication and existing feature checks. Build
ccb9e65c-b20d-42e1-9e27-8b10fbb718a7. API image/config-version only; min/max 0/1,
identities, secret refs, worker jobs, IAM and schedules unchanged. Production
verification checked image digest, traffic, health and assets. User documents
were not modified. No Git commit or push was made.
