# Sharing and Review App Implementation Plan

Status: detailed draft derived from spec.md on 18 October 2026. This plan describes future implementation. It does not authorize coding, deployment, identity-provider registration, Slack connection, credential creation, or external messages. The application is unnamed and separate from Wipstash.

## Delivery approach

Build vertical slices around publication and review, while keeping unresolved security and product choices as explicit gates. A Rust application layer should serve both the Dioxus management interface and MCP adapter. Isolate source builds and uploaded browser content from application authority. Prove those boundaries before investing in the full review UI.

The plan uses proposed work packages rather than calendar estimates: team size, hosting, operational constraints, and compatibility targets are not yet known. Each milestone has dependencies and an observable exit gate. Recommended designs remain proposals until accepted.

## Dependency map

1. Resolve publisher authority, Entra admission, lifecycle semantics, and the first input/build compatibility contract.
2. Specify identity and content-isolation boundaries; validate them with focused prototypes after implementation is authorized.
3. Establish persistence, application contracts, authorization, and audit/event foundations.
4. Deliver static and Markdown publication end to end, then bounded React builds.
5. Deliver guest/authenticated review, then cross-revision rematching.
6. Deliver lifecycle management and complete MCP parity.
7. Deliver Slack batching against a fake service first; connect a real workspace only when separately authorized.
8. Complete operational hardening and a gated pilot.

Review UI layout and test fixtures can be prepared alongside upload work. Live integrations, lifecycle behavior, and publisher authorization must not be built on silently invented defaults.

## Milestone 0 Resolve contracts and release gates

### Tasks

- Translate specification status labels into a decision register. Keep every unresolved item linked to its affected acceptance test and work package.
- Set publisher admission and ownership rules. Define how a person and an agent authenticate, act for an owner, and lose access.
- Set gated-viewer tenant/admission rules. Enumerate external-guest cases explicitly.
- Agree unpublish/archive/delete, archive visibility, restore behavior, recovery duration, and purge policy.
- Agree an initial React build contract: runtime, package manager, entry/build script, output directory, lockfile behavior, and allowed dependency egress.
- Agree upload sizes, expanded ZIP limits, file counts, build resources, job concurrency, and timeouts. Choose values from expected workloads and measured prototype behavior rather than this draft.
- Record deferred items: standalone JSX, Visma authorization, possible CLI, and tentative analytics.

### Outputs and gate

A reviewed decision register; accepted permission matrix; lifecycle transition table; first supported-input contract; and a release scope. A blocked question does not prevent documenting independent interfaces, but implementation must not treat an unanswered choice as approval.

## Milestone 1 Validate content and build boundaries

Dependencies: accepted publisher/trust assumptions and compatibility contract; implementation authorization.

### Work package 1A Viewer isolation spike

- Prototype a trusted review shell and separately hosted content frame.
- Test script execution, assets, internal navigation, React routing, text selection, explicit element picking, scroll/resize coordinates, and a minimal bridge.
- Define the bridge's allowed messages, schemas, frame/session binding, maximum payloads, source checks, and invalid-message handling.
- Demonstrate that the frame cannot obtain application cookies or invoke management operations. Never route privileged arbitrary requests through the bridge.
- Test behavior with content that tries to navigate the top frame, spoof messages, create popups, or send outbound requests. Compare results to the accepted content-permission policy.
- Document limitations for shadow DOM, cross-origin embedded frames, canvas, transient views, and remote data. Provide an unmatched/general-comment fallback.

### Work package 1B Build boundary spike

- Build one approved sample project in a pinned disposable container.
- Apply non-root mode, no host credentials/socket/mounts, explicit resource bounds, output quotas, timeouts, and guaranteed teardown.
- Exercise package-install scripts as well as the main build command under the same restrictions.
- Test denied metadata/internal-network access where network is permitted for dependency downloads.
- Kill the worker and verify cleanup/recovery behavior; record how orphan jobs are reclaimed.

### Gate

A short reproducible spike report proves both boundaries and lists residual risks. If the bridge cannot safely support arbitrary supported content, narrow compatibility with user agreement before proceeding. If stronger isolation is needed than practical containers offer, surface that architecture choice rather than claiming security from a container label.

