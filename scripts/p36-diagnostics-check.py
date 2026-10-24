#!/usr/bin/env python3
"""Phase 3.6 real CLI acceptance (Linux, ROS Humble, colcon, CMake, GCC, Clang).

Creates a NEW --root, including minimal C++/Python projects and raw evidence.
Does not install dependencies or run apply. Both compiler suites are required;
missing prerequisites are failures, never silently skipped.
"""
import argparse
import json
import os
from pathlib import Path
import selectors
import shlex
import signal
import subprocess
import time


def package(workspace, name, body, files=None):
    path = workspace / 'ws/src' / name
    path.mkdir(parents=True)
    (path / 'package.xml').write_text(f'''<package format="3">
<name>{name}</name><version>0.0.0</version><description>P36 acceptance</description>
<maintainer email="test@example.com">Test</maintainer><license>MIT</license>
<buildtool_depend>cmake</buildtool_depend><export><build_type>cmake</build_type></export>
</package>''')
    (path / 'CMakeLists.txt').write_text(
        f'cmake_minimum_required(VERSION 3.16)\nproject({name} LANGUAGES CXX)\n'
        + body + '\ninstall(FILES package.xml DESTINATION share/${PROJECT_NAME})\n')
    for name, content in (files or {}).items():
        (path / name).write_text(content)


def project(root, compiler):
    root.mkdir()
    (root / 'main.repos').write_text('repositories: {}\n')
    (root / 'rospm.toml').write_text('''schema_version = 1
[project]
name = "p36-diagnostics"
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
[dependencies.rosdep]
enabled = false
''')
    color = '-fdiagnostics-color=always'
    exe = f'add_compile_options({color})\nadd_executable(probe main.cpp)'
    package(root, 'p_syntax', exe, {'main.cpp': 'int main() {\n  return 0\n}\n'})
    package(root, 'p_header', exe, {'main.cpp': '#include <rospm_p36_nonexistent.hpp>\nint main() {}\n'})
    package(root, 'p_template', exe, {'main.cpp': '''template<class T> int convert(T value) { return value; }
int main() { return convert("text"); }
'''})
    package(root, 'p_package', 'find_package(rospm_p36_nonexistent REQUIRED)')
    package(root, 'p_library', exe + '\ntarget_link_libraries(probe PRIVATE rospm_p36_nonexistent)',
            {'main.cpp': 'int main() {}\n'})
    package(root, 'p_symbol', exe, {'main.cpp': 'extern int missing();\nint main() { return missing(); }\n'})
    # Real Python files run by a CMake target so colcon captures each traceback
    # in its own package stream, rather than failing during package discovery.
    python_target = 'add_custom_target(probe ALL COMMAND /usr/bin/python3 -I -S "${CMAKE_CURRENT_SOURCE_DIR}/node.py")'
    package(root, 'p_python_syntax', python_target, {'node.py': 'def broken(\n'})
    package(root, 'p_python_import', python_target, {'node.py': 'import rospm_p36_nonexistent\n'})
    package(root, 'p_python_chain', python_target, {'node.py': '''import time
print("P36_LIVE_OUTPUT", flush=True)
time.sleep(1)
try:
    raise KeyError("first")
except KeyError as exc:
    raise ValueError("last") from exc
'''})
    package(root, 'p_unknown', python_target, {'node.py': '''import sys
sys.stderr.write("\\x1b[31mP36_OPAQUE_FAILURE\\x1b[0m\\n")
sys.exit(23)
'''})
    package(root, 'p_success', exe, {'main.cpp': 'int main() { return 0; }\n'})
    for cmake in (root / 'ws/src').glob('*/CMakeLists.txt'):
        cmake.write_text(f'set(CMAKE_CXX_COMPILER "{compiler}")\n' + cmake.read_text())


