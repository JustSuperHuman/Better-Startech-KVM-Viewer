"""Assemble the checked PNG size variants into a Windows ICO without dependencies."""
import argparse
import pathlib
import struct


ROOT = pathlib.Path(__file__).resolve().parent.parent


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--output', type=pathlib.Path, default=ROOT / 'assets/icons/cart.ico')
    args = parser.parse_args()
    sizes = (16, 20, 24, 32, 48, 64, 128, 256)
    images = []
    for size in sizes:
        path = ROOT / 'assets/icons' / (str(size) + '/cart.png' if size != 256 else 'cart.png')
        data = path.read_bytes()
        if data[:8] != b'\x89PNG\r\n\x1a\n' or struct.unpack('>II', data[16:24]) != (size, size):
            raise ValueError('Invalid icon dimensions: ' + str(path))
        if data[24:26] != bytes((8, 6)):
            raise ValueError('Icon must use 8-bit RGBA: ' + str(path))
        images.append(data)
    offset = 6 + len(sizes) * 16
    header = struct.pack('<HHH', 0, 1, len(sizes))
    for size, data in zip(sizes, images):
        header += struct.pack('<4B2H2I', size % 256, size % 256, 0, 0, 1, 32, len(data), offset)
        offset += len(data)
    with args.output.open('xb') as output:
        output.write(header + b''.join(images))
    print('ICO_CREATED:', args.output, '; sizes:', ', '.join(map(str, sizes)))


if __name__ == '__main__':
    main()