## Milestone 2 Application and persistence foundation

Dependencies: publisher/tenant decisions and boundary results.

### Proposed modules

| Module | Responsibility |
| --- | --- |
| Identity and authorization | Principals, sessions, agent authority, share permission checks |
| Share service | Metadata, ownership, viewer policy, current revision, lifecycle |
| Upload service | Sessions, manifests, staging validation, finalization |
| Revision service | Immutable artifacts, promotion, historical access |
| Build coordinator and worker | Queueing, execution, logs, quotas, result promotion |
| Discussion service | Threads, comments, resolution, origin anchors, activity |
| Placement service | Per-revision matching and unmatched results |
| Notification service | Recipient selection, debounce, Slack mapping/delivery |
| Dashboard and MCP adapters | Shared use cases with transport-specific representation |

These can start in one repository and a modular Rust application, with separate worker processes. Database, hosting, and queue technology choices remain open.

### Schema and invariants

- Create versioned migrations for principal, share, revision, upload_session, build_job, thread, comment, placement, activity_event, notification_batch, and slack_mapping records.
- Preserve original revision and anchor by schema/application invariants; placement rows are separate.
- Bind artifact references to share/revision ownership, not user-supplied storage paths.
- Uniquely constrain idempotency keys by principal and operation; constrain placement by thread/target revision/matcher version as appropriate.
- Record activity transactionally with the mutation that caused it. Add a durable outbox or equivalent recovery mechanism.
- Use compare-and-swap/version preconditions for settings changes and latest-revision promotion.
- Centralize authorization in application services. Transport adapters cannot opt out.
- Separate log-safe IDs from link-only bearer URLs; sanitize request and build logs.

### Gate

Migration round-trip tests, permission matrix unit tests, concurrency tests, and no-credential logging checks pass. A fake publisher can create and retrieve only authorized shares; a viewer capability cannot manage anything.

## Milestone 3 Static and Markdown publication

Dependencies: foundation; upload/input limits; viewer policy defaults.

### Tasks

1. Implement create-share and upload-session initiation with expiry and bounded authorization.
2. Support ZIP and direct one/multiple-file modes using one normalized manifest.
3. Reject traversal, absolute paths, duplicate/case-conflicting paths under the supported filesystem rules, symlinks/special files, excessive expansion, and mismatched declared sizes/digests.
4. Make finalization retry-safe; distinguish expired, incomplete, invalid, accepted, and already-finalized sessions.
5. Render Markdown using the agreed dialect/sanitization policy; validate entrypoints and asset references.
6. Promote prepared revisions atomically. Serve through the accepted viewer access boundary, including assets and historical content.
7. Build the Dioxus “my shares,” manual upload, progress/error, and share-link screens.
8. Expose equivalent MCP creation/upload/status/read operations and structured errors.

### Interface contract to specify

Requests: content kind, title, viewer policy, file manifest, entrypoint, optional configuration, idempotency key, expected revision where relevant.

Responses: share/revision/session IDs, upload URL and expiry, permitted upload method, readiness state, progress/error details, and viewer URL when ready. Upload URLs are not logged as ordinary identifiers.

Errors: unauthenticated, forbidden, invalid_manifest, expired_upload, incomplete_upload, unsupported_content, quota_exceeded, state_conflict, preparation_failed. Exact spelling is a proposal; retryability and actionable messages are required.

### Gate

Acceptance A01, A03–A07, and A13 pass for static/Markdown content. Browser and MCP integration tests demonstrate the same results. Expired/partial uploads never expose partial content or orphan permanent assets.

## Milestone 4 React source builds

Dependencies: build spike, milestone 3, accepted build contract.

### Tasks

- Detect approved project shape and package manager; enforce the documented lockfile policy without assuming mandatory lockfiles prematurely.
- Queue a build tied to an immutable revision and input digest.
- Provide progress and bounded diagnostic logs; redact credentials and unsafe terminal/control output.
- Implement queued/running/succeeded/failed/timed_out/cancelled transitions and worker leases.
- Validate produced static output, output size, entrypoint, and relative-path behavior before promotion.
- Keep the last ready revision live on failure; allow authorized retry with explicit attempt history.
- Cancel and clean up on agreed lifecycle transitions; recheck state immediately before promotion.
- Limit cache sharing; never cache credentials or let writable caches cross privilege boundaries uncontrolled.

