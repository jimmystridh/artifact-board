"""Deploy the fixed Artifact Board pilot; run with uv after budget approval."""
import argparse
import json
from pathlib import Path
import secrets
import subprocess
import time

PROJECT = 'dev-innov-cm'
REGION = 'europe-north2'
DATABASE = 'artifact-board-pilot'
BUCKET = 'dev-innov-cm-artifact-board-pilot'
ROOT = Path(__file__).resolve().parents[1]
EVIDENCE = ROOT / '.local/desktop-evidence'


def command(args, *, data=None, missing_ok=False, attempt=0):
    result = subprocess.run(
        ['gcloud', *args, '--project', PROJECT, '--quiet'],
        input=data, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
    )
    if result.returncode:
        message = result.stderr.decode()
        if 'add-iam-policy-binding' in args and '412' in message and attempt < 5:
            time.sleep(2)
            return command(args, data=data, missing_ok=missing_ok, attempt=attempt + 1)
        if 'add-iam-policy-binding' in args and 'Service account' in message and 'does not exist' in message and attempt < 5:
            time.sleep(10)
            return command(args, data=data, missing_ok=missing_ok, attempt=attempt + 1)
        if missing_ok and ('NOT_FOUND' in message or 'does not exist' in message or 'not found: 404' in message):
            return None
        raise RuntimeError(message)
    return result.stdout


def account(name):
    return f'{name}@{PROJECT}.iam.gserviceaccount.com'


def provision():
    if command(['firestore', 'databases', 'describe', '--database', DATABASE, '--format=json'], missing_ok=True) is None:
        command(['firestore', 'databases', 'create', '--database', DATABASE, '--location', REGION, '--type=firestore-native', '--edition=standard', '--delete-protection'])
    database = json.loads(command(['firestore', 'databases', 'describe', '--database', DATABASE, '--format=json']))
    if database['locationId'] != REGION or database['type'] != 'FIRESTORE_NATIVE':
        raise RuntimeError('Existing database does not match the approved region/type')
    bucket = command(['storage', 'buckets', 'describe', 'gs://' + BUCKET, '--format=json'], missing_ok=True)
    if bucket is None:
        command(['storage', 'buckets', 'create', 'gs://' + BUCKET, '--location', REGION, '--uniform-bucket-level-access', '--public-access-prevention'])
    else:
        bucket = json.loads(bucket)
        if bucket.get('location', '').lower() != REGION:
            raise RuntimeError('Existing bucket has a different location')
        if bucket.get('public_access_prevention') != 'enforced' or bucket.get('uniform_bucket_level_access') is not True:
            raise RuntimeError('Existing bucket does not enforce private uniform access')
    for name in ('artifact-board-api', 'artifact-board-build', 'artifact-board-notify'):
        if command(['iam', 'service-accounts', 'describe', account(name), '--format=json'], missing_ok=True) is None:
            command(['iam', 'service-accounts', 'create', name, '--display-name', name])
        command(['projects', 'add-iam-policy-binding', PROJECT,
                 '--member=serviceAccount:' + account(name), '--role=roles/datastore.user',
                 '--condition=title=artifact-board-database,expression=resource.name=="projects/' + PROJECT + '/databases/' + DATABASE + '"', '--format=none'])
    for name in ('artifact-board-api', 'artifact-board-build'):
        for role in ('roles/storage.objectCreator', 'roles/storage.objectViewer'):
            command(['storage', 'buckets', 'add-iam-policy-binding', 'gs://' + BUCKET,
                     '--member=serviceAccount:' + account(name), '--role=' + role, '--format=none'])
    if command(['secrets', 'describe', 'artifact-board-session-key', '--format=json'], missing_ok=True) is None:
        command(['secrets', 'create', 'artifact-board-session-key', '--replication-policy=user-managed',
                 '--locations', REGION, '--data-file=-', '--labels=app=artifact-board,environment=pilot'],
                data=secrets.token_urlsafe(64).encode())
    for secret in ('artifact-board-session-key', 'mcp-gateway-entra-client-secret'):
        state = command(['secrets', 'versions', 'describe', '1', '--secret', secret, '--format=value(state)']).decode().strip()
        if state != 'ENABLED':
            raise RuntimeError('Required pinned secret version is not enabled: ' + secret)
        command(['secrets', 'add-iam-policy-binding', secret, '--member=serviceAccount:' + account('artifact-board-api'),
                 '--role=roles/secretmanager.secretAccessor', '--condition=None', '--format=none'])
    exclusion = json.loads((ROOT / 'deploy/logging-exclusion.json').read_text())
    sink = json.loads(command(['logging', 'sinks', 'describe', '_Default', '--format=json']))
    current = next((item for item in sink.get('exclusions', []) if item['name'] == exclusion['name']), None)
    if current is None:
        value = 'name=' + exclusion['name'] + ',filter=' + exclusion['filter'] + ',description=' + exclusion['description']
        command(['logging', 'sinks', 'update', '_Default', '--add-exclusion=' + value])
    elif current.get('filter') != exclusion['filter'] or current.get('disabled', False):
        raise RuntimeError('Artifact Board request-log exclusion differs from the reviewed configuration')
    print('Provisioned private storage, database-scoped IAM and API-only secret access', flush=True)


