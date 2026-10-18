# Prototype ingestion and build boundary

The upload limits below are conservative local implementation defaults, not approved public product contracts: 2,048 files/ZIP entries, 16 MiB per expanded file, 64 MiB aggregate expanded content, 32 MiB compressed ZIP, and 200:1 maximum expansion for members larger than 1 MiB. Relative UTF-8 paths only; no traversal, percent escapes, control characters, backslashes, absolute/drive paths, ambiguous case aliases, file/directory conflicts, symlinks, or special files. ZIP CRC/read failures reject the upload. Direct JSON request decoding must reject duplicate object keys before converting to BTreeMap; map conversion cannot preserve evidence of duplicates.

Markdown dialect: CommonMark plus tables, strikethrough, task lists. Raw HTML is escaped. Links/images allow relative paths and http/https/mailto; other schemes and protocol-relative URLs are disabled. Local references resolve relative to the source Markdown directory; generated index.html stays there. The generated entry never overwrites an existing asset. Uploaded assets still require the application's isolated content-serving boundary.

## React source compatibility target

Full package.json at the upload root, nonempty scripts.build, npm, `npm run build`, static `dist/index.html`. A package-lock.json/npm-shrinkwrap.json is preferred; missing lockfiles produce a warning. No arbitrary output directories, SSR services, or standalone JSX contract. A future worker should pin Node 24.19.0 with an image digest and the npm version recorded in its build result. Uploaded node_modules is rejected.

## Verified local blocker

On 18 October 2026 Docker and Podman executables/sockets were absent. All effective/permitted/bounding Linux capabilities were zero. An unprivileged user/mount/network/PID namespace smoke test succeeded, but cgroup controls were not writable and bubblewrap failed to create its network setup socket. This is insufficient evidence for the required resource-bounded container build boundary. Production `prepare` therefore uses an unavailable adapter and fails closed with a useful error. It NEVER invokes npm, shell, Docker, or uploaded scripts on the host. Tests use a fake adapter returning inert fixture bytes; they do not establish real build execution or timeout acceptance.

## Future worker design, not enabled

A separate container-worker deployment is needed. Do not merely install Docker or point this API at a privileged runtime socket. Review worker authentication, queue ownership, dependency retrieval/network policy, runtime/image supply chain, cgroup support, disk quotas, cleanup, and cancellation before enabling.

Illustrative offline build command (not executed; variables supplied by the trusted worker):

```
docker run --name "$JOB_CONTAINER" --rm --network none \
  --user 1000:1000 --read-only --cap-drop ALL \
  --security-opt no-new-privileges --pids-limit 128 \
  --memory 512m --memory-swap 512m --cpus 1 \
  --ulimit nofile=1024:1024 --ulimit fsize=67108864:67108864 \
  --mount type=bind,src="$VALIDATED_SOURCE",dst=/input,readonly \
  --tmpfs /work:rw,nosuid,nodev,size=268435456,uid=1000,gid=1000 \
  --tmpfs /tmp:rw,nosuid,nodev,noexec,size=33554432,uid=1000,gid=1000 \
  --env HOME=/tmp --env npm_config_cache=/tmp/npm \
  --env PATH=/usr/local/bin:/usr/bin:/bin \
  --workdir /work "$PINNED_IMAGE_DIGEST" \
  /usr/local/bin/artifact-build-offline
```

The trusted image entrypoint copies input into /work, uses `npm ci --offline` if locked or `npm install --offline` otherwise, then `npm run build`. Offline mode requires a separately reviewed dependency provisioning mechanism; it does not provide general package compatibility today. Package lifecycle/install scripts execute only inside the same constrained worker. Container must have no runtime socket, host home, credentials, host namespaces, inherited application environment, or writable host mounts. The adapter launches the CLI with an explicit scrubbed environment and fixed arguments, never user-provided shell text.

The worker must enforce a 120-second proposed wall-clock deadline, including install and build; on expiry explicitly kill and remove the named container. Killing just the Docker client does not stop a container. Use a trusted supervisor that tears down on timeout, success, failure, cancellation and API crash recovery. Capture logs to a bounded 1 MiB ring buffer, sanitize terminal controls, and avoid exposing secrets. Enforce total workspace/tmpfs and output quotas, including open/unlinked files. Retrieve dist through a bounded tar stream before cleanup with identical path/link/size checks; do not follow links or blindly extract docker cp output. Validate dist/index.html and atomically promote only successful validated output. Resource settings must be tested on the actual runtime, not assumed from CLI success.

Future acceptance tests: successful real React fixture build; absent host credential sentinel; denied internal/metadata and external egress; PID/memory/tmp/output quota violations; deadline/cancellation teardown; symlink output rejection; failing build preserves prior published revision. These remain blocked until a supported worker exists.

