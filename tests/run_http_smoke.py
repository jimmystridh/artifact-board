"""Real loopback HTTP service smoke test. Only our source fixtures execute."""
import base64,json,os,pathlib,socket,subprocess,tempfile,time,urllib.request,urllib.error
root=pathlib.Path(__file__).resolve().parents[1];env=os.environ.copy()
subprocess.run([env.get('CARGO', 'cargo'),'build','--locked'],cwd=root,env=env,check=True)
for target,source in [('ARTIFACT_BUILD_HTTP_PROXY','HTTP_PROXY'),('ARTIFACT_BUILD_HTTPS_PROXY','HTTPS_PROXY'),('ARTIFACT_BUILD_CA_FILE','NODE_EXTRA_CA_CERTS')]:
 if source in env:env[target]=env[source]
base='http://127.0.0.1:4319';cookie=''
def request(path,method='GET',data=None,expected=200):
 body=None if data is None else json.dumps(data).encode();r=urllib.request.Request(base+path,data=body,method=method,headers={'Content-Type':'application/json','X-Artifact-Request':'1','Cookie':cookie})
 try:response=urllib.request.urlopen(r,timeout=150)
 except urllib.error.HTTPError as e:response=e
 payload=response.read();assert response.status==expected,(response.status,payload[:2000]);
 try:value=json.loads(payload)
 except:value=payload.decode()
 return value,response.headers
def publish(share,files,entry):
 u,_=request('/api/shares/'+share['id']+'/uploads','POST',{})
 request(u['upload_url'],'PUT',{'files':[{'path':p,'content_base64':base64.b64encode(b).decode()}for p,b in files.items()]})
 return request('/api/uploads/'+u['id']+'/finalize','POST',{'entrypoint':entry})[0]
with tempfile.TemporaryDirectory(prefix='artifact-http-') as data:
 env.update(ARTIFACT_BIND='127.0.0.1:4319',ARTIFACT_DATA_DIR=data,ARTIFACT_BUILD_MODE='trusted-host')
 app=subprocess.Popen([str(root/'target/debug/artifact-board'),'--dev-fixtures'],cwd=root,env=env)
 try:
  for i in range(100):
   try:
    with socket.create_connection(('127.0.0.1',4319),.1):break
   except OSError:time.sleep(.1)
  _,h=request('/dev/login','POST',{'identity':'owner'});cookie=h['Set-Cookie'].split(';')[0]
  share,_=request('/api/shares','POST',{'title':'Real React HTTP fixture','kind':'react','policy':'link-only','idempotency_key':'http-react'})
  fixture=root/'tests/fixtures/react';files={str(p.relative_to(fixture)):p.read_bytes()for p in fixture.rglob('*')if p.is_file()}
  result=publish(share,files,'package.json');html,_=request(result['viewer_url']);assert 'artifact-frame' in html
  before,_=request('/api/shares/'+share['id']);assert before['latest_revision']==result['revision_id']
  import re
  path=re.search(r'src="([^"]*/frame/[^"]+)"',html)
  if not path:path=re.search(r'src="(/frame/[^"]+)"',html)
  assert path,'SSR preview path missing'
  preview,h=request(path.group(1).replace('&amp;','&'));assert 'type="module"' in preview;assert h['Access-Control-Allow-Origin']=='null';assert 'allow-same-origin' not in h['Content-Security-Policy']
  # A failing replacement must preserve the currently published artifact.
  broken=dict(files);pkg=json.loads(broken['package.json']);pkg['scripts']['build']='node -e "process.exit(17)"';broken['package.json']=json.dumps(pkg).encode()
  u,_=request('/api/shares/'+share['id']+'/uploads','POST',{});request(u['upload_url'],'PUT',{'files':[{'path':p,'content_base64':base64.b64encode(b).decode()}for p,b in broken.items()]});failure,_=request('/api/uploads/'+u['id']+'/finalize','POST',{'entrypoint':'package.json'},422);after,_=request('/api/shares/'+share['id']);assert after['latest_revision']==before['latest_revision']
  print('PASS: real HTTP login, full React source upload/build/publication, SSR shell, scoped CORS preview, and failed replacement preserves live revision. Browser rendering remains unrun.')
 finally:
  app.terminate();app.wait(timeout=10)
