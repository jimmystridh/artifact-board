# Publishing from an agent

MCP handles identity, upload intents and publication. Send the actual file using
an ordinary HTTP PUT from curl or Python. File bytes never need to enter the MCP
tool arguments or model context.

1. Call `get_identity` and retain its `mutation_actor`.
2. Call `create_share` with title, kind (`static`, `markdown`, or `react`),
   viewer policy and a stable idempotency key. For a new revision, reuse the
   existing `share_id` instead.
3. Call `create_upload` with `share_id`. Retain the returned `id` as `upload_id`.
4. Compute the exact file's byte count and SHA-256 locally, then call
   `create_upload_intent` with metadata only:

   ```json
   {
     "upload_id": "<id from create_upload>",
     "format": "file",
     "path": "index.html",
     "size_bytes": 12345,
     "sha256": "<64-character SHA-256 hex digest of the local file>"
   }
   ```

   `size_bytes` above is illustrative; use the measured value. `path` is the
   artifact-relative filename for a raw file. Use `format: "zip"` and a ZIP
   filename for a directory bundle; archive entries retain their relative paths.
   Raw files are limited to 16 MiB and ZIP uploads to 32 MiB; existing extraction
   and artifact quotas still apply. Compute metadata after creating the ZIP.

   ```sh
   wc -c < report.html
   shasum -a 256 report.html
   ```

5. The tool returns `upload_id`, `upload_url`, `method: "PUT"`, `headers`,
   `expires_at`, `size_bytes` and `sha256`. PUT the unmodified bytes using the
   returned Content-Type:

   ```sh
   curl --fail-with-body --request PUT \
     --header 'Content-Type: application/octet-stream' \
     --data-binary @report.html "$UPLOAD_URL"
   ```

   For ZIPs use `Content-Type: application/zip` and `--data-binary @artifact.zip`.
   No Authorization header, cookie or MCP access token is needed for this PUT.
   Keep the signed URL private: it is a scoped upload credential, valid for at
   most ten minutes and bounded by the upload session's expiry.

   The equivalent Python request, where `intent` is the tool response:

   ```python
   from pathlib import Path
   from urllib.request import Request, urlopen

   request = Request(
       intent["upload_url"],
       data=Path("report.html").read_bytes(),
       headers=intent["headers"],
       method=intent["method"],
   )
   with urlopen(request, timeout=120) as response:
       assert response.status == 200
   ```

6. Call `finalize_upload` with the original `upload_id`, artifact-relative
   `entrypoint`, a stable `idempotency_key`, and the original `mutation_actor`.
   Finalization requires the authenticated MCP client. For React, poll
   `get_build`; queued or running does not mean published. Return the viewer URL
   only after publication succeeds.

A single-file upload replaces that upload session's staged file set. Bundle
multiple files and local assets in one ZIP, with the entrypoint at its intended
relative path. Each revision uses a new upload session.

Retries of the signed PUT accept only identical size and SHA-256. After an
expired URL, request a new intent for the same upload session and same metadata;
if the session itself expired, start a new upload. Once finalization begins,
staged content is frozen and PUT retries are rejected. Retry finalization with
the same upload ID, key, entrypoint and mutation actor to recover its receipt.
The signed URL cannot publish, read private content, or manage shares.

Authenticated API clients can also request an intent with
`POST /api/uploads/{upload_id}/intent`, using the same metadata fields except
`upload_id` in the JSON body. Existing authentication and mutation-header rules
apply to intent creation and finalization. The signed PUT is the only step that
requires no publisher session. The URL targets Artifact Board's API, which
validates the body and stages it in the existing private storage; it is not a
GCS URL. Legacy `upload_files` remains available for existing clients.
