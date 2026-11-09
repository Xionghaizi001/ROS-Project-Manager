import importlib
import json
import os
from pathlib import Path
import platform
import subprocess
import sys

prefix = Path(sys.argv[1]).resolve()
def require(ok, message):
    if not ok:
        sys.exit(f"venv {prefix}: {message}")

require(Path(sys.prefix).resolve() == prefix and sys.prefix != sys.base_prefix,
        "interpreter is not running in the selected venv")
require(not (prefix / 'conda-meta').exists(), 'Conda environments are not venvs')
config = dict(line.split('=', 1) for line in (prefix / 'pyvenv.cfg').read_text().splitlines() if '=' in line)
require(any(k.strip().lower() == 'include-system-site-packages' and v.strip().lower() == 'true' for k, v in config.items()), 'include-system-site-packages must be true')
system = Path('/usr/bin/python3').resolve()
require(Path(sys._base_executable).resolve() == system, 'base interpreter must be /usr/bin/python3')
owner = subprocess.run(['/usr/bin/dpkg-query', '-S', str(system)], capture_output=True, text=True)
require(owner.returncode == 0 and any(line.startswith('python3.') for line in owner.stdout.splitlines()), 'system Python is not owned by an apt Python package')
facts = subprocess.check_output(['/usr/bin/python3', '-I', '-c', 'import json,sys,platform; print(json.dumps([sys.version_info[:2],platform.machine(),platform.python_implementation()]))'], text=True)
require(json.loads(facts) == [list(sys.version_info[:2]), platform.machine(), platform.python_implementation()] and platform.python_implementation() == 'CPython', 'Python version/architecture mismatch')
modules = ['catkin_pkg', 'em', 'rclpy._rclpy_pybind11']
# ROS's freshly sourced PYTHONPATH is required by both interpreters.
subprocess.run(['/usr/bin/python3', '-s', '-c', 'import importlib; ' + '; '.join(f'importlib.import_module({m!r})' for m in modules)], check=True)
for module in modules:
    importlib.import_module(module)
