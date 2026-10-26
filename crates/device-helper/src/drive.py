# Disk images the adapter presents to the target as a USB drive or CD-ROM.
# Behaviour follows the original Adapter.ftInsert/ftRead/ftWrite and
# vhdfile.VhdFile; the image formats follow the published VHD specification.
from __future__ import print_function
import struct

DRIVE_DISK = 1   # vsproto.FT_MEDIA_DISK: 512-byte blocks
DRIVE_CDROM = 2  # vsproto.FT_MEDIA_CDROM: 2048-byte blocks
# The adapter asks for at most a few hundred KiB at once; anything larger is
# treated as a protocol fault rather than allocated.
MAX_TRANSFER = 16 * 1024 * 1024
UNALLOCATED = 0xffffffff
VHD_FOOTER = '>8sIIQIIIIQQIII16sB'
VHD_HEADER = '>8sQQIIII'


class DriveError(Exception):
    """A message for the person attaching the image; never a device fault."""


class ImageFile(object):
    """Positioned I/O on one handle that denies other writers while attached."""

    def __init__(self, transport, path, writable):
        C, k = transport.C, transport.k
        access = 0x80000000 | (0x40000000 if writable else 0)
        # FILE_SHARE_READ only: no other program may change sectors the target reads.
        handle = k.CreateFileW(path, access, 1, None, 3, 0x80, None)
        if handle is None or handle == C.c_void_p(-1).value:
            error = C.get_last_error()
            if error in (2, 3):
                raise DriveError('The disk image could not be found.')
            if error in (5, 32, 33):
                raise DriveError('The disk image is open in another program, or Windows denied access to it.')
            raise DriveError('Windows could not open the disk image (error %d).' % error)
        self.C, self.k, self.handle = C, k, handle
        self.Overlapped, self.DWORD = transport.Overlapped, transport.W.DWORD
        size = C.c_longlong(0)
        if not k.GetFileSizeEx(handle, C.byref(size)):
            error = C.get_last_error()
            self.close()
            raise DriveError('Windows could not read the disk image size (error %d).' % error)
        self.size = size.value

    def _overlapped(self, offset):
        ov = self.Overlapped()
        ov.Offset = offset & 0xffffffff
        ov.OffsetHigh = offset >> 32
        return ov

    def pread(self, offset, length):
        # Short only at end of file; the caller pads.
        C = self.C
        parts, done = [], 0
        while done < length:
            want = min(length - done, 1 << 20)
            buffer = C.create_string_buffer(want)
            count = self.DWORD(0)
            ov = self._overlapped(offset + done)
            if not self.k.ReadFile(self.handle, buffer, want, C.byref(count), C.byref(ov)):
                error = C.get_last_error()
                if error == 38:  # ERROR_HANDLE_EOF
                    break
                raise IOError('Disk image read failed (error %d)' % error)
            if not count.value:
                break
            parts.append(buffer.raw[:count.value])
            done += count.value
        return ''.join(parts)

    def pwrite(self, offset, data):
        C = self.C
        done = 0
        while done < len(data):
            chunk = data[done:done + (1 << 20)]
            count = self.DWORD(0)
            ov = self._overlapped(offset + done)
            if not self.k.WriteFile(self.handle, chunk, len(chunk), C.byref(count), C.byref(ov)) or not count.value:
                raise IOError('Disk image write failed (error %d)' % C.get_last_error())
            done += count.value

    def flush(self):
        self.k.FlushFileBuffers(self.handle)

    def close(self):
        if self.handle is not None:
            self.k.FlushFileBuffers(self.handle)
            self.k.CloseHandle(self.handle)
            self.handle = None


class FlatImage(object):
    """Raw disk images, ISO 9660 images and fixed VHDs: sectors stored in order."""

    def __init__(self, file, base, size, media, writable, label):
        self.file, self.base, self.size = file, base, size
        self.media, self.writable, self.label = media, writable, label
        self.block = 2048 if media == DRIVE_CDROM else 512
        self.blocks = size // self.block

    def read(self, offset, length):
        data = ''
        if offset < self.size:
            data = self.file.pread(self.base + offset, min(length, self.size - offset))
        return data + '\x00' * (length - len(data))

    def write(self, offset, data):
        if not self.writable or offset >= self.size:
            return
        self.file.pwrite(self.base + offset, data[:self.size - offset])

    def close(self):
        self.file.close()


