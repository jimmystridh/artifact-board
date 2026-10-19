# Artifact Board

A Rust review workspace with a real Dioxus server-rendered management interface. Publish static HTML, Markdown or a full React source project; discuss it with text/element annotations, immutable revision context, replies and resolution.

**Implementation checkpoint, not a production-readiness claim.** Production target is Google Cloud Run + Firestore + Google Cloud Storage. Local SQLite is a reference-test adapter only and is refused on Cloud Run. No cloud resources, Entra app registration, credentials or Slack messages have been created.

## Run locally

Prerequisites: Rust 1.99.1, Node 24/npm for explicitly enabled trusted-source builds. Use your normal Rust installation:

```sh
rustup toolchain install 1.99.1 --profile minimal
cargo +1.99.1 run --locked -- --dev-fixtures
```

Open http://127.0.0.1:4317. Synthetic development login is visible and is permitted only on loopback, never Cloud Run. Data defaults to `.local/local-fixture.sqlite`; this is not production persistence. Use `ARTIFACT_DATA_DIR` to choose a disposable local directory.

Default React builds fail closed when no configured container adapter exists. The user explicitly authorized an optional unsandboxed mode:

```sh
ARTIFACT_BUILD_MODE=trusted-host cargo run -- --dev-fixtures
```

**Trusted-host mode executes the publisher's npm install/build scripts as the application OS user. It is not isolation.** Only an operator can enable it; uploads cannot. Use it only for trusted source and a dedicated worker with minimal runtime privileges. Subprocess environment is scrubbed, cwd is disposable, concurrency is one per process, wall time is bounded and output/logs are capped. These controls do not provide CPU/memory/network isolation or protect workload identity from source code on Cloud Run. See [build boundary](docs/BUILD-ISOLATION.md).

This executor needs explicit approved network routing for npm. `ARTIFACT_BUILD_HTTP_PROXY`, `ARTIFACT_BUILD_HTTPS_PROXY`, and `ARTIFACT_BUILD_CA_FILE` may supply a credential-free proxy and public PEM CA. Never disable TLS verification or pass a general credential-bearing environment into builds.

## Production architecture

- API/UI: Cloud Run service, Rust/Axum + Dioxus SSR and progressively enhanced JavaScript
- Persistent state: native Firestore documents, transactions, update-time CAS and immutable event/revision records
- Files: private GCS immutable object generations; staged upload manifests and revision references stay in Firestore
- React jobs: Firestore queue/claim/fencing plus Cloud Run Jobs REST dispatcher; separate worker process publishes and records success atomically
- Notifications: Firestore per-share/recipient batches; two minutes of quiet; optional Slack adapter uses verified-email lookup and exact workspace checks
- Sessions: signed, audience-bound, one-hour cookies using an operator-provided shared key; browser-bound signed OIDC state works across instances
- Preview: opaque sandboxed iframe and five-minute, revision-scoped signed content tickets; no management authority in frame; CSP blocks network egress/forms/popups/top navigation

Source uploads, comment persistence and worker state do not depend on instance disk. Scratch directories are temporary build workspaces only. See [operations and configuration](docs/OPERATIONS.md).

## Supported input contract

- `static`: an explicit existing HTML entrypoint and relative local assets
- `markdown`: explicit Markdown entrypoint; unsafe embedded HTML is escaped and unsafe link schemes rejected
- `react`: full package.json project, npm, a `build` script, static `dist/index.html` output; optional lockfile (warning when absent). Asset URLs must be relative, e.g. Vite `build --base=./`. No SSR/backend-server contract.
- ZIP and direct single/multiple-file requests normalize to the same validated manifest. Traversal, absolute paths, backslashes, case-conflicting duplicates, symlinks/special entries, expansion bombs and output symlinks are rejected.

Numerical limits are conservative prototype defaults, not user-approved final product limits. Files/expanded content are bounded by the ingestion contract; Firestore publication rejects over 400 unresolved placements, comments reject over 140 notification recipients, and repository queries reject over 10,000 documents rather than silently truncating.

## UI and MCP

