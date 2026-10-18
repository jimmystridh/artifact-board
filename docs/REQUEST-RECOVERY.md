# Mutation acknowledgement recovery

Comment creation, replies and upload finalization require a caller-generated `idempotency_key` and the expected `mutation_actor`. Create the key once, persist it together with the exact request **before sending**, and reuse both after a timeout, lost response or restart. Never guess that a failed connection means a write did not happen.

Keys are nonempty, at most 200 bytes, and must contain no control characters. Comment/reply keys are scoped by share, actor and operation. Finalization keys are scoped to the publisher and bound to one upload and its frozen source/entrypoint. Identical retries return the stored acknowledgement; changed input returns HTTP 409. Authorization and lifecycle admission are checked again on replay. An idempotency receipt never grants access.

## Establish identity before creating a request

Authenticated dashboard/review HTML supplies `boot.mutation_actor`; authenticated MCP initialize supplies `_meta.mutation_actor`. The value is an identity label, not a credential. Send it with each recoverable request so a changed session cannot silently execute the old request as a different person.

For a link-only guest, opening the review HTML or posting to `/api/view/{capability}/identity` establishes a signed, HttpOnly, SameSite=Lax guest retry cookie and returns the pseudonymous `mutation_actor`. Production cookies also use Secure, and initialization fails closed unless a shared signing key of at least 64 characters is configured. The cookie does not prove the guest's display name or grant viewing/management authority. It is deliberately separate from Entra identity.

Identity initialization creates no comments. If its response is lost before the client receives the identity/cookie, initialize again **before creating any mutation request**. Mutation endpoints never mint a missing identity cookie. Once a mutation is pending, keep its original actor and key. A missing, invalid, expired or changed cookie/session requires restoring the original identity; do not change the pending request to a new actor. Guest cookies last 30 days; signing-key rotation also invalidates them. Recovery after expiry/rotation is not automatically transferred to another actor.

## Durable receipts and effects

Receipts are retained with the application data, without an automatic expiry or purge. Comment/reply responses are stored transactionally with the comment, thread and persisted notification effects, with a 512 KiB response bound. Replays do not create additional comment events or reset notification debounce. This is **not** an exactly-once guarantee for external Slack delivery.

Finalization freezes staged source before processing. Repeated requests resume the same upload/revision or durable React build job. A changed entrypoint, reused key for another upload, or upload replacement after freezing is rejected. Local-reference staging is persisted in SQLite, while production staging remains immutable GCS objects referenced by Firestore. Publication and receipt completion are atomic and lifecycle/version fenced.

Local-reference preparation has a durable five-minute ownership lease (longer than the two-minute build timeout). Concurrent retries retain the original request and receive `finalize_in_progress`; after a crash, an expired lease can be reclaimed. Stale attempts cannot publish or record a terminal failure. A current attempt records a definitive preparation failure durably; transient build-busy responses release only its own claim. Restarting the service does not discard active claims. Uncommitted upload sessions expire after one hour; preparation recovery must finish before that deadline. Completed receipts remain replayable under current authorization/lifecycle admission. An expired unfinished upload currently remains in the UI recovery journal and requires explicit client-side recovery/reset; automatic abandonment is not implemented.

A direct publication receipt identifies the original ready revision even if a later revision exists. A Firestore React acceptance receipt identifies the original queued job; use its status URL for current progress. Replaying acceptance never means creating a new job or promoting its revision again.

The UI retains exact pending requests in sessionStorage and retries them before accepting a replacement operation. It keeps the original request actor; pending requests are scoped to that actor and share. SessionStorage survives reloads in the same tab, but is not a cross-device backup and may be cleared by the browser. Programmatic clients must implement their own durable request journal. Never persist session cookies or access tokens in that journal.

## Limits

The guarantee is bounded to comment/reply creation and upload finalization. Share creation already has its own idempotency contract. It does not claim recovery for arbitrary lifecycle operations, upload-session creation, or external-service delivery. Rendered browser acceptance remains skipped at the user's direction; source-level UI regressions and service/transaction tests are reported separately.