def deploy(image):
    for filename in ('build-job.json', 'notify-job.json', 'service.json'):
        path = ROOT / 'deploy' / filename
        document = json.loads(path.read_text())
        if document['kind'] == 'Service':
            container = document['spec']['template']['spec']['containers'][0]
        else:
            container = document['spec']['template']['spec']['template']['spec']['containers'][0]
        container['image'] = image
        path.write_text(json.dumps(document, indent=2) + '\n')
    for filename in ('build-job.json', 'notify-job.json'):
        command(['run', 'jobs', 'replace', str(ROOT / 'deploy' / filename), '--region', REGION])
    command(['run', 'jobs', 'add-iam-policy-binding', 'artifact-board-build', '--region', REGION,
             '--member=serviceAccount:' + account('artifact-board-api'),
             '--role=roles/run.jobsExecutorWithOverrides', '--format=none'])
    service_path = str(ROOT / 'deploy/service.json')
    command(['run', 'services', 'replace', service_path, '--region', REGION, '--dry-run'])
    command(['run', 'services', 'replace', service_path, '--region', REGION])
    command(['run', 'services', 'add-iam-policy-binding', 'artifact-board-pilot', '--region', REGION,
             '--member=allUsers', '--role=roles/run.invoker', '--format=none'])
    EVIDENCE.mkdir(parents=True, exist_ok=True)
    service = command(['run', 'services', 'describe', 'artifact-board-pilot', '--region', REGION, '--format=json'])
    (EVIDENCE / 'deployed-service.json').write_bytes(service)
    for job in ('artifact-board-build', 'artifact-board-notify'):
        payload = command(['run', 'jobs', 'describe', job, '--region', REGION, '--format=json'])
        (EVIDENCE / (job + '.json')).write_bytes(payload)
    print('Deployed service and worker jobs with pinned image:', image, flush=True)


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument('--image', required=True)
    parser.add_argument('--approved-budget-usd', required=True, type=int)
    args = parser.parse_args()
    prefix = REGION + '-docker.pkg.dev/' + PROJECT + '/cloud-run-source-deploy/artifact-board-pilot@sha256:'
    digest = args.image.removeprefix(prefix)
    if not args.image.startswith(prefix) or len(digest) != 64 or any(c not in '0123456789abcdef' for c in digest):
        parser.error('A validated immutable image digest in the approved registry is required')
    if args.approved_budget_usd <= 0:
        parser.error('An explicitly approved positive pilot budget is required')
    provision()
    deploy(args.image)
    (EVIDENCE / 'approved-budget.json').write_text(json.dumps({
        'monthly_pilot_budget_usd': args.approved_budget_usd,
        'hard_spending_cap': False,
        'automatic_billing_alerts': 'not configured; Billing Budgets API currently unavailable',
        'api_min_instances': 0, 'api_max_instances': 1,
        'slack_delivery': False, 'worker_max_retries': 0,
    }, indent=2) + '\n')


if __name__ == '__main__':
    main()