## Explicitly authorized configurable trusted-host prototype mode

The user subsequently authorized building without sandboxing as a configurable option. The server can call `prepare_with_mode(..., "trusted-host")` using only its operator configuration (`ARTIFACT_BUILD_MODE=trusted-host`); default `prepare` remains container/fail-closed. Never bind this to a requester field or let uploaders enable it. `ARTIFACT_NODE_BIN_DIR` selects the operator-controlled absolute Node/npm bin directory (local default is the available Node 24.19.0 runtime).

This mode executes arbitrary uploaded install/build code as the server user on the host. It IS NOT ISOLATION. Scrubbing environment and using a private disposable working directory does not prevent reading host files, network access, credential discovery elsewhere, process-group escape, filesystem damage, or memory/disk exhaustion. Host-wide CPU, memory, process, and temporary-disk quotas are not provided by this mode. It is appropriate only for a local trusted-source demonstration, never untrusted uploads or public deployment. Container acceptance requirements remain unmet.

Implemented best-effort bounds: one concurrent build in this process (reject while busy), shared 120-second install/build/output wall-clock deadline, 1 MiB combined captured logs (excess drained/discarded), 2,048 output files, 16 MiB per output file, 64 MiB total output, maximum output directory depth 64, no output symlinks/special files. Only the newly spawned npm process group is terminated on normal completion/failure/timeout. Malicious processes can escape this group; no claim of hostile-source teardown. Temporary workspace is removed on ordinary terminal paths. A process crash can leave workspace/processes and requires operator cleanup. The subprocess environment is explicitly cleared, with only PATH, HOME, TMPDIR, CI, npm cache/userconfig/registry values provided. No inherited host secrets are intentionally passed.

Command contract: npm ci --no-audit --no-fund when locked, otherwise npm install --no-audit --no-fund, then npm run build. Reputable npm registry default is https://registry.npmjs.org. Uploaded project configuration/scripts can still change behavior and are trusted executable code.

## Production target: Cloud Run Jobs, Firestore and GCS

The later user-selected deployment target is Cloud Run with Firestore durable state and GCS artifacts. The Docker command above illustrates controls only; production must not assume Docker-in-Docker or a container socket on Cloud Run. A Cloud Run Job worker is the intended production boundary. The API queues a durable Firestore job and creates a bounded Job execution; the Job reads one validated GCS input manifest and writes immutable, validated output objects to a job-scoped GCS prefix. Promotion requires a compare-and-set/transaction against the expected revision and still-valid share lifecycle state. A crash/timeout must not publish incomplete output.

The coordinator and REST adapter are implemented but not deployed or tested against real Cloud Run. Required deployment review: job identity and narrowly scoped storage access (no API/Slack/identity secrets), job timeout/memory/CPU/task/concurrency settings, egress and dependency-fetch policy, input/output integrity and quotas, bounded Cloud Logging without sensitive source leakage, cleanup/retention, cancellation, and idempotent Firestore claims/completion. Cloud-provided metadata credentials are a critical consideration: executing arbitrary source in the same identity-bearing job may expose its workload identity. Separate untrusted build execution from privileged artifact transfer and avoid describing ordinary Cloud Run Job identity as credential-free. Local filesystem remains temporary scratch only in this production design. No production credentials or cloud resources have been created by these changes.

Optional operator-only proxy routing uses ARTIFACT_BUILD_HTTP_PROXY and ARTIFACT_BUILD_HTTPS_PROXY. Values must parse as HTTP(S) URLs with a host and no username/password. Only the corresponding explicit proxy/npm proxy keys enter the scrubbed child environment. This preserves an existing approved network proxy without copying other host environment values or relaxing its network policy. Do not put authenticated proxy credentials in these settings.

Where the existing approved network proxy uses a private CA, operator-only ARTIFACT_BUILD_CA_FILE passes its public PEM certificate file to Node as NODE_EXTRA_CA_CERTS. Files with private-key markers or over 1 MiB are rejected. TLS verification stays enabled. Local test environment example uses the already configured proxy and CA, without exposing their values: ARTIFACT_BUILD_HTTP_PROXY="$HTTP_PROXY" ARTIFACT_BUILD_HTTPS_PROXY="$HTTPS_PROXY" ARTIFACT_BUILD_CA_FILE="$NODE_EXTRA_CA_CERTS". Without proxy routing, the test hit DNS failure and terminated at 120.02 seconds as designed; without the existing CA, Node correctly rejected the TLS certificate.

## Local validation result (18 October 2026)