def run(binary, root, evidence, label, env, lang, packages, json_mode, expected_live=False):
    command = [binary, '--lang', lang, 'build']
    if json_mode:
        command.append('--json')
    command += ['--', '--continue-on-error', '--parallel-workers', '4',
                '--event-handlers', 'console_direct+', '--packages-select', *packages]
    buffers = {'stdout': bytearray(), 'stderr': bytearray()}
    live = False
    started = time.monotonic()
    with subprocess.Popen(command, cwd=root, env=env, stdin=subprocess.DEVNULL,
                          stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                          start_new_session=True) as process:
        try:
            with selectors.DefaultSelector() as selector:
                selector.register(process.stdout, selectors.EVENT_READ, 'stdout')
                selector.register(process.stderr, selectors.EVENT_READ, 'stderr')
                while selector.get_map():
                    assert time.monotonic() - started < 180, f'{label}: timeout'
                    for key, _ in selector.select(timeout=0.2):
                        chunk = os.read(key.fd, 65536)
                        if not chunk:
                            selector.unregister(key.fileobj)
                            continue
                        buffers[key.data].extend(chunk)
                        if b'P36_LIVE_OUTPUT' in buffers[key.data] and process.poll() is None:
                            live = True
            code = process.wait(timeout=5)
        except BaseException:
            os.killpg(process.pid, signal.SIGKILL)
            process.wait()
            raise
    for stream, data in buffers.items():
        (evidence / f'{label}.{stream}').write_bytes(data)
    log = root / '.rospm/log/build.log'
    saved = log.read_bytes()
    (evidence / f'{label}.build.log').write_bytes(saved)
    (evidence / f'{label}.command').write_text(shlex.join(command) + '\n')
    success = packages == ['p_success']
    assert code == (0 if success else 1), (label, code, buffers['stderr'].decode(errors='replace'))
    assert not expected_live or live, f'{label}: output was not received while child was running'
    result = json.loads(buffers['stdout']) if json_mode else None
    if success:
        assert result == {'ok': True}, result
        assert b'Build failure summary' not in buffers['stderr']
    else:
        assert b'==> exit status:' in saved
        if json_mode:
            assert result['ok'] is False
            summary = result['build_summary']
            assert summary['log'] == str(log)
            expected_status = 'exit status: 2' if packages == ['p_unknown'] else 'exit status: 1'
            assert summary['status'] == expected_status, summary['status']
            assert expected_status in result['error']
            assert ('==> ' + expected_status).encode() in saved
        else:
            title = 'Build failure summary' if lang == 'en' else '构建失败摘要'
            assert title.encode() in buffers['stderr']
            assert str(log).encode() in buffers['stderr']
    return result, saved, buffers


