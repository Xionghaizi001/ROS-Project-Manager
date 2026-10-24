#!/usr/bin/env python3
"""Real managed-Conda replay in disposable Docker containers.

Requires Docker, network access and a rospm binary matching --platform. Installs
ROS only inside containers. Keeps evidence in a NEW --root; removes only its
own containers. Example: sudo python3 scripts/p35-managed-conda-check.py
--root /tmp/p35-managed --rospm target/debug/rospm
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import shlex
import shutil
import subprocess

p = argparse.ArgumentParser(description=__doc__)
p.add_argument('--root', required=True)
p.add_argument('--rospm', default='target/debug/rospm')
p.add_argument('--platform', default='amd64', choices=['amd64', 'arm64'])
p.add_argument('--base-image', help='Prepared Ubuntu 24.04 image; defaults to rospm-p3:<platform>')
p.add_argument('--mirror', default='tuna')
a = p.parse_args()
a.base_image = a.base_image or 'rospm-p3:' + a.platform
root = Path(a.root).resolve()
root.mkdir(parents=True, exist_ok=False)
repo = Path(__file__).resolve().parent.parent
binary = Path(a.rospm).resolve()
shutil.copytree(repo / 'scripts/p35/fixture', root / 'fixture')
shutil.copy2(binary, root / 'rospm')
shutil.copy2(Path(__file__), root / 'suite.py')
results = []
containers = []
image = f'rospm-p35-prepared:{os.getpid()}'


def run(label, cmd, ok=True):
    print('RUN', label, flush=True)
    with (root / (label + '.log')).open('w') as log:
        log.write('$ ' + shlex.join(cmd) + '\n')
        log.flush()
        proc = subprocess.run(cmd, stdout=log, stderr=subprocess.STDOUT, timeout=3600)
    output = (root / (label + '.log')).read_text()
    assert (proc.returncode == 0) == ok, (label, proc.returncode, output[-6000:])
    results.append({'case': label, 'exit': proc.returncode, 'passed': True})
    (root / 'results.json').write_text(json.dumps(results, indent=2) + '\n')
    return output


def guest(name, label, script, ok=True):
    return run(label, ['docker', 'exec', '-u', 'tester', '-e', 'ROSPM_LANG=en',
                       '-e', 'ROS_DOMAIN_ID=86', '-e', 'ROS_LOCALHOST_ONLY=1',
                       '-w', '/home/tester/project', name,
                       'bash', '--noprofile', '--norc', '-ec', script], ok)


def container(suffix, base):
    name = f'rospm-p35-{os.getpid()}-{suffix}'
    run(suffix + '-container', ['docker', 'run', '-d', '--name', name,
        '--platform', 'linux/' + a.platform, '-v', f'{root}:/evidence',
        '-v', f'{root}/rospm:/usr/local/bin/rospm:ro', base, 'sleep', 'infinity'])
    containers.append(name)
    run(suffix + '-permissions', ['docker', 'exec', name, 'bash', '-ec',
        'mkdir -p /home/tester/project; chown -R tester:tester /home/tester/project; chmod 777 /evidence'])
    return name


cli = 'rospm --manifest /home/tester/project/rospm.toml '
apply = cli + 'apply --mirror ' + shlex.quote(a.mirror)
# Persist source revision and binary provenance, independent of local edits later.
run('revision', ['git', '-C', str(repo), 'rev-parse', 'HEAD'])
(root / 'binary.sha256').write_text(hashlib.sha256((root / 'rospm').read_bytes()).hexdigest() + '\n')
try:
    seed = container('seed', a.base_image)
    if a.mirror == 'tuna':
        run('ubuntu-mirror', ['docker', 'exec', seed, 'bash', '-ec',
            "sed -i -E 's@http://(archive|security).ubuntu.com/ubuntu/?@https://mirrors.tuna.tsinghua.edu.cn/ubuntu/@g' /etc/apt/sources.list.d/ubuntu.sources"])
    guest(seed, 'prepare-source', '''
cp /evidence/fixture/{rospm.toml,environment.yml} .
mkdir -p /home/tester/upstream
cp -R /evidence/fixture/p35_probe/. /home/tester/upstream/
git -C /home/tester/upstream init -q
git -C /home/tester/upstream add .
git -C /home/tester/upstream diff --cached --quiet || git -C /home/tester/upstream -c user.name=P35 -c user.email=test@example.com commit -qm fixture
printf 'repositories:\n  p35_probe:\n    type: git\n    url: /home/tester/upstream\n    version: HEAD\n' > main.repos
# Prepare only ROS host prerequisites; no Conda, project environment or source checkout.
sed '/^\[build\]/,$d' rospm.toml > native.toml
sed -i 's/sources = "main.repos"/sources = "empty.repos"/' native.toml
printf 'repositories: {}\n' > empty.repos
''')
    guest(seed, 'prepare-ros', 'rospm --manifest native.toml apply --mirror ' + shlex.quote(a.mirror))
    guest(seed, 'clean-baseline', '''
if command -v conda; then exit 1; fi
test ! -e /home/tester/.local/share/rospm/conda
test ! -e .rospm/conda/env
test ! -e ws/src/p35_probe
sha256sum ~/.bashrc ~/.profile > /evidence/shell-before.sha256
''')
    run('prepared-image', ['docker', 'commit', seed, image])
    guest(seed, 'seed-apply-build', apply + ' --build')
    guest(seed, 'seed-lock', cli + 'lock')
    guest(seed, 'export-lock', '''
cp rospm.lock conda-*.lock /evidence/
git -C ws/src/p35_probe rev-parse HEAD > /evidence/source.rev
''')
    # Each replay gets a separate filesystem, empty package cache and no Conda.
    for suffix in ['replay-a', 'replay-b']:
        target = container(suffix, image)
        guest(target, suffix + '-input', '''
if command -v conda; then exit 1; fi
test ! -e ~/.local/share/rospm/conda
test ! -e .rospm/conda/env
test ! -e ws/src/p35_probe
cp /evidence/rospm.lock /evidence/conda-*.lock .
sha256sum rospm.lock conda-*.lock > /tmp/locks-before.sha256
''')
        if suffix == 'replay-a':
            guest(target, 'tamper-lock', "printf '\\n# tampered\\n' >> conda-*.lock")
            error = guest(target, 'reject-digest', apply + ' --locked --dry-run', ok=False)
            assert 'digest' in error, error
            guest(target, 'remove-record', '''
cp /evidence/conda-*.lock .
python3 - <<'INNER'
from pathlib import Path
import re
p=Path('rospm.lock')
p.write_text(re.sub(r'(?ms)^\\[\\[conda\\]\\].*?(?=^\\[|\\Z)', '', p.read_text()))
INNER
''')
            error = guest(target, 'reject-missing-record', apply + ' --locked --dry-run', ok=False)
            assert 'Conda' in error and 'lock' in error, error
            guest(target, 'restore-record', '''
test ! -e ~/.local/share/rospm/conda
test ! -e .rospm/conda/env
cp /evidence/rospm.lock .
''')
        else:
            # Reproduce the on-disk state left by a killed installer. The real
            # installer must discard this incomplete prefix and finish normally.
            subdir = 'linux-64' if a.platform == 'amd64' else 'linux-aarch64'
            table = (repo / 'crates/rospm-core/data/miniforge.toml').read_text()
            version = re.search(r'^version = "([^"]+)"', table, re.M).group(1)
            prefix = f'/home/tester/.local/share/rospm/conda/miniforge3-{version}-{subdir}'
            guest(target, 'interrupted-prefix', f'mkdir -p {prefix}; touch {prefix}/interrupted-sentinel')
        output = guest(target, suffix + '-one-command', apply + ' --locked --build')
        assert 'P35_MANAGED_OK' in output, suffix
        if suffix == 'replay-b':
            guest(target, 'retry-discarded-partial-prefix', f'test ! -e {prefix}/interrupted-sentinel; test -f {prefix}.ready')
        guest(target, suffix + '-verify', cli + 'verify')
        guest(target, suffix + '-integrity', '''
sha256sum -c /evidence/shell-before.sha256
sha256sum -c /tmp/locks-before.sha256
test "$(git -C ws/src/p35_probe rev-parse HEAD)" = "$(cat /evidence/source.rev)"
''')
        guest(target, suffix + '-records', f'''
python3 - <<'INNER' > /evidence/{suffix}-packages.json
import glob,json
records=[]
for path in glob.glob('.rospm/conda/env/conda-meta/*.json'):
    obj=json.load(open(path))
    records.append({{k: obj.get(k) for k in ('name','version','build','subdir','sha256','md5','url')}})
assert records, 'No installed Conda package records'
print(json.dumps(sorted(records,key=lambda p:p['name']),sort_keys=True,indent=2))
INNER
''')
        if suffix == 'replay-a':
            guest(target, 'repeat-baseline', 'stat -c "%i %Y %n" ~/.local/share/rospm/conda/*/bin/conda .rospm/conda/env/conda-meta/history > /tmp/reuse-before')
            repeated = guest(target, 'repeat-one-command', apply + ' --locked --build')
            assert 'P35_MANAGED_OK' in repeated
            guest(target, 'repeat-reused', '''
stat -c "%i %Y %n" ~/.local/share/rospm/conda/*/bin/conda .rospm/conda/env/conda-meta/history > /tmp/reuse-after
cmp /tmp/reuse-before /tmp/reuse-after
sha256sum -c /evidence/shell-before.sha256
''')
    assert (root / 'replay-a-packages.json').read_bytes() == (root / 'replay-b-packages.json').read_bytes()
    (root / 'result.txt').write_text('PASS: two clean Conda replays, build, node, exact package records, reuse, shell files\n')
    print((root / 'result.txt').read_text(), flush=True)
finally:
    for name in containers:
        subprocess.run(['docker', 'rm', '-f', name], check=False, stdout=subprocess.DEVNULL)
    subprocess.run(['docker', 'image', 'rm', image], check=False, stdout=subprocess.DEVNULL)
