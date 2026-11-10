"""Self-check the creation boundary without installing tools or touching ROS."""
import os
from pathlib import Path
import subprocess
import sys
import tempfile

create = Path(sys.argv[1]).read_text()
with tempfile.TemporaryDirectory(prefix='rospm-create-') as directory:
    root = Path(directory)
    target = root / "env with 'quotes'"
    setup = root / 'setup.bash'
    setup.write_text('export ROSPM_SETUP_CHECKED=1\n')
    # Stand-in only for the external tool: assert argv and environment contracts.
    backend = root / 'uv'
    backend.write_text('''#!/usr/bin/python3
import os,sys
from pathlib import Path
assert sys.argv[1:-1] == ['--no-config','venv','--python','/usr/bin/python3','--no-python-downloads','--system-site-packages']
assert not any(k.startswith(('UV_', 'CONDA_', 'PIP_')) for k in os.environ)
assert 'PYTHONHOME' not in os.environ
assert Path(sys.argv[-1]).is_dir()
Path(sys.argv[-1], 'backend-ran').touch()
with open(Path(sys.argv[-1]).parent / 'calls', 'a') as output:
    output.write('called'+chr(10))
raise SystemExit(7)
''')
    backend.chmod(0o755)
    check = "import os; assert os.environ['ROSPM_SETUP_CHECKED'] == '1'"
    env = dict(os.environ, UV_VENV_SEED='1', CONDA_PREFIX='/foreign',
               PIP_TARGET='/foreign', PYTHONHOME='/foreign', BASH_ENV='/foreign')
    def args(check_code=check):
        return ['/usr/bin/python3', '-I', '-B', '-c', create, str(target), str(setup),
                str(backend), check_code, 'partial retained', 'ensurepip missing']
    # Preflight failure never claims the target.
    result = subprocess.run(args("raise RuntimeError('ROS check failed')"), env=env, capture_output=True)
    assert result.returncode and not target.exists()
    # Concurrent requests: exactly one backend enters the exclusively claimed path.
    first = subprocess.Popen(args(), env=env, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
    second = subprocess.Popen(args(), env=env, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
    outputs = [first.communicate(), second.communicate()]
    assert first.returncode and second.returncode
    assert (target / 'backend-ran').exists()
    assert (root / 'calls').read_text() == 'called\n'
    assert subprocess.run(args(), env=env, capture_output=True).returncode
    assert (target / 'backend-ran').exists()
    # Existing empty directory, file and dangling symlink are all protected.
    (target / 'backend-ran').unlink()
    assert subprocess.run(args(), env=env, capture_output=True).returncode
    target.rmdir()
    target.write_text('keep')
    assert subprocess.run(args(), env=env, capture_output=True).returncode
    assert target.read_text() == 'keep'
    target.unlink()
    target.symlink_to(root / 'absent')
    assert subprocess.run(args(), env=env, capture_output=True).returncode
    assert target.is_symlink() and not (root / 'absent').exists()
    target.unlink()
    # Successful tool + successful postcheck; the test check does not claim ABI validation.
    backend.write_text('#!/usr/bin/python3\nimport sys\nfrom pathlib import Path\nPath(sys.argv[-1], "bin").mkdir()\nPath(sys.argv[-1], "bin/python").symlink_to("/usr/bin/python3")\n')
    assert subprocess.run(args(), env=env, capture_output=True).returncode == 0
    target.joinpath('bin/python').unlink()
    target.joinpath('bin').rmdir()
    target.rmdir()
    # Postcheck fails after the backend returns success: preserve its output.
    postfail = "import sys; assert len(sys.argv) > 2, 'postcheck failed'"
    result = subprocess.run(args(postfail), env=env, capture_output=True)
    assert result.returncode and target.joinpath('bin/python').exists()
    # Standard backend contract without requiring ensurepip on the test host.
    import shutil
    from unittest.mock import patch
    shutil.rmtree(target)
    calls = []
    def fake_run(command, **kwargs):
        calls.append(command)
        if command[:4] == ['/usr/bin/python3', '-I', '-m', 'venv']:
            assert command[4:] == ['--system-site-packages', str(target)]
            assert target.is_dir()
        return subprocess.CompletedProcess(command, 0)
    with patch.object(sys, 'argv', ['create', str(target), str(setup), '', check, 'failed', 'missing']), patch.object(subprocess, 'run', fake_run):
        exec(compile(create, 'venv_create.py', 'exec'), {})
    assert len(calls) == 4  # system check, ensurepip probe, creation, postcheck
    assert calls[1][-1] == 'import venv, ensurepip'
print('creation boundary checks passed')
