"""Package the local Windows development build; never includes vendor binaries.
Run with Python 3 -I. Refuses to replace an existing output directory.
"""
import argparse
import hashlib
import json
import pathlib
import shutil
import struct
import subprocess
import tempfile


ROOT = pathlib.Path(__file__).resolve().parent.parent


def command(*arguments):
    return subprocess.check_output(arguments, cwd=ROOT, text=True, encoding='utf-8')


def check_machine(path, expected):
    with path.open('rb') as source:
        if source.read(2) != b'MZ':
            raise ValueError('Not a Windows executable: ' + str(path))
        source.seek(0x3c)
        offset = struct.unpack('<I', source.read(4))[0]
        source.seek(offset)
        if source.read(4) != b'PE\0\0' or struct.unpack('<H', source.read(2))[0] != expected:
            raise ValueError('Wrong executable architecture: ' + str(path))


def dependencies():
    packages = {}
    for target in ('x86_64-pc-windows-msvc', 'i686-pc-windows-msvc'):
        metadata = json.loads(command('cargo', 'metadata', '--offline', '--locked',
                                      '--manifest-path', str(ROOT / 'Cargo.toml'),
                                      '--format-version', '1', '--filter-platform', target))
        for package in metadata['packages']:
            if package['source'] is not None:
                packages[package['id']] = package
    return sorted(packages.values(), key=lambda package: (package['name'], package['version']))


def copy_notices(directory, packages):
    lines = [
        'Third-party dependency notices',
        '',
        'Local development build. Vendor software is required but is not included.',
        'This inventory includes resolved Windows dependencies and build dependencies;',
        'not every listed package is necessarily linked into the final executables.',
        'License expressions and repository URLs come from Cargo package metadata.',
        'Available upstream license/copyright/notice files are copied under licenses/.',
        '',
        'Inter: Copyright (c) 2016 The Inter Project Authors. SIL OFL 1.1.',
        'See licenses/Inter-OFL.txt.',
        '',
    ]
    licenses = directory / 'licenses'
    licenses.mkdir()
    shutil.copy2(ROOT / 'assets/fonts/inter-license/OFL.txt', licenses / 'Inter-OFL.txt')
    for package in packages:
        label = package['name'] + '-' + package['version']
        source = pathlib.Path(package['manifest_path']).parent
        files = set()
        for pattern in ('**/LICENSE*', '**/LICENCE*', '**/COPYING*', '**/COPYRIGHT*',
                        '**/NOTICE*', '**/UNLICENSE*', '**/OFL*', '**/*-LICENSE*',
                        '**/*-LICENCE*', '**/fonts/*.txt', '**/licenses/**/*'):
            files.update(path for path in source.glob(pattern) if path.is_file())
        if package.get('license_file'):
            explicit = (source / package['license_file']).resolve()
            if not explicit.is_relative_to(source.resolve()):
                raise ValueError('License path escapes dependency directory: ' + label)
            if explicit.is_file():
                files.add(explicit)
        copied = []
        if files:
            output = licenses / label
            output.mkdir()
            for path in sorted(files):
                relative = path.relative_to(source)
                destination = output / relative
                destination.parent.mkdir(parents=True, exist_ok=True)
                shutil.copy2(path, destination)
                copied.append('licenses/' + label + '/' + relative.as_posix())
        lines.extend([
            label,
            '  License: ' + (package.get('license') or 'See upstream license file'),
            '  Repository: ' + (package.get('repository') or 'Not declared'),
            '  Files: ' + (', '.join(copied) if copied else 'No top-level license file in cached package'),
            '',
        ])
    sysroot = pathlib.Path(command('rustc', '--print', 'sysroot').strip())
    rust_notices = sysroot / 'share/doc/rust'
    rust_files = [path for name in ('LICENSE-APACHE', 'LICENSE-MIT', 'COPYRIGHT')
                  if (path := rust_notices / name).is_file()]
    if rust_files:
        output = licenses / 'Rust'
        output.mkdir()
        for path in rust_files:
            shutil.copy2(path, output / path.name)
    lines.extend(['Rust standard library: MIT OR Apache-2.0.',
                  'https://github.com/rust-lang/rust/tree/master/library', ''])
    (directory / 'THIRD-PARTY-NOTICES.txt').write_text('\n'.join(lines), encoding='utf-8')


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--output', type=pathlib.Path, default=ROOT / 'dist')
    parser.add_argument('--dry-run', action='store_true', help='Validate inputs and preview without writing files')
    args = parser.parse_args()
    output = args.output.resolve()
    if output.exists():
        parser.error('Refusing to replace existing output: ' + str(output))
    if not output.parent.is_dir():
        parser.error('Output parent directory must already exist')
    executables = [
        (ROOT / 'target/x86_64-pc-windows-msvc/release/cart-viewer.exe', 0x8664),
        (ROOT / 'target/i686-pc-windows-msvc/release/cart-device-helper.exe', 0x014c),
    ]
    for source, machine in executables:
        check_machine(source, machine)
    packages = dependencies()
    if args.dry_run:
        print('OUTPUT:', output)
        for source, _ in executables:
            print('INPUT:', source, '; bytes:', source.stat().st_size)
        print('Dependency notice entries:', len(packages))
        print('Includes README, docs, Inter license, dependency notices and checksums.')
        print('Vendor files included: none. No files were written.')
        return
    with tempfile.TemporaryDirectory(prefix='.cart-package-', dir=output.parent) as temporary:
        staging = pathlib.Path(temporary) / 'package'
        staging.mkdir()
        for source, _ in executables:
            shutil.copy2(source, staging / source.name)
        shutil.copy2(ROOT / 'README.md', staging / 'README.md')
        shutil.copytree(ROOT / 'docs', staging / 'docs')
        copy_notices(staging, packages)
        (staging / 'Start Viewer.cmd').write_text(
            '@echo off\r\nstart "USB Crash Cart" "%~dp0cart-viewer.exe" --connect\r\n',
            encoding='ascii', newline='')
        hashes = {str(path.relative_to(staging)).replace('\\', '/'): hashlib.sha256(path.read_bytes()).hexdigest()
                  for path in sorted(staging.rglob('*')) if path.is_file()}
        (staging / 'SHA256SUMS.json').write_text(json.dumps(hashes, indent=2) + '\n', encoding='utf-8')
        staging.rename(output)
    print('PACKAGED:', output)
    print('Executables: x64 viewer, x86 helper; vendor files included: none')
    print('Dependency notice entries:', len(packages))
    print('Paste remains experimental; read docs/compatibility.md before use.')


if __name__ == '__main__':
    main()
