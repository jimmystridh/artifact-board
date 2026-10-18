# Artifact Board validation report

Date:18 October2026. Rust1.99.1; Node24.19.0; official Firestore emulator1.22.0/Java21. All code/source fixtures are newly authored for Artifact Board; Wipstash was not modified.

## Passed

- Rust aggregate including explicitly enabled integration tests: **68 passed,0 failed,0 ignored** (43 library,2 build-contract,4 Firestore domain,5 HTTP,14 ingestion executions). Some ingestion tests run in both library and integration targets; this is an execution count, not68 unique scenarios.
- Optimized release build (`cargo build --release --locked`): passed; optimized binary HTTP readiness/Dioxus response and graceful SIGTERM shutdown passed
- Clippy all targets with warnings denied: passed
- Rust formatting check: passed
- JavaScript:6 annotation-bridge tests +5 asynchronous-build-state tests; syntax checks for application/bridge/browser scenario script
- Actual loopback HTTP process: authenticated login, full React source upload, real npm install/build, immutable publication, Dioxus review-shell response, scoped CORS preview response, and failing replacement leaving the previous revision live
- Real React/Vite authored fixture built successfully in explicitly enabled trusted-host mode, producing2 static files (~194KB); final aggregate source-build smoke completed in18.56seconds including its target's other tests
- Actual separate worker CLI process reconstructed Firestore/GCS configuration, claimed and built own source, atomically published, and emitted only job ID/state without viewer capability

## What external systems were really exercised

| System | Evidence |
|---|---|
| Firestore | Official local emulator: real REST transactions, CAS, equality query and pagination beyond200 records, rollback, concurrent replies, restart reconstruction, lifecycle/version fencing and atomic job completion |
| Google Cloud Storage | Local fake HTTP object store verifies request shape, immutable generation preconditions, generation-pinned reads and durable-domain behavior; real GCS IAM/service not tested |
| Cloud Run Jobs | Local fake HTTP transport plus real separate worker binary; duplicate/ambiguous dispatch, lease expiration and publication fences tested; real Cloud Run not deployed/tested |
| Slack | Local fake API: verified-email/workspace/account checks, no guessed matches, lifecycle pre-send recheck, count-only DM,429 detail, ambiguous send handling and no blind resend; no live messages |
| Entra | Synthetic identity/token boundary tests, wrong signing algorithm/unsigned-token rejection, fail-closed configuration, signed-session instance portability; live OIDC not exercised |

## Important scenario coverage

Authorization and CSRF; owner/other-publisher separation; viewer capabilities cannot manage; gated HTML/assets/discussion; signed preview ticket scope/expiry/audience; guest-name requirement and explicit unverified label; immutable anchors and original revisions; duplicate/missing quote rematching; replies/resolution; recoverable lifecycle and historical-access revocation; idempotent create/finalize/publish; unsafe paths/duplicate names/ZIP expansion and symlinks; Markdown HTML/URL safety; bounded build logs/output/timeout; source manifest freezing; stale build/lifecycle races; source-build failure preserves old latest; durable notification debounce, relevant recipients, missing identity, crash recovery to ambiguous without resend; durable queue/worker replay; same API through MCP.

## Failed attempts that were diagnosed and corrected

- Initial Dioxus tag/attribute compile incompatibilities: fixed; compile/render tests pass
- Firestore emulator transaction GET issue: switched to documented batchGet transaction API; emulator tests pass
- Initial scrubbed npm environment lacked approved proxy/CA routing: first source-build attempt hit the120-second deadline as intended. Operator-only credential-free proxy/public-CA inputs were added; real fixture and final aggregate now pass without disabling TLS
- Worker initially logged a result containing the viewer capability: replaced with ID/state-only output and verified by separate-process test
- Initial lint warnings: corrected; final Clippy warnings-denied pass

## Blocked / not run

**Browser:** Chromium fails before opening any page with process_singleton_posix.cc `socket() failed: Operation not permitted`. A combined app+Chromium runner was tried with Chromium sandbox explicitly enabled; the app ran and shut down, but browser startup aborted. No security controls or denied paths were bypassed. Desktop/mobile visual QA, actual React counter interaction, rendered selection/margins and browser-proven cookie/CSP isolation are therefore **unrun**, not passed. `tests/browser.cjs` contains these scenarios, including interrupted/repeated actions, and is syntax-checked only. No screenshots are presented as verified output.

**Live services/deployment:** real Entra, real Slack, real GCS, Cloud Run service/Job execution/IAM, container image build, deployment, production egress/resource controls, backup/restore, load/performance and vulnerability audit are unrun. No account setup, credentials, cloud resources or live communications occurred.

## Reproduction

Normal local tests: `cargo test`; bridge tests: `node assets/bridge.test.cjs`; polling tests: `node assets/build-status.test.cjs`; strict lint: `cargo clippy --all-targets -- -D warnings`; format: `cargo fmt --check`.

Full emulator/integration run, with this executor's explicitly approved routing configuration:

```sh
cargo build
ARTIFACT_BUILD_HTTP_PROXY="$HTTP_PROXY" \
ARTIFACT_BUILD_HTTPS_PROXY="$HTTPS_PROXY" \
ARTIFACT_BUILD_CA_FILE="$NODE_EXTRA_CA_CERTS" \
python3 tests/run_firestore_contract.py -- -- --include-ignored
```

