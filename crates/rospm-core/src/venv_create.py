import os
from pathlib import Path
import shlex
import subprocess
import sys

path, setup, uv, check, failure, missing = sys.argv[1:]
# Do not let startup files, Python paths or uv configuration select another target.
env = {key: value for key, value in os.environ.items()
       if not key.startswith(('PYTHON', 'CONDA_', 'UV_', 'PIP_', 'VIRTUAL_ENV'))
       and key not in {'BASH_ENV', 'ENV', '_CE_CONDA', '_CE_M', 'LD_LIBRARY_PATH',
                      'AMENT_PREFIX_PATH', 'COLCON_PREFIX_PATH', 'CMAKE_PREFIX_PATH'}}
env.update(PATH='/usr/bin:/bin:/usr/sbin:/sbin', PYTHONNOUSERSITE='1', PYTHONDONTWRITEBYTECODE='1')

def validate(python, system=False):
    command = [python, '-B', '-s', '-c', check, path]
    if system:
        command.append('--system')
    script = 'set -e\nsource ' + shlex.quote(setup) + '\n' + shlex.join(command)
    subprocess.run(['/bin/bash', '--noprofile', '--norc', '-c', script], env=env, check=True)

try:
    if os.path.lexists(path):
        raise FileExistsError(path)
    validate('/usr/bin/python3', system=True)
    if uv:
        command = [uv, '--no-config', 'venv', '--python', '/usr/bin/python3',
                   '--no-python-downloads', '--system-site-packages', path]
    else:
        probe = subprocess.run(['/usr/bin/python3', '-I', '-c', 'import venv, ensurepip'], env=env)
        if probe.returncode:
            sys.exit(missing)
        command = ['/usr/bin/python3', '-I', '-m', 'venv', '--system-site-packages', path]
    # Exclusive mkdir claims the final path; never use exist_ok, clear or cleanup.
    os.mkdir(path)
    subprocess.run(command, env=env, check=True)
    validate(str(Path(path) / 'bin/python'))
except (OSError, subprocess.CalledProcessError) as error:
    print(f'{failure}\n{error}', file=sys.stderr)
    sys.exit(1)