11 regular ingestion tests pass: unsafe paths/case/file-directory conflicts, expanded file limit, ZIP traversal/symlink/expansion checks, static entry validation, escaped Markdown/unsafe schemes/local image base, overwrite prevention, React contract and missing-lock warning, fail-closed default, output symlink rejection, and scoped owned-process timeout with scrubbed environment. A separately enabled real own-source React 19.2.0/Vite 7.1.12 smoke test passed in 22.68 seconds, producing 2 static dist files totaling 194,400 bytes. This validates the explicitly authorized trusted-host path, not container isolation or production Cloud Run execution. The smoke test is ignored by default to avoid silently executing/installing source during routine tests. Run it explicitly with `cargo test --test ingestion own_react_fixture_builds_on_trusted_host -- --ignored --nocapture` and the operator proxy/CA settings where needed.

## Durable Cloud Run coordinator implemented, deployment unverified

src/build_jobs.rs builds and validates Cloud Run v2 jobs.run requests. Its explicit dispatcher uses runtime identity, a 15-second HTTP deadline, no redirects, and a 64 KiB response bound; local fake endpoints never receive runtime credentials. No real Cloud Run request has been made during local validation. It constrains resource identifiers, pins input GCS generation, scopes output to builds/{job_id}/, specifies one task and a proposed 120-second maximum, and passes only job/input/output/revision identifiers. The selected Job must already be configured by an operator. Run overrides merge with the template environment; they do not scrub inherited template secrets. CPU/memory/retry/network/service-account policy must be reviewed on that template. No source-defined image, command, arguments, cloud identity, or output destination is accepted.

Google documents that jobs.run creates an Execution and returns a long-running Operation; overrides require run.jobs.runWithOverrides. API acceptance is not successful build completion. [Cloud Run jobs.run REST reference](https://docs.cloud.google.com/run/docs/reference/rest/v2/projects.locations.jobs/run).

Implemented durable flow: enqueue atomically freezes the staged upload and creates one job keyed by upload ID. Dispatcher CAS claims a queued job before launching and persists its operation or ambiguous outcome. It never automatically repeats an ambiguous launch. Worker CAS claims queued/dispatching state once, verifies the pinned manifest digest, retrieves GCS source, runs the scrubbed source subprocess, and invokes publish_for_job. The domain transaction fences promotion on job claim, unexpired lease, upload association, expected share version/latest, and lifecycle; it publishes immutable output and marks job succeeded atomically. Failed/timed-out builds retain the old latest revision. Failure diagnostics are limited to 16,384 characters and control characters are stripped. Owner-only status omits internal claim identifiers.

reconcile_expired marks overdue running/dispatching jobs timed_out without rerunning source; later output cannot pass the domain claim fence. Production still needs a durable dispatcher/reconciliation trigger (for example a reviewed scheduler), rather than an in-request task or sleeping scale-to-zero API instance. The API/CLI wiring is owned by the transport layer. Cloud Run Job templates must explicitly supply worker mode/backend configuration, fixed image digest, task resource/network policy and narrowly scoped identity. This code neither creates Job templates nor grants IAM permissions. No cloud credentials/resources have been provisioned.

The deployment isolation caveat remains: trusted-host within a separate Cloud Run Job means the job is the execution boundary, but uploaded code may reach the Job's workload identity and filesystem. Environment scrubbing alone does not make it credential-free. Production should restrict trusted publishers and separately review the privileged transfer/build identity design before activation.

Additional validation: missing dist/index.html and oversized fake output are rejected; two-million-byte failed-process log output is capped; full 14-test ingestion suite including a real relative-base React build passes. Exported local browser fixture has 2 dist files totaling 194,401 bytes. Projects must use relative built asset URLs (Vite --base=./ in the fixture) for nested revision routes. Browser runtime hydration still requires separate viewer-origin/CORS verification.

## Durable worker validation result

The full durable coordinator contract passed against an actual local Firestore emulator, explicitly fake in-memory GCS HTTP API, and explicitly fake Cloud Run HTTP API. The final test invokes the actual Artifact Board CLI in a separate process with a cleared environment and explicit emulator/backend/job settings. It reconstructs state, claims once, runs our synthetic npm build, and atomically publishes. Its stdout is checked not to contain the viewer bearer capability. This validates process separation and durable local contracts; it does not claim a real Google Cloud deployment.

Assertions cover duplicate enqueue/dispatch and worker replay, source freezing, runtime credentials not forwarded to fake endpoints, preserved latest revision on build failure, lifecycle/version fencing after unpublish, ambiguous dispatch without automatic retry, expired lease fencing, and denied outsider status access. Final separate-process run: 2 tests passed in 2.79 seconds. Three pure Cloud Run request-validator tests also pass. Authenticated job status polling reconciles expired worker claims, while a durable proactive reconciler can call the same internal method when separately configured.