`python3 tests/run_http_smoke.py` runs app/client in the same execution namespace. `python3 tests/run_browser.py` is the presently blocked rendered QA stage. The Firestore runner requires the official emulator JAR installed at its documented local path. The source archive intentionally excludes downloaded tools, caches, runtime state and secrets.

## Acceptance interpretation

Static/Markdown/React publication, MCP, discussion, immutable revision/lifecycle and batching have passing service-level tests. Browser-dependent portions of A01/A02/A08/A09/A17/A19 remain unverified. A06 admission and A16 lifecycle require final product-policy review plus live identity testing. A12 container isolation is **not** claimed; trusted-host execution was separately authorized and is clearly labeled. Tentative analytics A18 is not implemented. Passing tests do not close the operational/live-integration gates listed in OPERATIONS.md.

## Acceptance hardening follow-up (18 October 2026)

Rendered browser acceptance is now **skipped at the user's direction**. The supported cloud browser became available, but opening the running loopback app returned `ERR_CONNECTION_REFUSED` across execution namespaces. A narrowly reviewed host-boundary launch failed before starting the app at the filesystem sandbox boundary. No tunnels, public exposure, browser security changes or boundary workarounds were used. This does not establish a rendered desktop/mobile pass.

Source-level fixes include preserving per-thread reply drafts across refresh/filter operations, reconciling a successfully posted reply without requiring a second successful read, rejecting stale discussion refresh results, and restricting annotation matching to the correct artifact document. Deterministic Node regressions exercise these paths; they are simulated DOM tests, not browser tests.

The HTTP and Firestore runners now preserve the caller's Rust environment and support `CARGO`; Firestore supports `ARTIFACT_FIRESTORE_EMULATOR_JAR`. Fresh checkouts no longer depend on the original cloud workspace toolchain path. GitHub source-check CI covers default Rust tests, strict lint, formatting and Node regressions only; ignored integration tests and rendered/live acceptance are excluded explicitly.

Revalidated locally: the full 68-execution Rust aggregate with all ignored tests enabled, real authored React source build/publication and failed replacement over HTTP, Clippy with warnings denied, formatting and Python runner syntax. Existing real-versus-fake service distinctions above are unchanged.

Follow-up JavaScript result: **19 passed, 0 failed** (7 reply/draft/race acceptance regressions, 7 annotation bridge tests, 5 build-state tests). These do not close rendered-browser or live-integration gaps.

The initial acceptance-hardening checkpoint still had a lost-mutation-acknowledgement gap. The subsequent request-recovery checkpoint below supersedes that limitation for keyed comment/reply creation and upload finalization.

## Durable request-recovery checkpoint (18 October 2026)

Implemented actor/share/request-bound durable receipts for comment/reply creation and upload finalization, signed guest initialization before mutation, exact client acknowledgements and pending-request journals. Local preparations use durable expiring ownership claims; Firestore uses transactional receipts and stable build-job acceptance. See [REQUEST-RECOVERY.md](REQUEST-RECOVERY.md) for protocol, retention, expiry and client obligations.

Passed locally against this checkpoint:
- Full Rust aggregate: **91 passed, 0 failed, 0 ignored**, including official Firestore emulator transaction tests, fake GCS and fake Cloud Run dispatch contracts, and authored trusted-host React builds.
- JavaScript: **51 passed, 0 failed**. Includes lost/truncated responses, concurrency, reloads, actor changes, altered payloads, stale/malformed receipts, response aliases and pending-job recovery. These are simulated DOM/source tests.
- Real loopback HTTP smoke: authored full React source install/build/publication, exact finalization retry after an actual server-process restart, SSR shell/content responses, scoped CORS, and failed replacement preserving the live revision.
- Strict Clippy across all targets, formatting, JavaScript/Python syntax and whitespace checks.

Service regressions cover exact original receipts after later replies/revisions, independent concurrent clients, reconstructed stores, actual inserted comment identifiers, cookie tampering/expiry, cross-actor/share isolation, access revocation, source freezing, lifecycle fences, bounded receipt rollback, durable notification effects without duplication, lease expiry/reclaim and stale completion/failure rejection. In-process HTTP tests deliberately discard mutation responses to model a lost acknowledgement; they do not inject real network packet loss.

An initial aggregate exposed Firestore contention beyond the earlier retry budget; bounded jittered retries fixed it and the complete aggregate passed afterward. Review also found unsafe busy-build rejection and malformed terminal acknowledgements; regression coverage now protects both. No known failing check remains at this local checkpoint.

CI is expanded to the same emulator/owned-React aggregate and process-restart HTTP smoke, with a checksum-pinned official emulator and Java 21. Its result must be checked for the published commit separately. Browser acceptance remains skipped at the user's direction. Real Entra, Slack delivery, GCS, Cloud Run/IAM, container deployment, and production operations remain unrun. Persisting notification effects once does not promise exactly-once external Slack delivery.

The first expanded GitHub run passed formatting, lint, default Rust and all 51 JavaScript tests, then exposed a test-fixture portability defect: the explicitly scrubbed worker-process environment omitted `ARTIFACT_NODE_BIN_DIR`. The fixture now forwards only this operator-selected runtime setting while retaining environment scrubbing. The subsequent published CI result is authoritative for that correction.
