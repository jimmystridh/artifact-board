"""Run our loopback app and Chromium in the same execution namespace; no sandbox overrides."""
import os, subprocess, pathlib, socket, time, tempfile
root=pathlib.Path(__file__).resolve().parents[1]
env=os.environ.copy(); env['CARGO_HOME']='/workspace/shared/wipstash-toolchain/cargo';env['RUSTUP_HOME']='/workspace/shared/wipstash-toolchain/rustup'
cargo=env['CARGO_HOME']+'/bin/cargo'
subprocess.run([cargo,'build'],cwd=root,env=env,check=True)
(root/'.local/qa').mkdir(parents=True,exist_ok=True)
with tempfile.TemporaryDirectory(prefix='artifact-board-browser-') as directory:
 env['ARTIFACT_DATA_DIR']=directory;env['ARTIFACT_BIND']='127.0.0.1:4317';env['ARTIFACT_BUILD_MODE']='trusted-host'
 for target,source in [('ARTIFACT_BUILD_HTTP_PROXY','HTTP_PROXY'),('ARTIFACT_BUILD_HTTPS_PROXY','HTTPS_PROXY'),('ARTIFACT_BUILD_CA_FILE','NODE_EXTRA_CA_CERTS')]:
  if source in env:env[target]=env[source]
 app=subprocess.Popen([str(root/'target/debug/artifact-board'),'--dev-fixtures'],cwd=root,env=env)
 try:
  for i in range(100):
   if app.poll() is not None:raise RuntimeError('Application stopped before readiness')
   try:
    with socket.create_connection(('127.0.0.1',4317),.1):break
   except OSError:time.sleep(.1)
  subprocess.run(['node',str(root/'tests/browser.cjs')],cwd=root,env=env,check=True,timeout=150)
 finally:
  app.terminate()
  try:app.wait(timeout=10)
  except subprocess.TimeoutExpired:raise RuntimeError('Owned application did not stop gracefully')