def validate(result, root, saved):
    summary = result['build_summary']
    assert summary['diagnostic_status'] == 'parsed'
    groups = {g['package']: g for g in summary['groups'] if g['package']}
    expected = {
        'p_syntax': ('syntax_error', 'syntax'),
        'p_header': ('missing_header', 'header_search'),
        'p_template': ('type_error', 'type'),
        'p_package': ('package_not_found', 'package_discovery'),
        'p_library': ('library_not_found', 'linking'),
        'p_symbol': ('undefined_reference', 'linking'),
        'p_python_syntax': ('syntax_error', 'syntax'),
        'p_python_import': ('module_not_found', 'import'),
        'p_python_chain': ('runtime_error', 'runtime'),
    }
    for name, (code, category) in expected.items():
        group = groups[name]
        matches = [d for d in group['diagnostics'] if d['code'] == code]
        assert len(matches) == 1, (name, group)
        d = matches[0]
        assert group['workspace'] == 'main'
        assert d['category'] == category, (name, d)
        assert d['explanation'] and d['suggestions'], (name, d)
        suggestion = ' '.join(d['suggestions'])
        anchors = {'p_header': 'target_include_directories', 'p_library': 'target_link_libraries',
                   'p_symbol': 'target_link_libraries', 'p_package': 'rospm deps check',
                   'p_python_import': 'sys.executable'}
        if name in anchors:
            assert anchors[name] in suggestion, (name, suggestion)
        # Excerpts can omit unsupported source-display lines (Clang); every
        # retained raw line must appear unchanged, in order, in the package log.
        package_log = root / f'ws/log/latest_build/{name}/stdout_stderr.log'
        remaining = package_log.read_text()
        for line in filter(None, d['raw'].splitlines()):
            assert line.encode() in saved, (name, line)
            assert line in remaining, (name, line)
            remaining = remaining.split(line, 1)[1]
        if name in ('p_header', 'p_python_import', 'p_python_syntax'):
            assert d['location']['line'] == 1, (name, d)
        if name == 'p_syntax':
            assert saved.count(d['raw'].splitlines()[0].encode()) >= 2
        if name in ('p_syntax', 'p_template'):
            assert d['location']['file'].endswith('main.cpp'), d
            assert d['location']['line'] in (1, 2, 3), d
        if name == 'p_library':
            assert d['location'] is None, d
        if name in ('p_template', 'p_python_chain'):
            assert d['related'], (name, d)
        if name == 'p_python_chain':
            assert 'KeyError' in json.dumps(d['related']), d
            assert d['location']['line'] == 7, d
    assert any(g['consequences'] for g in summary['groups'])
    # Colcon echoes package errors; the aggregate must not duplicate diagnostics.
    assert not any(g['diagnostics'] for g in summary['groups'] if g['package'] is None)
    assert b'\x1b[' in saved
    assert b'P36_LIVE_OUTPUT' in saved
    assert b'Failed' in saved


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--root', required=True)
    parser.add_argument('--rospm', default='target/debug/rospm')
    parser.add_argument('--gcc', default='/usr/bin/g++')
    parser.add_argument('--clang', default='/usr/bin/clang++')
    args = parser.parse_args()
    binary = str(Path(args.rospm).resolve())
    compilers = {'gcc': str(Path(args.gcc).absolute()), 'clang': str(Path(args.clang).absolute())}
    root = Path(args.root).resolve()
    root.mkdir(parents=True, exist_ok=False)
    evidence = root / 'evidence'
    evidence.mkdir()
    env = dict(os.environ)
    for key in list(env):
        if key.startswith(('CONDA', '_CONDA', 'AMENT_', 'COLCON_', 'ROS_', 'RMW_')) or key in (
                'PYTHONPATH', 'CMAKE_PREFIX_PATH', 'ROSPM_LANG', 'LANGUAGE', 'LC_MESSAGES'):
            env.pop(key, None)
    env.update(PATH='/usr/bin:/bin', LANG='C.UTF-8', LC_ALL='C.UTF-8')
    assert Path('/opt/ros/humble/setup.bash').is_file(), 'requires ROS Humble'
    versions = {}
    for name, command in {**{k: [v, '--version'] for k, v in compilers.items()},
                          'cmake': ['cmake', '--version'], 'ld': ['ld', '--version'],
                          'python': ['/usr/bin/python3', '--version']}.items():
        versions[name] = subprocess.check_output(command, env=env, text=True).splitlines()[0]
    (evidence / 'versions.json').write_text(json.dumps(versions, indent=2) + '\n')
    results = []
    for compiler, path in compilers.items():
        workspace = root / f'{compiler} project with spaces'
        project(workspace, path)
        packages = sorted(p.name for p in (workspace / 'ws/src').iterdir() if p.name != 'p_unknown')
        for lang, json_mode in [('en', True), ('zh', True), ('en', False), ('zh', False)]:
            label = f'{compiler}-{lang}-' + ('json' if json_mode else 'text')
            result, saved, _ = run(binary, workspace, evidence, label, env, lang, packages, json_mode, True)
            if json_mode:
                validate(result, workspace, saved)
            results.append(label)
            print('PASS', label, flush=True)
        for lang in ('en', 'zh'):
            for json_mode in (True, False):
                label = f'{compiler}-unknown-{lang}-{json_mode}'
                result, saved, buffers = run(binary, workspace, evidence, label, env, lang, ['p_unknown'], json_mode)
                raw = b'\x1b[31mP36_OPAQUE_FAILURE\x1b[0m'
                assert raw in saved and raw in buffers['stderr']
                if json_mode:
                    summary = result['build_summary']
                    assert summary['diagnostic_status'] == 'unrecognized', summary
                    assert all(not g['diagnostics'] for g in summary['groups'])
                else:
                    hint = 'No supported error format' if lang == 'en' else '未识别到支持的错误格式'
                    assert hint.encode() in buffers['stderr']
                results.append(label)
                print('PASS', label, flush=True)
        label = f'{compiler}-success-after-failures'
        run(binary, workspace, evidence, label, env, 'en', ['p_success'], True)
        results.append(label)
        print('PASS', label, flush=True)
    (evidence / 'results.json').write_text(json.dumps({'passed': results, 'versions': versions}, indent=2) + '\n')
    print('All acceptance cases passed. Evidence:', evidence)


if __name__ == '__main__':
    main()
