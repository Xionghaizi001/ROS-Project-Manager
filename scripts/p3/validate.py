#!/usr/bin/env python3
"""Acceptance assertions over actual rospm outputs (Ubuntu 24.04 Python)."""
import hashlib
import json
import sys
import tomllib
from pathlib import Path


def lock(path):
    return tomllib.loads(Path(path).read_text())


def pins(data, platform):
    return {p['name']: p['version'] for p in data['apt'] if p['platform'] == platform}


mode, filename, *args = sys.argv[1:]
if mode == 'lock':
    data = lock(filename)
    native = data['provider']['native']
    assert native.get('ros_snapshot'), 'ROS snapshot missing (degraded lock)'
    assert native.get('ubuntu_snapshot'), 'Ubuntu snapshot missing (degraded lock)'
    arches = ['x86_64', 'aarch64'] if args[0] == 'both' else [args[0]]
    for arch in arches:
        entries = pins(data, f'linux-{arch}')
        assert 'ros-jazzy-ros-base' in entries
        assert all(tool in entries for tool in (
            'python3-rosdep', 'python3-vcstool', 'python3-colcon-common-extensions'))
        replacement = 'aarch64' if arch == 'aarch64' else 'x86-64'
        assert f'binutils-{replacement}-linux-gnu' in entries
        if arch == 'aarch64':
            assert 'binutils-x86-64-linux-gnu' not in entries
        print(f'{arch}: {len(entries)} apt pins; snapshots: {native}')
    assert len(data['sources']) == 1
elif mode == 'seed':
    data = lock(filename)
    expected_hash = 'sha256:' + hashlib.sha256(Path(args[0]).read_bytes()).hexdigest()
    assert data['manifest_sha256'] == expected_hash, 'Seed manifest changed'
    assert data['sources'][0]['rev'] == Path(args[1]).read_text().strip(), 'Seed source changed'
    print('Imported real ARM seed matches this manifest and source revision')
elif mode == 'sources':
    native = lock(filename)['provider']['native']
    ros_source = Path('/etc/apt/sources.list.d/ros2.list').read_text()
    ubuntu_source = Path('/etc/apt/sources.list.d/rospm-ubuntu-snapshot.list').read_text()
    assert f"snapshots.ros.org/jazzy/{native['ros_snapshot']}/ubuntu" in ros_source
    assert f"snapshot.ubuntu.com/ubuntu/{native['ubuntu_snapshot']}" in ubuntu_source
    assert 'arch=arm64' in ros_source
    print('Both configured snapshots match the input lock; ROS source uses arm64')
elif mode == 'compare':
    before, after = lock(filename), lock(args[0])
    for platform in ('linux-x86_64', 'linux-aarch64'):
        assert pins(before, platform) == pins(after, platform), platform
    assert before['sources'] == after['sources']
    assert before['manifest_sha256'] == after['manifest_sha256']
    print('Relock preserved both platforms, source revision and manifest hash')
elif mode == 'missing':
    data = json.loads(Path(filename).read_text())
    items = data['repositories']['items']
    assert any(i['status'] == 'missing' and
               'rospm-p3-deliberately-missing-package' in str(i) for i in items), items
    assert not any(i['status'] == 'unknown' for i in items), items
    print('Absent target package explicitly reported missing')
else:
    raise ValueError(mode)
