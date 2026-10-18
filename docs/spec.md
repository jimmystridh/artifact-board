# Sharing and Review App Specification

Status: draft for continued design review. Updated 18 October 2026. Product name undecided. This is a new application, separate from Wipstash. This document does not authorize implementation, deployment, credential creation, or external integration setup.

## Purpose

An agent or person publishes a website, report, mockup, or Markdown document and receives a durable review link. Reviewers can discuss the whole share or attach threads to text and page elements. New revisions preserve the discussion's original context while making unresolved feedback useful on the latest revision.

The intended technical foundation is a Rust backend and a Dioxus management/menu UI. Agent workflows use MCP; people can manage shares and upload content through a dashboard. The main unresolved boundary is who can publish and manage shares. Viewer access policy does not answer that question.

## Status vocabulary

- **Confirmed** records the requirements accepted during the design conversation.
- **Recommended** is a proposed design or default requiring review. It is not an accepted decision.
- **Open** identifies missing product decisions or constraints.
- **Deferred** is intentionally outside the current design scope.

All technical schemas, operation names, numerical limits, and architectural components below are recommendations unless expressly marked confirmed. The acceptance matrix distinguishes confirmed behavior from proposed safeguards.

## Confirmed scope

1. Rust backend and Dioxus management/menu interface.
2. Publish static HTML sites, reports, and mockups; Markdown; and full React source projects containing package.json. Build React sources on the server. A lockfile is preferred, not yet mandatory.
3. MCP initiation creates a share/upload session and returns an upload URL. Accept ZIP archives or direct single/multiple-file uploads. Final viewer links are long and unguessable.
4. Choose viewer access separately for each share: link-only or Entra-gated. Gated shares require login for both viewing and interaction. Link-only shares permit optional login.
5. Guests commenting on link-only shares supply a nonempty, unverified name. A name such as “banana” is valid. The UI must not imply identity verification.
6. Support text-selection annotations and an explicit element/section selection tool. Ordinary interaction with uploaded content must not unexpectedly create annotations.
7. Provide Google Docs-like margin threads, replies, resolution, a discussion feed/list, and general comments.
8. Preserve each thread's original revision and anchor. Best-effort rematch unresolved threads to relevant text, elements, or sections on later revisions; label their earlier-revision origin. Unmatched threads remain in a list with access to the original version. Resolved threads stay in history unless requested.
9. Provide a dashboard with “my shares,” manual uploads, and unpublish/archive/delete actions. Provide MCP parity for application actions.
10. Use disposable, practically isolated builds with time/resource bounds and no host credentials. Publishers are trusted; the design need not claim perfect isolation.
11. Batch Slack notifications after two minutes of quiet, resetting on activity, per share and recipient. The owner matches all comment activity; participants match activity in their own threads. Use a verified identity email to resolve Slack users through users.lookupByEmail in the ordinary case, without manual mapping.

## Deferred and tentative scope

- Standalone JSX input mode is deferred. A complete React project is the supported source-project input.
- Visma organization master employee, company, and team authorization is deferred. The user already uses Entra ID and identifies Visma organization master as their directory. No directory integration is implied by this draft.
- A CLI is possible, not committed.
- Basic visit counts are tentative. Tracking rules, retention, and whether to count identified visitors are undecided.
- Server-rendered applications, arbitrary backend services, custom domains, collaborative source editing, and broad framework compatibility are not established requirements. Do not silently add them to the first release.

## Actors and permission boundaries

### Publishing and management

**Open:** publisher authentication, admission, ownership, delegation to agents, and recovery. In particular, anonymous viewing does not authorize anonymous publishing. Do not ship creation or management routes until a publisher policy is explicit.

**Recommended:** keep publishing authority separate from viewer access. Every create, upload, revision, lifecycle, and management operation resolves to an authenticated principal with explicit authority over the target share. An agent acts under bounded delegated publishing authority. A viewer URL never conveys management permission. Agent tokens, provisioning, rotation, expiry, and revocation must be designed after the publisher identity choice.

### Viewing and interaction

| Share policy | Read content | Read discussion | Comment identity |
| --- | --- | --- | --- |
| Link-only | Possession of viewer link | Recommended same boundary as content | Optional login, or nonempty unverified guest name |
| Entra-gated | Login plus approved tenant/admission policy | Same gate as content, recommended | Authenticated identity required |

