#!/usr/bin/env python3
"""Real P3.5 first-item acceptance; requires ROS Humble and prepared Conda envs.

Run in an unactivated terminal:
  python3 scripts/p35-existing-conda-check.py --root /tmp/p35-run \
    --conda /path/to/miniforge/bin/conda --name p35-py310 \
    --prefix /path/to/envs/p35-py310 --other-prefix /path/to/envs/p35-other \
    --bad-prefix /path/to/miniforge
Good environments need Python 3.10, colcon-common-extensions, catkin_pkg,
empy==3.3.4, setuptools<80, numpy<2 and pyfiglet. Bad environment needs a different
Python minor version and the same build tools. This script creates a NEW root;
it never deletes existing workspaces or installs system packages.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import shlex
import subprocess

p = argparse.ArgumentParser(description=__doc__)
for key in ('root', 'conda', 'name', 'prefix', 'other-prefix', 'bad-prefix'):
    p.add_argument('--' + key, required=True)
p.add_argument('--rospm', default='target/debug/rospm')
a = p.parse_args()
root = Path(a.root).resolve()
root.mkdir(parents=True, exist_ok=False)
logs = root / 'evidence'
logs.mkdir()
rospm = str(Path(a.rospm).resolve())
env = dict(os.environ)
for key in list(env):
    if key.startswith(('CONDA', '_CONDA', 'AMENT_', 'COLCON_', 'ROS_', 'RMW_')) or key in ('PYTHONPATH', 'LD_LIBRARY_PATH', 'CMAKE_PREFIX_PATH'):
        env.pop(key, None)
tools = root / 'tools'
tools.mkdir()
(tools / 'conda').symlink_to(Path(a.conda).resolve())
env['PATH'] = str(tools) + ':/usr/local/bin:/usr/bin:/bin'
env['ROSPM_LANG'] = 'en'
env['ROS_LOCALHOST_ONLY'] = '1'
env['ROS_DOMAIN_ID'] = '85'
assert 'CONDA_PREFIX' not in env
results = []

def run(label, cmd, cwd=root, ok=True, contains=()):
    proc = subprocess.run(cmd, cwd=cwd, env=env, stdin=subprocess.DEVNULL,
                          stdout=subprocess.PIPE, stderr=subprocess.STDOUT, text=True, timeout=180)
    (logs / (label + '.log')).write_text('$ ' + shlex.join(cmd) + '\n' + proc.stdout)
    assert (proc.returncode == 0) == ok, (label, proc.returncode, proc.stdout[-5000:])
    for value in contains:
        assert value in proc.stdout, (label, value, proc.stdout[-5000:])
    results.append({'case': label, 'exit': proc.returncode, 'passed': True})
    (logs / 'results.json').write_text(json.dumps(results, indent=2) + '\n')
    print('PASS', label, flush=True)
    return proc.stdout

base = '''schema_version = 1
[project]
name = "p35-existing-conda"
ros_distro = "humble"
platforms = ["linux-x86_64"]
[environment]
provider = "native"
[environment.native]
ubuntu = "22.04"
ros_base = "ros-humble-ros-base"
[workspaces.main]
path = "ws"
sources = "main.repos"
[workspaces.main.colcon]
build_args = ["--symlink-install"]
[dependencies.rosdep]
enabled = false
'''

def make(label, selection=''):
    project = root / label
    pkg = project / 'ws/src/p35_probe'
    (pkg / 'p35_probe').mkdir(parents=True)
    (pkg / 'resource').mkdir()
    (pkg / 'resource/p35_probe').touch()
    (pkg / 'p35_probe/__init__.py').touch()
    (pkg / 'p35_probe/node.py').write_text('''import os, sys
import rclpy

def main():
    if os.environ.get('P35_REQUIRE_EXTRA'):
        import pyfiglet
        assert pyfiglet.figlet_format('OK').strip()
        assert pyfiglet.__file__.startswith(os.environ['P35_EXPECT_PREFIX'] + '/')
        print('EXTRA=' + pyfiglet.__file__)
    assert sys.executable.startswith(os.environ['P35_EXPECT_PREFIX'] + '/')
    rclpy.init()
    node = rclpy.create_node('p35_probe')
    print('P35_NODE_OK PYTHON=' + sys.executable, flush=True)
    node.destroy_node()
    rclpy.shutdown()
''')
    (pkg / 'setup.py').write_text("""from setuptools import setup
