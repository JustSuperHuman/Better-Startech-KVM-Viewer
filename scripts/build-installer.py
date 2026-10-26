"""Stage the complete installer payload and build one setup program.

Run with Python 3 -I after release builds of the viewer and helper. Refuses to
replace existing output. The payload contains StarTech's adapter components
(driver and runtime) and Microsoft's VC++ 2008 redistributable; keep the
resulting setup to machines covered by the StarTech license.
"""
import argparse
import hashlib
import importlib.util
import json
import os
import pathlib
import shutil
import subprocess
import tempfile

ROOT = pathlib.Path(__file__).resolve().parent.parent
VENDOR = pathlib.Path(r'C:\Program Files (x86)\USB Crash Cart Adapter')
FPGA_SHA256 = 'c32c853b376aaf6db53cb2a972218c5f69e347c28a2e5bc98601b1d6f246fadc'
VCREDIST_SHA256 = '8742bcbf24ef328a72d2a27b693cc7071e38d3bb4b9b44dec42aa3d2c8d61d92'
# Exactly what the helper loads: the embedded Python 2.7 runtime, its archive,
# the decoder, COM transport and the extension modules they import. No GUI,
# firmware (.fc5), recording or legacy setup files.
RUNTIME = [
    'python27.dll', 'library.zip', 'fbext_windows.pyd', 'usbiocom.dll',
    'pythoncom27.dll', 'pywintypes27.dll', '_ctypes.pyd', '_hashlib.pyd',
    '_socket.pyd', '_win32sysloader.pyd', 'bz2.pyd', 'pyexpat.pyd', 'select.pyd',
    'unicodedata.pyd', 'win32api.pyd', 'win32file.pyd', 'win32pipe.pyd',
    'win32trace.pyd', 'version.txt',
]
DRIVER = ['ulcvm.inf', 'ulcvm.sys', 'ulcvm_x64.sys', 'ulcvm.cat', 'ulcvm_x64.cat']


def load_package_module():
    spec = importlib.util.spec_from_file_location('cart_package', ROOT / 'scripts' / 'package.py')
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def sha256(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def stage(payload, vcredist, package):
    viewer = ROOT / 'target/x86_64-pc-windows-msvc/release/cart-viewer.exe'
    helper = ROOT / 'target/i686-pc-windows-msvc/release/cart-device-helper.exe'
    package.check_machine(viewer, 0x8664)
    package.check_machine(helper, 0x014c)
    shutil.copy2(viewer, payload / viewer.name)
    shutil.copy2(helper, payload / helper.name)
    runtime = payload / 'runtime'
    (runtime / 'data').mkdir(parents=True)
    for name in RUNTIME:
        shutil.copy2(VENDOR / name, runtime / name)
    fpga = VENDOR / 'data' / 'ulcvm.fgz'
    if sha256(fpga) != FPGA_SHA256:
        raise ValueError('Installed FPGA configuration differs from the verified version')
    shutil.copy2(fpga, runtime / 'data' / fpga.name)
    (payload / 'driver').mkdir()
    for name in DRIVER:
        shutil.copy2(VENDOR / 'Drivers' / name, payload / 'driver' / name)
    if sha256(vcredist) != VCREDIST_SHA256:
        raise ValueError('vcredist_x86.exe is not the verified Microsoft VC++ 2008 SP1 package')
    (payload / 'vcredist').mkdir()
    shutil.copy2(vcredist, payload / 'vcredist' / 'vcredist_x86.exe')
    shutil.copy2(VENDOR / 'license.txt', payload / 'STARTECH-LICENSE.txt')
    shutil.copy2(ROOT / 'README.md', payload / 'README.md')
    shutil.copytree(ROOT / 'docs', payload / 'docs')
    package.copy_notices(payload, package.dependencies())
    notices = payload / 'THIRD-PARTY-NOTICES.txt'
    notices.write_text(notices.read_text(encoding='utf-8') + '\n'.join([
        '',
        'Adapter components (not open source)',
        '  StarTech.com USB Crash Cart Adapter ' + (VENDOR / 'version.txt').read_text().split()[0]
        + ': runtime, decoder and COM transport under runtime\\.',
        '  Digital Multitools ULC driver 2.80 under driver\\.',
        '  Licensed by StarTech.com; see STARTECH-LICENSE.txt.',
        '',
        'Microsoft Visual C++ 2008 SP1 Redistributable (x86), installed only when missing.',
        '',
    ]), encoding='utf-8')
    hashes = {str(path.relative_to(payload)).replace('\\', '/'): sha256(path)
              for path in sorted(payload.rglob('*')) if path.is_file()}
    (payload / 'SHA256SUMS.json').write_text(json.dumps(hashes, indent=2) + '\n', encoding='utf-8')
    return hashes


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--vcredist', type=pathlib.Path, required=True,
                        help='Verified Microsoft vcredist_x86.exe (VC++ 2008 SP1)')
    parser.add_argument('--output', type=pathlib.Path, required=True,
                        help='New folder for the setup program')
    args = parser.parse_args()
    output = args.output.resolve()
    if output.exists():
        parser.error('Refusing to replace existing output: ' + str(output))
    package = load_package_module()
    with tempfile.TemporaryDirectory(prefix='cart-installer-', dir=ROOT / 'target') as temporary:
        payload = pathlib.Path(temporary) / 'payload'
        payload.mkdir()
        hashes = stage(payload, args.vcredist.resolve(), package)
        environment = dict(os.environ, CART_INSTALLER_PAYLOAD=str(payload), CARGO_NET_OFFLINE='true')
        subprocess.check_call(['cargo', 'build', '--locked', '--release', '--manifest-path',
                               str(ROOT / 'Cargo.toml'), '-p', 'cart-installer',
                               '--target', 'x86_64-pc-windows-msvc'], cwd=ROOT, env=environment)
        setup = ROOT / 'target/x86_64-pc-windows-msvc/release/cart-setup.exe'
        package.check_machine(setup, 0x8664)
        output.mkdir(parents=True)
        version = load_version()
        target = output / ('USB Crash Cart Setup ' + version + '.exe')
        shutil.copy2(setup, target)
        (output / 'PAYLOAD-SHA256SUMS.json').write_text(json.dumps(hashes, indent=2) + '\n', encoding='utf-8')
    digest = sha256(target)
    (output / 'SHA256.txt').write_text(digest + '  ' + target.name + '\n', encoding='ascii')
    print('SETUP:', target)
    print('Bytes:', target.stat().st_size, '; payload files:', len(hashes), '; SHA256:', digest)


def load_version():
    for line in (ROOT / 'Cargo.toml').read_text(encoding='utf-8').splitlines():
        if line.startswith('version = '):
            return line.split('"')[1]
    raise ValueError('Workspace version not found')


if __name__ == '__main__':
    main()
