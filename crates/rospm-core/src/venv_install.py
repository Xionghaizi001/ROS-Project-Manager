import os
from pathlib import Path
import shlex
import subprocess
import sys

path, setup, uv, requirements, check, failure, missing = sys.argv[1:]
python = str(Path(path) / 'bin/python')
env = {key: value for key, value in os.environ.items()
       if not key.startswith(('PYTHON', 'CONDA_', 'UV_', 'PIP_', 'VIRTUAL_ENV'))
       and key not in {'BASH_ENV', 'ENV', '_CE_CONDA', '_CE_M', 'LD_LIBRARY_PATH',
                      'AMENT_PREFIX_PATH', 'COLCON_PREFIX_PATH', 'CMAKE_PREFIX_PATH'}}
env.update(PATH=str(Path(path) / 'bin') + ':/usr/bin:/bin:/usr/sbin:/sbin',
           PYTHONNOUSERSITE='1', PYTHONDONTWRITEBYTECODE='1', PIP_CONFIG_FILE=os.devnull,
           VIRTUAL_ENV=path)

def validate():
    command = [python, '-B', '-s', '-c', check, path]
    script = 'set -e\nsource ' + shlex.quote(setup) + '\n' + shlex.join(command)
    subprocess.run(['/bin/bash', '--noprofile', '--norc', '-c', script], env=env, check=True)

try:
    validate()
    if uv:
        base = [uv, '--no-config', '--no-python-downloads', 'pip']
        install = base + ['install', '--python', python, '--requirement', requirements]
        verify = base + ['check', '--python', python]
    else:
        # system-site-packages can expose system pip; never silently use it.
        probe = "import pip,sys; from pathlib import Path; assert Path(pip.__file__).resolve().is_relative_to(Path(sys.prefix).resolve())"
        result = subprocess.run([python, '-I', '-B', '-c', probe], env=env)
        if result.returncode:
            sys.exit(missing)
        base = [python, '-I', '-B', '-m', 'pip', '--disable-pip-version-check', '--no-input']
        install = base + ['install', '--requirement', requirements]
        verify = base + ['check']
    subprocess.run(install, env=env, check=True)
    subprocess.run(verify, env=env, check=True)
    validate()
except (OSError, subprocess.CalledProcessError) as error:
    print(f'{failure}\n{error}', file=sys.stderr)
    sys.exit(1)
