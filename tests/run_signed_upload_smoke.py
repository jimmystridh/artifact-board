"""Verify MCP metadata -> curl raw bytes -> MCP publication on a disposable local server."""
import hashlib
import json
import os
from pathlib import Path
import socket
import subprocess
import tempfile
import time
from urllib.request import Request, urlopen

ROOT = Path(__file__).resolve().parents[1]


def main():
    with tempfile.TemporaryDirectory(prefix="artifact-signed-upload-") as work:
        with socket.socket() as listener:
            listener.bind(("127.0.0.1", 0))
            port = listener.getsockname()[1]
        origin = f"http://127.0.0.1:{port}"
        env = {**os.environ, "ARTIFACT_STORE": "local-reference",
               "ARTIFACT_BIND": f"127.0.0.1:{port}", "ARTIFACT_ORIGIN": origin,
               "ARTIFACT_DATA_DIR": work}
        with open(Path(work) / "server.log", "w") as log:
            server = subprocess.Popen([str(ROOT / "target/debug/artifact-board"), "--dev-fixtures"],
                                      cwd=ROOT, env=env, stdout=log, stderr=log)
            try:
                deadline = time.monotonic() + 30
                while True:
                    try:
                        with urlopen(origin + "/health", timeout=1) as response:
                            assert json.load(response)["identity"]["development_fixture"] is True
                        break
                    except OSError:
                        if server.poll() is not None or time.monotonic() >= deadline:
                            raise RuntimeError("Disposable local server did not start") from None
                        time.sleep(.2)
                headers = {"Content-Type": "application/json", "X-Artifact-Request": "1"}
                def post(path, body):
                    with urlopen(Request(origin + path, data=json.dumps(body).encode(),
                                         headers=headers, method="POST"), timeout=30) as response:
                        data = json.load(response)
                        cookie = response.headers.get("Set-Cookie")
                        if cookie:
                            headers["Cookie"] = cookie.split(";", 1)[0]
                        return data
                post("/dev/login", {"identity": "owner"})
                def tool(name, arguments):
                    response = post("/mcp", {"jsonrpc": "2.0", "id": 1,
                        "method": "tools/call", "params": {"name": name, "arguments": arguments}})
                    assert response["result"]["isError"] is False, response
                    return response["result"]["structuredContent"]
                actor = tool("get_identity", {})["mutation_actor"]
                share = tool("create_share", {"title": "Disposable curl test", "kind": "static",
                    "policy": "link-only", "idempotency_key": "curl-smoke-share"})
                upload = tool("create_upload", {"share_id": share["id"]})
                report = Path(work) / "report.html"
                report.write_bytes(b"<!doctype html><h1>Uploaded by curl without agent credentials</h1>")
                data = report.read_bytes()
                intent = tool("create_upload_intent", {"upload_id": upload["id"], "format": "file",
                    "path": "index.html", "size_bytes": len(data), "sha256": hashlib.sha256(data).hexdigest()})
                response = subprocess.run(["curl", "--fail-with-body", "--silent", "--show-error",
                    "--request", "PUT", "--header", "Content-Type: " + intent["headers"]["Content-Type"],
                    "--data-binary", "@" + str(report), intent["upload_url"]], capture_output=True)
                assert response.returncode == 0, "curl signed PUT failed"
                assert json.loads(response.stdout)["state"] == "uploaded"
                with urlopen(Request(intent["upload_url"], data=data,
                                     headers=intent["headers"], method="PUT"), timeout=30) as response:
                    assert json.load(response)["state"] == "uploaded"
                payload = {"upload_id": upload["id"], "entrypoint": "index.html",
                           "idempotency_key": "curl-smoke-finalize", "mutation_actor": actor}
                ready = tool("finalize_upload", payload)
                assert tool("finalize_upload", payload) == ready
                with urlopen(origin + f"/content/{share['capability']}/{ready['revision_id']}/index.html") as response:
                    assert b"Uploaded by curl without agent credentials" in response.read()
                print("PASS: MCP metadata-only intent, curl and Python PUT without cookies or Authorization, publication and exact finalization retry")
            finally:
                server.terminate()
                try:
                    server.wait(timeout=5)
                except subprocess.TimeoutExpired:
                    server.kill()
                    server.wait()


if __name__ == "__main__":
    main()
