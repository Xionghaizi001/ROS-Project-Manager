"""Exercise install orchestration without network, ROS or changing packages."""
import os
from pathlib import Path
import subprocess
import sys
import tempfile
from unittest.mock import patch

source = Path(sys.argv[1]).read_text()
with tempfile.TemporaryDirectory(prefix='rospm-install-') as directory:
    root = Path(directory)
    path = root / "env with 'quotes'"
    requirements = root / 'requirements file.txt'
    requirements.write_text('demo==1\n')
    for uv in ['', '/explicit/uv']:
        calls = []
        def run(command, **kwargs):
            calls.append(command)
            env = kwargs['env']
            assert env['PIP_CONFIG_FILE'] == os.devnull
            assert env['VIRTUAL_ENV'] == str(path)
            assert 'PIP_TARGET' not in env and 'PIP_PREFIX' not in env and 'PIP_USER' not in env
            assert not any(k.startswith(('UV_', 'CONDA_')) for k in env)
            assert 'PYTHONHOME' not in env and 'BASH_ENV' not in env
            return subprocess.CompletedProcess(command, 0)
        args = ['install', str(path), '/opt/ros/test/setup.bash', uv, str(requirements), 'check', 'failed', 'missing pip']
        with patch.object(sys, 'argv', args), patch.dict(os.environ, {'UV_SYSTEM_PYTHON':'1','CONDA_PREFIX':'foreign','PIP_TARGET':'foreign','PIP_USER':'1','PIP_PREFIX':'foreign','PYTHONHOME':'foreign','BASH_ENV':'foreign'}), patch.object(subprocess, 'run', run):
            exec(compile(source, 'venv_install.py', 'exec'), {})
        install = next(c for c in calls if 'install' in c)
        assert install[-2:] == ['--requirement', str(requirements)]
        if uv:
            assert install[:4] == [uv, '--no-config', '--no-python-downloads', 'pip']
            assert install[install.index('--python')+1] == str(path / 'bin/python')
        else:
            assert install[:5] == [str(path / 'bin/python'), '-I', '-B', '-m', 'pip']
        assert calls[0][0] == calls[-1][0] == '/bin/bash'
        assert any(c[-1] == 'check' or (uv and 'check' in c) for c in calls)
    # Missing target-local pip must not fall back to any installation command.
    calls = []
    def no_pip(command, **kwargs):
        calls.append(command)
        return subprocess.CompletedProcess(command, 1 if command[0] != '/bin/bash' else 0)
    args[3] = ''
    with patch.object(sys, 'argv', args), patch.object(subprocess, 'run', no_pip):
        try:
            exec(compile(source, 'venv_install.py', 'exec'), {})
            raise AssertionError('missing pip succeeded')
        except SystemExit as error:
            assert error.code == 'missing pip'
    assert not any('install' in c for c in calls)
    # A failed install stops before check/postvalidation and preserves input.
    def fail_install(command, **kwargs):
        if 'install' in command:
            raise subprocess.CalledProcessError(7, command)
        return subprocess.CompletedProcess(command, 0)
    with patch.object(sys, 'argv', args), patch.object(subprocess, 'run', fail_install):
        try:
            exec(compile(source, 'venv_install.py', 'exec'), {})
            raise AssertionError('failure reported success')
        except SystemExit as error:
            assert error.code == 1
    assert requirements.read_text() == 'demo==1\n'
print('install boundary checks passed')