### Gate

A02, A12, and A13 pass against a fixture set containing a valid project, install failure, missing build output, infinite build, memory exhaustion, oversized output, worker crash, duplicate completion, and deletion during build. Unsupported server-dependent projects receive a clear explanation.

## Milestone 5 Review and discussion

Dependencies: viewer bridge, authenticated/guest policy, discussion permissions.

### Tasks

- Implement thread and comment services with server-side authorization and bounded safe text rendering.
- Add nonempty guest names with an explicit unverified label. Do not merge two guests just because they use the same name.
- Implement text-selection capture and explicit element/section mode, with clear selected-target preview and cancel behavior.
- Add margin threads, replies, resolve/history filters, general comments, and a unified feed.
- Synchronize selection, scrolling, and thread opening without breaking ordinary content interactions.
- Add keyboard controls and a narrow-screen list layout; use general comments when an anchor cannot be displayed.
- Record canonical activity events for notification processing without yet sending external messages.
- Add MCP operations for accepted discussion actions, preserving identical rights and validation.

### Gate

A05, A08, A09, and A19 pass. Test authenticated and guest identities, invalid names, stored-script attempts in comments, unauthorized resolution, simultaneous replies, scrolling/layout changes, and content clicks both inside and outside annotation mode.

## Milestone 6 Revision history and rematching

Dependencies: immutable revisions and discussion model.

### Tasks

- Add original-revision navigation and clearly label original versus current placement.
- Implement deterministic text normalization and compound anchors; version the algorithm and normalization rules.
- Build a fixture corpus for repeated text, reordered sections, heading changes, deleted elements, duplicate IDs, changed routes, responsive layouts, and interactive content.
- Match unresolved threads conservatively; keep ambiguous/missing targets unmatched. Do not modify their immutable origin data.
- Implement history view for resolved threads and original-context access under current viewer policy.
- Persist matcher outcomes and expose diagnostics for review without presenting uncertain results as exact matches.
- Decide whether likely placements display automatically before shipping that behavior.

### Gate

A10–A11 pass. Review precision with the fixture corpus; set an accepted quality threshold based on measured behavior. A false confident match is more dangerous than an honest unmatched result, so optimize for preserving trust over maximizing placement count.

## Milestone 7 Lifecycle and complete management parity

Dependencies: accepted lifecycle semantics; all affected services available.

### Tasks

- Implement the agreed transition table and owner confirmations in the dashboard.
- Apply equivalent changes through MCP with the same authorization/preconditions.
- Coordinate viewer access, old revisions, active builds, caches, queued notifications, and storage retention.
- Implement restoration only if accepted, including policy preservation and expiry handling.
- Implement purging as an auditable operation after the agreed recovery window; test backup-retention implications separately.
- Add dashboard status filters and clear explanations for unavailable shares.
- Maintain a parity checklist for every accepted UI action, including read/status operations.
- If visit counts are accepted, implement the minimal agreed measure with exclusions and retention, rather than adding broad tracking.

### Gate

A16 passes across dashboard and MCP. Race tests cover delete versus upload, unpublish versus build completion, policy tightening versus cached content, restoration versus pending notifications, and concurrent lifecycle requests. Analytics A18 is a gate only if that feature is accepted.

## Milestone 8 Slack notification batches

Dependencies: accepted recipient/event semantics, verified email route, fallback policy, lifecycle behavior. Start with a fake Slack adapter. Real connection requires separate authorization.

### Tasks

- Define participant eligibility and event types precisely; record recipient expansion from canonical activity.
- Implement share/recipient keyed durable batches and a controllable clock for tests.
- Advance due time to the relevant activity time plus 120 seconds; ensure a sleeping worker cannot send a stale due batch after a reset.
- Deduplicate recipients and event IDs, including owner-as-participant cases.
- Resolve verified email through users.lookupByEmail in the chosen workspace and store the validated workspace/user mapping.
- Handle absent/deactivated/mismatched addresses without guessing. Keep guest notification behavior gated on its decision.
- Render concise digests with deep links and privacy-safe content under the agreed policy.
- Implement delivery state, transient retry, service retry instructions, and a visible failure reason. Model ambiguous sends explicitly instead of promising exactly-once delivery.
- Recheck lifecycle/access eligibility before delivery; handle new events arriving while a batch is sent.
- Decide a maximum delay, or explicitly accept unbounded postponement under continuous activity.