def vhd_checksum(data, start):
    total = sum(bytearray(data[:start])) + sum(bytearray(data[start + 4:]))
    return ~total & 0xffffffff


def read_footer(file, offset):
    raw = file.pread(offset, 512)
    if len(raw) != 512 or raw[:8] != 'conectix':
        return None
    fields = struct.unpack(VHD_FOOTER, raw[:85])
    if fields[12] != vhd_checksum(raw, 64):
        raise DriveError('This VHD file is damaged: its footer checksum does not match.')
    return raw, fields


class DynamicVhd(object):
    """Dynamic VHD: a block allocation table maps blocks; unwritten blocks read as zero."""

    def __init__(self, file, footer, fields):
        self.file, self.footer, self.media, self.writable = file, footer, DRIVE_DISK, True
        self.block, self.label = 512, 'vhd'
        self.size = fields[9]
        header = file.pread(fields[3], 1024)
        if len(header) != 1024 or header[:8] != 'cxsparse':
            raise DriveError('This VHD file is damaged: its dynamic disk header is missing.')
        cookie, _, table_offset, version, entries, block_size, checksum = struct.unpack(VHD_HEADER, header[:40])
        if checksum != vhd_checksum(header, 36):
            raise DriveError('This VHD file is damaged: its dynamic header checksum does not match.')
        if block_size < 512 or block_size & (block_size - 1) or block_size > (256 << 20):
            raise DriveError('This VHD file uses an unsupported block size.')
        if entries > (1 << 22) or self.size > entries * block_size:
            raise DriveError('This VHD file has an inconsistent allocation table.')
        self.block_size = block_size
        self.sectors_per_block = block_size // 512
        self.bitmap_size = ((self.sectors_per_block + 7) // 8 + 511) // 512 * 512
        self.table_offset = table_offset
        table = file.pread(table_offset, entries * 4)
        if len(table) != entries * 4:
            raise DriveError('This VHD file is truncated: its allocation table is incomplete.')
        self.table = list(struct.unpack('>%dI' % entries, table))
        self.bitmaps = {}
        self.blocks = self.size // 512
        # New blocks go where the trailing footer is; it moves to the new end.
        self.footer_offset = file.size - 512

    def bitmap(self, index):
        bitmap = self.bitmaps.get(index)
        if bitmap is None:
            bitmap = bytearray(self.file.pread(self.table[index] * 512, self.bitmap_size))
            bitmap.extend('\x00' * (self.bitmap_size - len(bitmap)))
            self.bitmaps[index] = bitmap
        return bitmap

    def spans(self, offset, length):
        # (block index, offset within block, length) pieces of one transfer.
        end = min(offset + length, self.size)
        while offset < end:
            index, within = divmod(offset, self.block_size)
            count = min(end - offset, self.block_size - within)
            yield index, within, count
            offset += count

    def read(self, offset, length):
        parts = []
        for index, within, count in self.spans(offset, length):
            if self.table[index] == UNALLOCATED:
                parts.append('\x00' * count)
                continue
            data = self.file.pread(self.table[index] * 512 + self.bitmap_size + within, count)
            data = bytearray(data + '\x00' * (count - len(data)))
            bitmap = self.bitmap(index)
            # VHD sector bitmaps are most-significant bit first; clear sectors read as zero.
            first, last = within // 512, (within + count - 1) // 512
            for sector in xrange(first, last + 1):
                if not bitmap[sector >> 3] & (0x80 >> (sector & 7)):
                    start = max(sector * 512, within) - within
                    stop = min((sector + 1) * 512, within + count) - within
                    data[start:stop] = '\x00' * (stop - start)
            parts.append(str(data))
        data = ''.join(parts)
        return data + '\x00' * (length - len(data))

    def allocate(self, index):
        # Data and the moved footer are written before the table points at them.
        position = self.footer_offset
        self.file.pwrite(position, '\x00' * (self.bitmap_size + self.block_size))
        self.file.pwrite(position + self.bitmap_size + self.block_size, self.footer)
        self.file.pwrite(self.table_offset + index * 4, struct.pack('>I', position // 512))
        self.table[index] = position // 512
        self.footer_offset = position + self.bitmap_size + self.block_size
        self.bitmaps[index] = bytearray(self.bitmap_size)

    def write(self, offset, data):
        done = 0
        for index, within, count in self.spans(offset, len(data)):
            if self.table[index] == UNALLOCATED:
                self.allocate(index)
            base = self.table[index] * 512
            self.file.pwrite(base + self.bitmap_size + within, data[done:done + count])
            done += count
            bitmap = self.bitmap(index)
            before = str(bitmap)
            for sector in xrange(within // 512, (within + count - 1) // 512 + 1):
                bitmap[sector >> 3] |= 0x80 >> (sector & 7)
            if str(bitmap) != before:
                self.file.pwrite(base, str(bitmap))

    def close(self):
        self.file.close()


def open_drive(transport, path, kind):
    """kind is the viewer's identification; every format is checked again here."""
    if kind == 'iso':
        file = ImageFile(transport, path, False)
        try:
            if file.size < 34816 or file.pread(32769, 5) != 'CD001':
                raise DriveError('This file is not an ISO 9660 disc image.')
            return FlatImage(file, 0, file.size, DRIVE_CDROM, False, 'iso')
        except Exception:
            file.close()
            raise
    if kind not in ('raw', 'vhd'):
        raise DriveError('Unsupported disk image type.')
    file = ImageFile(transport, path, True)
    try:
        if kind == 'raw':
            if file.size < 65536:
                raise DriveError('The disk image is too small to be a drive.')
            if file.size // 512 > 0xffffffff:
                raise DriveError('The disk image is larger than 2 TB.')
            return FlatImage(file, 0, file.size, DRIVE_DISK, True, 'raw')
        trailing = read_footer(file, file.size - 512) if file.size % 512 == 0 else None
        found = trailing or read_footer(file, 0)
        if found is None:
            raise DriveError('This file is not a VHD disk image.')
        raw, fields = found
        disk_type, size = fields[11], fields[9]
        if disk_type in (2, 3) and trailing is None:
            # Growth appends blocks where the trailing footer belongs.
            raise DriveError('This VHD file is truncated: its trailing footer is missing.')
        if size < 65536 or size % 512 or size // 512 > 0xffffffff:
            raise DriveError('This VHD file reports an unsupported disk size.')
        if disk_type == 2:
            if size > file.size - 512:
                raise DriveError('This VHD file is truncated.')
            return FlatImage(file, 0, size, DRIVE_DISK, True, 'vhd')
        if disk_type == 3:
            return DynamicVhd(file, raw, fields)
        if disk_type == 4:
            raise DriveError('Differencing VHD files are not supported. Use a fixed or dynamic VHD.')
        raise DriveError('This VHD file uses an unknown disk type.')
    except Exception:
        file.close()
        raise


def make_footer(size, disk_type, data_offset):
    footer = struct.pack(VHD_FOOTER, 'conectix', 2, 0x00010000, data_offset, 0, 0x63617274,
                         0x00010000, 0x5769326b, size, size, 0, disk_type, 0, '\x01' * 16, 0)
    footer += '\x00' * (512 - len(footer))
    return footer[:64] + struct.pack('>I', vhd_checksum(footer, 64)) + footer[68:]


def drive_selftest(directory):
    # Never opens the adapter: exercises every backend on files in a new directory.
    import os
    transport = Transport()
    os.mkdir(directory)
    checks = []

    def roundtrip(image, label):
        pattern = ''.join(chr((i * 7 + 3) & 255) for i in xrange(3 * 512 + 100))
        for offset in (0, 512 * 9, image.size - 1536):
            image.write(offset, pattern[:1536])
            if image.read(offset, 1536) != pattern[:1536]:
                raise AssertionError(label + ': read-back mismatch at %d' % offset)
        if image.read(image.size - 512, 1024)[512:] != '\x00' * 512:
            raise AssertionError(label + ': reads past the end must be zero')
        checks.append(label)

    raw = os.path.join(directory, 'raw.img')
    with open(raw, 'wb') as output:
        output.truncate(1 << 20)
    image = open_drive(transport, unicode(raw), 'raw')
    roundtrip(image, 'raw')
    image.close()

    fixed = os.path.join(directory, 'fixed.vhd')
    with open(fixed, 'wb') as output:
        output.truncate(1 << 20)
        output.seek(1 << 20)
        output.write(make_footer(1 << 20, 2, 0xffffffffffffffff))
    image = open_drive(transport, unicode(fixed), 'vhd')
    roundtrip(image, 'fixed vhd')
    image.close()

    # Dynamic VHD with 64 KiB blocks: footer copy, header, table, footer.
    size, block = 4 << 20, 65536
    entries = size // block
    dynamic = os.path.join(directory, 'dynamic.vhd')
    header = struct.pack(VHD_HEADER, 'cxsparse', 0xffffffffffffffff, 1536, 0x00010000, entries, block, 0)
    header += '\x00' * (1024 - len(header))
    header = header[:36] + struct.pack('>I', vhd_checksum(header, 36)) + header[40:]
    footer = make_footer(size, 3, 512)
    with open(dynamic, 'wb') as output:
        output.write(footer + header + '\xff' * (entries * 4))
        output.write('\x00' * ((-output.tell()) % 512))
        output.write(footer)
    image = open_drive(transport, unicode(dynamic), 'vhd')
    if image.read(70000, 4096) != '\x00' * 4096:
        raise AssertionError('dynamic vhd: unallocated blocks must read as zero')
    roundtrip(image, 'dynamic vhd')
    # A span across a block boundary allocates both blocks.
    image.write(block - 512, 'B' * 1024)
    image.close()
    image = open_drive(transport, unicode(dynamic), 'vhd')
    if image.read(block - 512, 1024) != 'B' * 1024:
        raise AssertionError('dynamic vhd: data must survive reopening')
    stale = image.table[1] * 512 + image.bitmap_size + 512
    image.close()
    # Sectors whose bitmap bit is clear read as zero whatever the block holds.
    with open(dynamic, 'r+b') as output:
        output.seek(stale)
        output.write('G' * 512)
    image = open_drive(transport, unicode(dynamic), 'vhd')
    if image.read(block + 512, 512) != '\x00' * 512:
        raise AssertionError('dynamic vhd: sectors with a clear bitmap bit must read as zero')
    if image.size != size:
        raise AssertionError('dynamic vhd: footer must move to the new end')
    image.close()
    checks.append('dynamic vhd reopen and sector bitmap')

    iso = os.path.join(directory, 'disc.iso')
    with open(iso, 'wb') as output:
        output.write('\x00' * 32768 + '\x01CD001\x01' + '\x00' * (2048 * 4 - 7))
    image = open_drive(transport, unicode(iso), 'iso')
    if image.media != DRIVE_CDROM or image.block != 2048 or image.blocks != 20:
        raise AssertionError('iso: must be a 2048-byte CD-ROM')
    image.write(0, 'X' * 2048)
    if image.read(0, 16) != '\x00' * 16:
        raise AssertionError('iso: CD-ROM media must never be written')
    image.close()
    checks.append('iso')

    for kind, name, message in (('vhd', raw, 'not a VHD'), ('iso', raw, 'not an ISO')):
        try:
            open_drive(transport, unicode(name), kind).close()
        except DriveError:
            checks.append('rejects ' + message)
        else:
            raise AssertionError('accepted a file that is ' + message)
    exclusive = open_drive(transport, unicode(raw), 'raw')
    try:
        open(raw, 'r+b').close()
    except IOError:
        checks.append('denies other writers while attached')
    else:
        raise AssertionError('another writer opened an attached image')
    finally:
        exclusive.close()
    print('DRIVE_SELFTEST_OK:', ', '.join(checks))