**Open:** whether all admitted viewers may resolve threads; comment edit/delete/moderation powers; whether “participants” means authors and repliers only or also explicit followers; whether general comments form threads under identical rules. The working recommendation is that participation means posting in a thread, including its first comment, with the owner always subscribed.

**Open:** allowed Entra tenants and external/guest user rules. A valid login from some tenant is not itself a complete admission policy.

**Recommended:** authorize with stable provider identifiers, not names or email. For Entra, model the tenant and object identifier together. Microsoft's documentation describes oid as immutable for the object, while email and preferred_username are mutable; email is not guaranteed correct and must not be used for authorization. Treat a routable, verified notification address as separate data. [Microsoft ID token claims](https://learn.microsoft.com/en-us/entra/identity-platform/id-token-claims-reference)

## Main workflows

### Agent publication

1. An authorized publisher calls create-share with title, content kind, and viewer policy.
2. Return a share identifier, upload session, expiry, allowed upload shape, and upload URL. Return the future viewer URL only with an explicit pending state; it must not appear ready before successful publication.
3. Upload either a ZIP or a manifest-backed set of files. A direct single-file upload uses the same session lifecycle.
4. Finalize the session. Validate all content before queueing any build. Retries must not create duplicate revisions.
5. Static/Markdown input is prepared for serving; React input enters a bounded build job.
6. When preparation succeeds, atomically publish the revision and return a ready viewer URL. Failed or timed-out builds expose useful diagnostics without changing a previously published revision.

**Recommended:** separate upload, preparation, and publication explicitly so interrupted transfers and build failures cannot expose partial content. Preserve the old latest revision until its replacement is ready. Use idempotency keys for create/finalize/publish and optimistic concurrency for changes to share settings.

### Manual publication

The Dioxus dashboard invokes the same authorization and application services as MCP. A person selects a supported input, chooses viewer policy, uploads, sees progress/errors, and receives the same final share link. Display the chosen policy before publication. Do not default to link-only without an accepted default-policy decision.

### Review

A viewer opens the link, completes any required login, and sees the content and review shell. Text selection offers a comment action. Explicit element-selection mode highlights a target before thread creation; Escape exits without modifying the page. Normal mode preserves the content's click behavior. General feedback can be created without an anchor. Margin threads and a unified list present the same canonical discussion.

**Recommended:** keyboard access, visible focus, a mobile list view, and an always-available general-comment fallback. Uploaded forms, canvas, shadow DOM, nested frames, and highly dynamic React views require documented annotation coverage rather than a claim that every element can be anchored reliably.

### Revision publication

Create a new immutable revision under an existing share. Existing thread origin data never changes. Calculate a separate placement for each unresolved thread against the new revision. Display successful rematches with an earlier-revision label and an original-context link. Show uncertain or missing matches in the unmatched list. Resolved threads remain discoverable in history.

**Recommended:** latest-share URL remains stable; each immutable revision has a distinct deep link subject to the share's current access policy. Whether historical content remains viewable after particular lifecycle actions depends on the lifecycle decision below.

## Input and build contract

### Recommended input manifest

Record content kind, relative file paths, byte sizes, checksums, entrypoint, optional package-manager hint, optional build script, and output directory. ZIP uploads are normalized to the same file manifest after inspection. Detect and reject path traversal, absolute paths, ambiguous duplicate paths, unsupported links, oversized decompressed payloads, excessive file counts, and prohibited device/special files. Reject invalid input with a precise explanation.

Static HTML requires an identified entrypoint and relative assets. Markdown rendering should use a documented dialect, escape unsafe embedded HTML by default, and define local image/link behavior. Full React projects require package.json. Supported package managers, runtime versions, required scripts, static output contract, output directory, routing fallback, and missing-lockfile handling remain open.

**Recommended first compatibility target:** one pinned Node runtime and one documented package manager/build convention, producing static browser assets. Do not claim arbitrary React frameworks or server components work. A package.json alone does not guarantee a static build. Missing lockfiles should produce a warning while policy is undecided, not an invented hard requirement.

### Build execution

**Confirmed:** disposable practical container isolation, bounded time/resources, and no host credentials.

