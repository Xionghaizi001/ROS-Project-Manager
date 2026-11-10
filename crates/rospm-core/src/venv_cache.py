import os
from pathlib import Path
import sys

# Resolve directory aliases, not the final python symlink: two venvs can
# point to the same system binary while having different sys.prefix values.
def identity(value):
    path = Path(value)
    return path.parent.resolve(), path.name.rstrip('0123456789.')

expected = identity(sys.argv[1])
keys = {'Python3_EXECUTABLE', 'Python_EXECUTABLE', 'PYTHON_EXECUTABLE',
        '_Python3_EXECUTABLE', '_Python_EXECUTABLE'}
stale = []
def failed(error):
    raise error
for base in sys.argv[2:]:
    if not os.path.lexists(base):
        continue
    for directory, _, files in os.walk(base, onerror=failed, followlinks=False):
        if 'CMakeCache.txt' not in files:
            continue
        cache = Path(directory) / 'CMakeCache.txt'
        for line in cache.read_text().splitlines():
            key, separator, value = line.partition('=')
            if separator and key.split(':')[0] in keys and value.startswith('/'):
                if identity(value) != expected:
                    stale.append(f'{cache}: {key}={value}')
if stale:
    print('\n'.join(sorted(stale)), file=sys.stderr)
    sys.exit(1)