setup(name='p35_probe', version='0.0.0', packages=['p35_probe'],
 data_files=[('share/ament_index/resource_index/packages',['resource/p35_probe']),
 ('share/p35_probe',['package.xml'])],
 entry_points={'console_scripts':['probe = p35_probe.node:main']})
""")
    (pkg / 'setup.cfg').write_text('[develop]\nscript_dir=$base/lib/p35_probe\n[install]\ninstall_scripts=$base/lib/p35_probe\n')
    (pkg / 'package.xml').write_text('''<package format="3"><name>p35_probe</name><version>0.0.0</version>
<description>P35 real acceptance</description><maintainer email="test@example.com">Test</maintainer>
<license>MIT</license><buildtool_depend>ament_python</buildtool_depend><exec_depend>rclpy</exec_depend>
<export><build_type>ament_python</build_type></export></package>''')
    cmake = project / 'ws/src/p35_cache'
    cmake.mkdir()
    (cmake / 'package.xml').write_text('''<package format="3"><name>p35_cache</name><version>0.0.0</version>
<description>Real interpreter cache</description><maintainer email="test@example.com">Test</maintainer>
<license>MIT</license><buildtool_depend>ament_cmake</buildtool_depend>
<export><build_type>ament_cmake</build_type></export></package>''')
    (cmake / 'CMakeLists.txt').write_text('cmake_minimum_required(VERSION 3.16)\nproject(p35_cache)\nfind_package(ament_cmake REQUIRED)\nfind_package(Python3 REQUIRED COMPONENTS Interpreter)\nament_package()\n')
    (project / 'main.repos').write_text('repositories: {}\n')
    (project / 'rospm.toml').write_text(base + selection)
    return project

def cli(project, *args):
    return [rospm, '--manifest', str(project / 'rospm.toml'), *args]

def snapshot(project):
    return {str(f.relative_to(project)): ('dir' if f.is_dir() else hashlib.sha256(f.read_bytes()).hexdigest())
            for f in project.rglob('*')}

def node(project, label, prefix, extra=True):
    script = subprocess.check_output(cli(project, 'env'), env=env, text=True)
    script += '\nexport P35_EXPECT_PREFIX=' + shlex.quote(prefix) + '\n'
    if extra:
        script += 'export P35_REQUIRE_EXTRA=1\n'
    script += 'ros2 run p35_probe probe\n'
    run(label, ['bash', '--noprofile', '--norc', '-c', script], cwd=project,
        contains=['P35_NODE_OK'] + (['EXTRA='] if extra else []))

run('system-dependency-absent', ['/usr/bin/python3', '-c',
    "import importlib.util; assert importlib.util.find_spec('pyfiglet') is None"])
for label, selection, prefix in [
    ('name', '\n[build.conda]\nname = ' + json.dumps(a.name) + '\n', a.prefix),
    ('prefix with spaces', '\n[build.conda]\nprefix = ' + json.dumps(a.other_prefix) + '\n', a.other_prefix),
]:
    project = make(label, selection)
    before = snapshot(project)
    run(label + '-preview', cli(project, 'build', '--dry-run'))
    assert snapshot(project) == before, 'preview wrote files'
    run(label + '-build', cli(project, 'build'), cwd=project)
    node(project, label + '-node', prefix)
    if label == 'name':
        cache = project / 'ws/build/p35_cache/CMakeCache.txt'
        assert a.prefix in cache.read_text()
        cache_before = cache.read_bytes()
        run('switched-environment-cache', cli(project, 'build', '--conda-prefix', a.other_prefix),
            ok=False, contains=['p35_cache', 'another Python interpreter', 'Remove the workspace'])
        assert cache.read_bytes() == cache_before

project = make('diagnostics')
run('missing-name', cli(project, 'build', '--conda-env', 'p35-no-such-env'), ok=False, contains=['was not found'])
run('missing-prefix', cli(project, 'build', '--conda-prefix', str(root / 'absent')), ok=False, contains=['was not found'])
run('abi-conflict', cli(project, 'build', '--conda-prefix', a.bad_prefix), ok=False,
    contains=['does not match', 'Python 3.10', 'ROS Python extensions cannot load'])
assert not (project / 'ws/build').exists()
project = make('native')
run('native-build', cli(project, 'build'), cwd=project)
node(project, 'native-node', '/usr', extra=False)
print('All acceptance cases passed. Evidence:', logs)