Dashboard creation, uploads, revision publication, lifecycle, thread reads/posts/replies/resolution and status operations use the same application services. `/mcp` implements JSON-RPC initialize/tools/list/tools/call. Management requires an authenticated publisher session; a viewer link never grants management rights. MCP clients can use OAuth discovery, dynamic registration, Entra sign-in and explicit agent approval. Public desktop clients use loopback callbacks and S256 PKCE. Claude web and ChatGPT web can use the documented HTTPS callbacks through dynamic client registration; callback matching remains exact. The dashboard includes separate setup guides for Codex, Claude Code, Claude web and ChatGPT web. Access tokens last 15 minutes; refresh credentials rotate and expire after 30 days. Reuse revokes the grant, and owners can disconnect immediately at `/oauth/connections`. Grants persist in Firestore. Browser callers still require their session and `X-Artifact-Request: 1`. Call `get_identity` for the stable mutation actor, then `create_share`, `create_upload`, `upload_files`, and `finalize_upload` to publish entirely through MCP.

Link-only reviewers can optionally sign in, or post with a nonempty unverified name. Authenticated Entra identity does not automatically imply a verified notification email. No guest-name/email guessing occurs.

Lifecycle prototype: unpublish/archive/delete revoke viewing of all revisions; delete is a recoverable tombstone; restore returns unpublished, republish is explicit. No irreversible purge. Only the owner resolves threads pending the final moderation decision.

## Tests

```sh
cargo test
node --test assets/*.test.cjs
python3 tests/run_http_smoke.py
python3 tests/run_firestore_contract.py -- --test firestore_domain -- --ignored --nocapture
python3 tests/run_firestore_contract.py -- --test http firestore_http -- --ignored --nocapture
python3 tests/run_browser.py
```

`run_firestore_contract.py` starts the official Firestore emulator and a clearly labeled fake GCS HTTP server in the same process namespace as tests. Its JAR must first be installed using official Firebase tools; set `ARTIFACT_FIRESTORE_EMULATOR_JAR` to its path when installed elsewhere. The HTTP and Firestore runners use `cargo` from your PATH (or the `CARGO` executable override) and preserve your toolchain environment. The emulator requires Java 21. The real React smoke test intentionally runs only authored fixtures with operator-enabled trusted mode. Tests marked ignored require their named external/local fixture; an ordinary `cargo test` does not execute them.

Rendered browser acceptance is **skipped at the user's direction** after environment-boundary failures. The supported cloud browser cannot reach the execution namespace's loopback app, and the reviewed host launch fails at its filesystem boundary. Earlier local Chromium startup was also blocked. Desktop/mobile scenarios are provided, but no rendered-browser pass or screenshot is claimed. Live Entra, Slack, GCS and Cloud Run tests have not run. See [validation report](docs/TEST-REPORT.md).

## Review coverage and limits

Normal content clicks do not create annotations; explicit element mode and text selection use a bounded parent/source/nonce bridge. Original anchors/revisions are immutable. Later matches are conservative and labeled possible/unmatched. Duplicated quotes are not called exact. Shadow DOM, canvas, nested frames, cross-node quote highlighting and dynamically replaced targets may remain list-only; general feedback and original context remain available.

The browser boundary requires actual browser verification before release. Untrusted source isolation, identity admission/verified-email policy, real service permissions, operational retention and recovery, production domain/origin routing and live integration validation remain release gates. No analytics or permanent purge is silently added.

## Continuous integration

The source-checks workflow runs locked Rust tests, strict Clippy, formatting and all Node behavior regressions on pushes to main and pull requests, with read-only repository permissions and commit-pinned official GitHub actions. It also runs the pinned official Firestore emulator, synthetic GCS/Cloud Run transports, authored React source builds and real loopback HTTP publishing. Browser acceptance and live service integrations remain excluded; a green run is not full product acceptance.

## Recovering interrupted writes

Comment/reply creation and upload finalization require a stable `idempotency_key` plus expected `mutation_actor`. Preserve the exact request before sending and replay it after a lost acknowledgement. Guest retry identity is established before mutations; it never replaces the required unverified guest name or authorizes viewing. See [request recovery contract](docs/REQUEST-RECOVERY.md) for initialization, retention, identity-change handling and limits.
