# Artifact Board decision register

The original specification and plan remain preserved with confirmed/proposed/open/deferred distinctions. Implementation authorization subsequently superseded their documentation-only status.

## Confirmed during implementation,18 October2026

- Product name: Artifact Board; separate from Wipstash
- Build/run/test on the assistant's own cloud computer
- Publishing/management require the owner's Entra tenant; viewer access remains independent per share
- Deployment target: Google Cloud Run, Firestore for application state/comments, object storage for files (GCS implemented)
- Configurable unsandboxed trusted-source builds are allowed; operator-only, disabled by default

No cloud deployment, resource creation, app registration, credentials or live Slack messages were authorized or performed.

## Local implementation choices, still provisional product contracts

- Explicit viewer-policy selection, no implicit link-only default
- Single owner; owner-only resolution and management
- Unpublish/archive/delete revoke all viewer routes including original revisions; delete is a recoverable tombstone; restore becomes unpublished; explicit republish
- No irreversible purge or chosen retention period
- Participation means posting in a thread; recipient-relevant comment activity resets the two-minute timer; own posts currently count
- Missing verified email produces recorded no-delivery, never guessed Slack mapping
- Possible/unmatched anchor placements are labeled conservatively; no claim of exact source-to-rendered-DOM matching
- Content runs in an opaque sandbox with no same-origin privilege, forms, popups or top navigation; outbound browser network blocked
- Five-minute revision-scoped preview tickets support opaque-origin module loading without management cookies
- Local validation quotas/fanout bounds are explicit and fail rather than silently truncate

## Implementation and external validation status

Firestore dispatch, immutable GCS references, durable worker queue/claims and notification batches are implemented. Official Firestore emulator tests pass; GCS and Cloud Run transport tests use explicit fakes. SQLite is restricted to the local reference adapter.

Entra and Slack adapters are implemented and fail closed when unconfigured. Actual tenant IDs, app registration, session signer secret, verified-email source, Slack workspace/destination and live integration validation remain unconfigured/unrun.

Container source-build mode is fail-closed because no usable container runtime exists here. The explicitly enabled trusted-host mode was exercised with our own React/source fixtures. It does not provide sandbox isolation. The Cloud Run Job template/resources and runtime permissions are not provisioned.

Browser execution is blocked by the cloud executor's Chromium socket permission before page load. Dioxus render/escape tests and annotation bridge tests pass, but visual desktop/mobile, iframe security and React interaction must still be validated in a supported browser.

## Still open before production

Gated-viewer tenant/guest admission, agent delegation/provisioning, verified-email acquisition, moderation/reopening rights, full lifecycle/retention/recovery semantics, final numerical limits, source build isolation/egress, Slack scheduler/retry/destination/content, session revocation expectations, production domain/origin details, backup/restore and operational targets. Analytics remains deferred/tentative. These are not silently converted into accepted ADRs.