### Test timeline

- Event at 00:00 schedules 02:00; relevant event at 01:30 moves it to 03:30.
- Events on a different share do not delay this batch.
- Recipient-specific relevance follows the agreed reset scope.
- Worker restart at 02:30 preserves the 03:30 deadline.
- An event racing with the worker claim either resets the batch or is safely included in the next batch, according to a documented transaction rule.
- A recipient who is both owner and participant receives one batch.
- No email match yields a recorded non-delivery state, not a message to a similarly named person.
- Continuous events exercise the accepted maximum-delay policy.

### Gate

A14–A15 pass against the fake adapter. A later authorized end-to-end workspace test verifies lookup, consent/scopes, chosen destination, and digest delivery. No production message is part of this documentation task.

## Milestone 9 Operational readiness and pilot

### Tasks

- Define availability, recovery time, recovery point, retention, capacity, and supported browsers with the operator. Do not invent service-level promises.
- Instrument upload failures, queue age, build timeouts, resource use, rematch outcomes, notification delays/failures, and authorization denials without logging sensitive payloads or bearer URLs.
- Exercise database and object-store backup/restore together so revisions, original anchors, and discussion remain consistent.
- Add orphan-upload cleanup, build-workspace cleanup, queue recovery, storage quota alarms, and dependency/image update procedures.
- Document incident response for leaked link capabilities, compromised publisher credentials, failing builds, misrouted notification risk, and access revocation.
- Conduct a security review of identity validation, tenant admission, content origin separation, asset gating, upload extraction, build sandbox configuration, and notification data exposure.
- Run a small authorized pilot using representative reports, static mockups, and React projects. Record compatibility gaps and review usability before expanding scope.

### Release gate

All confirmed acceptance requirements in spec.md pass; proposed safeguards selected for release have explicit tests; no unresolved admission, lifecycle, or credential boundary is hidden behind a default. Documentation lists supported inputs and known limitations. Deferred items remain visibly deferred.

## Test layers and traceability

| Layer | Main coverage |
| --- | --- |
| Unit | Authorization decisions, lifecycle transition validation, text normalization, matching rules, debounce calculations |
| Property and fuzz | Archive paths, manifest parsing, arbitrary anchor payloads, bridge messages, concurrent state transitions |
| Integration | Database invariants, outbox recovery, object access, worker leasing, idempotent finalization |
| Browser | Gated assets, guest labeling, content isolation, selection mode, margin/list synchronization, keyboard/mobile flows |
| MCP contract | Schema validation, parity, structured errors, ownership, retry and pagination |
| Failure injection | Crashes, timeouts, network loss, resource exhaustion, stale work, duplicate events, ambiguous sends |
| Security | Token/audience/tenant checks, CSRF, stored XSS, capability leakage, cross-share access, build escape surface |

Tie each test to the specification's A01–A19 acceptance IDs and the relevant decision. A failing recommended feature should not be misreported as a rejected confirmed requirement, and an open requirement should not be marked passed using a guessed policy.

## Deliverables from future implementation

- Rust services/workers and Dioxus UI with shared application contracts.
- MCP schemas and usage examples; optional CLI only if later approved.
- Schema migrations and operator configuration reference.
- Supported input/build guide and diagnostic examples.
- Fixture corpus, acceptance suite, security checks, and recorded compatibility limits.
- Deployment, backup/restore, cleanup, and incident runbooks.
- Updated spec and terminology glossary, with ADRs only for accepted, consequential tradeoffs.

## Immediate next decision

Resolve who can create and manage shares, including how an agent obtains bounded authority. This determines ownership, “my shares,” upload authorization, and management MCP security. Continue the design conversation one question at a time; this backlog is not an instruction to interrupt the user with a bulk questionnaire.