**Recommended baseline:** non-root execution; no privileged mode or host/container-runtime socket; no host home mounts; read-only base image; per-job writable workspace; CPU, memory, process, temporary-disk and output quotas; wall-clock cancellation; scrubbed environment; teardown on every terminal outcome. Separate build workers from API/database hosts. Collect bounded, sanitized logs. Install scripts are executable code and fall inside the same boundary as the build itself.

**Open:** exact limits, concurrency, dependency-network access, supported registries, cache policy, build-log retention, source retention, and whether uploaded projects may request custom build commands. Recommend network only as necessary for dependency retrieval, blocking internal/metadata endpoints and disallowing runtime server processes. These are proposals requiring a deployable network design.

Container resource restrictions must be configured explicitly; Docker documents that containers have no resource constraints by default. Containers are a practical control here, not a promise of a complete hostile-code boundary. [Docker resource constraints](https://docs.docker.com/engine/containers/resource_constraints/)

**Recommended job states:** queued, running, succeeded, failed, timed_out, cancelled. Record toolchain image digest, input digest, command, start/end times, bounded diagnostic output, and output digest. Workers claim jobs atomically; a crashed worker cannot publish twice. Builds never receive Slack or identity-provider secrets.

## Viewer content isolation

**Recommended, not yet accepted:** host uploaded content separately from the management/application origin and keep the discussion UI in a trusted shell. Use a sandboxed content frame with a narrow annotation bridge. Never expose application sessions, owner tokens, Slack credentials, or arbitrary authenticated API access to uploaded scripts. Use host-only secure cookies, narrow CORS, CSRF protection, explicit message-origin/source checks, and payload validation.

The annotation bridge is a design spike, not solved merely by choosing an iframe. A parent cannot directly inspect an unrelated origin's DOM. A cooperating injected bridge can describe selections and rectangles, but uploaded script may tamper with its own frame; messages must never authorize management operations. Where sandboxing creates opaque origins, source validation and a per-frame handshake need explicit design. Never trust an origin string of “null” alone.

The HTML iframe specification warns about combining allow-scripts and allow-same-origin for same-origin content; the application must test the actual origin and sandbox arrangement rather than copy flags mechanically. [MDN iframe reference](https://developer.mozilla.org/en-US/docs/Web/HTML/Reference/Elements/iframe)

**Open:** permitted outbound requests from uploaded browser JavaScript, external scripts/assets, popups, downloads, navigation, clipboard, storage, and forms. Trusted publishers can still upload compromised dependencies or accidentally invasive code. Gated serving cannot prevent authorized content JavaScript from transmitting what it sees if arbitrary egress is allowed; this residual risk needs an explicit policy.

All gated HTML, assets, source downloads if enabled, revision routes, annotation data, and build diagnostics must enforce their appropriate access boundary. Do not protect only the shell while exposing object-storage URLs publicly. Cache headers and CDN keys must preserve the gate. An unguessable URL is a bearer capability for link-only access; avoid leaking it into referrers, logs, analytics, and previews. Recommend at least 128 bits of cryptographic randomness and non-indexing headers, while noting that non-indexing is not access control.

## Annotation and revision model

**Confirmed invariant:** original revision and original anchor remain immutable. A rematch is an additional placement; it does not rewrite history.

**Recommended anchor payload:** revision ID, document/route path, anchor kind, exact normalized text where applicable, short surrounding context, positional hint, stable element identifier if present, structural path fallback, semantic element/section label, and algorithm version. Avoid relying exclusively on CSS paths or viewport coordinates. Store only bounded context, not an accidental copy of an entire document.

The W3C model defines text-quote selectors using exact text and surrounding prefix/suffix, and text-position selectors using offsets. Position selectors are brittle across content changes. These are useful foundations, not a prescribed complete matching algorithm. [W3C Web Annotation Data Model](https://www.w3.org/TR/annotation-model/#selectors)

**Recommended rematch sequence:** same stable identifier with compatible context; unique exact quote plus context; conservative contextual/section match; otherwise unmatched. Multiple plausible candidates are ambiguous, not success. Store placement result, algorithm version, confidence class, and rationale. Use exact / likely / unmatched labels initially; numeric confidence thresholds require a real fixture corpus and review. Preserve explicit human reattachment separately if that feature is later approved.

**Open:** automatic display of “likely” matches versus requiring review; handling route changes; whether thread resolution carries across all revisions; reopening rights; whether to snapshot runtime state for interactive pages. Recommend share-wide thread resolution and original revision context, but original source alone cannot reproduce time-dependent or externally fetched runtime content. The first release should state this limitation.

## Recommended data model

| Record | Core fields and invariants |
| --- | --- |
| Principal | Internal ID; provider tenant/object subject when applicable; display identity; no email-based ownership |
| Share | Owner principal; title; viewer policy; lifecycle state; latest revision pointer; opaque viewer capability |
| Revision | Share ID; immutable input/output digests; content kind; manifest; preparation status; timestamps |
| Upload session | Authorized principal/share; permitted payload; expiry; upload status; finalization key |
| Build job | Revision; state; toolchain; limits; logs reference; attempt and timing |
| Thread | Share; immutable originating revision/anchor; general or anchored type; resolution state |
| Comment | Thread; actor; body; time; edit/deletion metadata if later approved |
| Guest identity | Unverified display name and session identifier, never treated as verified principal |
| Placement | Thread; target revision; matched anchor; confidence/status; matcher version |
| Activity event | Unique event ID; share/thread; actor; event type; sequence and time |
| Notification batch | Share/recipient; included event IDs; deadline; delivery and retry state |
| Slack mapping | Verified notification identity; Slack workspace/user IDs; validation time/status |

**Recommended storage:** relational metadata and transactional activity/outbox records; object storage for immutable artifacts; a durable build/notification queue. Exact database, object store, hosting provider, and queue are open. A modular Rust service with separately deployed workers is a reasonable first shape; microservices are not required.

## Lifecycle

**Confirmed:** unpublish, archive, and delete must exist, including MCP parity.

**Open and release-blocking:** precise meaning of each action, viewer behavior, build cancellation, recovery window, permanent deletion timing, and what happens to comments and old revision links. The following is a proposal only:

| Action | Recommended meaning to review |
| --- | --- |
| Unpublish | Stop viewer access while retaining owner-managed content and discussion; allow republish |
| Archive | Remove from the default active dashboard and freeze new changes; viewer availability must be chosen explicitly |
| Delete | Enter recoverable deleted state; deny viewing and new writes; permanently purge only after an agreed retention window |

Do not implement archive as secretly public or secretly unavailable. Do not set a recovery duration without agreement. A lifecycle transition must atomically affect authorization, queued work, notification eligibility, and cache behavior. Recheck state at build promotion and notification send time. Restoring must not revive invalid credentials or silently weaken viewer policy.

## MCP surface and parity

**Recommended operation families:** create/list/get shares; initiate/finalize upload; get/cancel build; create/list/get revisions; publish a ready revision; change title/viewer policy; list/create/reply/resolve threads; unpublish/archive/delete and agreed restore actions; retrieve supported visit summaries; inspect notification delivery state. Names and schemas are not fixed.

MCP tools and dashboard services share validation and authorization. An MCP request must not become a bypass for gated comments, ownership, archive restrictions, upload quotas, or lifecycle confirmations. Return structured errors with machine-readable codes, actionable descriptions, retryability, and request IDs. Pagination, idempotency, revision preconditions, capability discovery, and cancellation should be tested across transports.

**Open:** whether “all actions” includes comment moderation, notification preferences, guest interactions, and identity/session management. Resolve parity as a checklist when each UI action is accepted. A CLI can later reuse these contracts.

## Slack notification behavior

### Confirmed behavior

Group eligible comment activity separately for each share and recipient. The owner matches all comment activity; a participant matches their own thread activity. Wait for two minutes of quiet and reset on further activity. Resolve a recipient through a verified identity email and Slack users.lookupByEmail in the normal case; no routine manual mapping.

Slack documents the lookup as using the workspace user's registered email and requiring users:read.email. A missing or deactivated account can return users_not_found. This method resolves a user; it is not the complete message-delivery integration. [Slack users.lookupByEmail](https://docs.slack.dev/reference/methods/users.lookupByEmail/)

### Recommended durable algorithm

1. Commit comment mutation and uniquely identified activity event together.
2. Derive recipients without duplicating a person who is both owner and participant.
3. Append to each share/recipient batch and set its due time to the newest relevant event time plus 120 seconds.
4. A worker claims due batches atomically, rechecks policy/recipient eligibility and whether activity extended the window, then delivers a concise digest with a deep link.
5. Record included event IDs and delivery result. New events during delivery belong to the next batch. Retry transient failures with bounded backoff and respect service retry instructions.

**Open:** whether “activity” resetting the timer means only recipient-relevant comments or any comment on the share; whether edits, resolutions, deletions, and one's own comments trigger alerts; maximum delay during continuous activity; notification preferences; approved Slack workspace; message destination and content; recovery after ambiguous send outcome. Do not claim exactly-once external delivery. Recommend minimizing content in notifications for gated shares and verifying continued access before sending.

**Open:** how to obtain/verify a usable email when Entra claims are absent or unsuitable; mismatched-email fallback; guest notification identity. Never match a guest's typed name to Slack or assume an Entra claim matches a Slack address. Use a no-delivery state with a clear reason rather than guessing a recipient while fallback is undecided.

This draft does not connect Slack, register apps, request permissions, create credentials, or send messages.

## Acceptance matrix

| ID | Status | Scenario and acceptance result |
| --- | --- | --- |
| A01 | Confirmed | Publish supported static HTML and local assets; ready link renders them |
| A02 | Confirmed | Publish Markdown and a full React source project with package.json; React is built server-side |
| A03 | Confirmed | MCP initiation returns upload URL; ZIP, single-file, and multifile flows are supported |
| A04 | Recommended | Interrupted upload, checksum mismatch, and unsafe archive fail before publication |
| A05 | Confirmed | Link-only viewer reads without login; guest name is nonempty and visibly unverified |
| A06 | Confirmed plus open admission | Gated viewer cannot read or interact before login and allowed-admission validation |
| A07 | Recommended | Knowing the viewer URL never grants create/update/delete privileges |
| A08 | Confirmed | Text selection and explicit element selection create threads; normal clicks preserve page behavior |
| A09 | Confirmed | Reply, resolve, general comments, margin display, and unified list share consistent state |
| A10 | Confirmed | New revision retains original anchors; unresolved rematches are labeled; unmatched and resolved history remain accessible as specified |
| A11 | Recommended | Duplicated paragraphs and moved/deleted elements do not silently produce confident incorrect anchors |
| A12 | Confirmed | Build exceeds configured bounds and terminates; host credentials are absent |
| A13 | Recommended | Failed build never replaces an existing ready revision; duplicate finalization creates one revision |
| A14 | Confirmed | Owner and participant notification batches wait for two quiet minutes and reset on relevant activity once reset scope is decided |
| A15 | Recommended | Restarts preserve pending batches; duplicate events are deduplicated; unavailable identity does not cause guessed delivery |
| A16 | Confirmed plus open semantics | Dashboard lifecycle actions have equivalent MCP behavior and follow the agreed recovery policy |
| A17 | Recommended | Content frames cannot call owner APIs, steal app sessions, or bypass gated asset access |
| A18 | Tentative | If visit counts are accepted, counts and exclusions match a documented privacy policy |
| A19 | Recommended | Keyboard/mobile review and fallback list remain usable when an anchor cannot be displayed |

## Design decisions still required

These are an ordered backlog for the continuing interview, not a request to answer everything at once.

1. Who can publish/manage, how agents receive authority, and whether shares have one owner or collaborators.
2. Which Entra tenants/users may view gated shares, including external guests.
3. Default viewer policy and whether policy may change after comments exist.
4. Exact unpublish/archive/delete behavior and recovery/retention.
5. React build contract, runtime/package manager, build network policy, and numerical upload/build limits.
6. Browser-content permissions and isolation architecture.
7. Thread participation and moderation rights; rematch confidence policy and dynamic-content coverage.
8. Verified notification email acquisition, missing/mismatched accounts, guest notifications, event/reset rules, and maximum debounce delay.
9. Visit tracking scope, source/revision retention, hosting environment, and operational objectives.
10. Product name and final document/repository destination.

No ADR is included: the conversation has not yet established a hard-to-reverse, surprising tradeoff with an accepted rationale that warrants one. These recommendations should not be converted into accepted ADRs automatically.
