# Trusted bridge code. Never import legacy application/transport startup.
from __future__ import print_function
import sys

config = __cart_config
GUID = '{B50CA981-518E-4a24-9759-07234F0AC6EE}'


def load_decoder():
    import imp
    return imp.load_dynamic('fbext', config['vendor_dir'] + '\\fbext_windows.pyd')


def runtime_info():
    decoder = load_decoder()
    print('Python:', sys.version.replace('\n', ' '))
    print('Import path:', repr(sys.path))
    print('Decoder:', decoder.__file__)
    decoder.createBuffer(1920, 1600)
    rgba, size = decoder.toRGBString(0, 0, 1, 1, 1.0)
    if tuple(size) != (16, 16) or len(rgba) != 1024:
        raise ValueError('Unexpected decoder tile output shape')
    print('Decoder allocation/readout: one 16x16 RGBA tile,', len(rgba), 'bytes')
    import usbcon.usbio as usbio
    print('COM class:', usbio.USBIOInterface3.CLSID)
    for name in ('CyclePort', 'ResetDevice', 'UnconfigureDevice'):
        method = getattr(usbio.IUSBIOInterface3, name, None)
        if method is not None:
            code = method.func_code
            print('COM method:', name, code.co_varnames[:code.co_argcount], repr(code.co_consts))
    for name in dir(usbio.constants):
        value = getattr(usbio.constants, name)
        if 'TIMEOUT' in name or 'ACQUIRED' in name or value == -536805376:
            print('Vendor constant:', name, value, ('0x%08X' % (value & 0xffffffff)) if isinstance(value, (int, long)) else '')
    print('RUNTIME_OK: decoder and generated COM declarations loaded; no device opened')


class Transport(object):
    """Own cancelable endpoint I/O without invoking legacy recovery routines."""
    def __init__(self):
        import ctypes as C
        # The vendor archive omits ctypes.wintypes; define only the ABI types used.
        class W(object):
            DWORD = C.c_uint32
            HANDLE = C.c_void_p
            BOOL = C.c_int
            LPCWSTR = C.c_wchar_p
        self.C = C
        self.W = W
        self.k = C.WinDLL('kernel32', use_last_error=True)
        self.handles = []
        self.sync_handles = set()
        import collections
        self.submitted_buffers = collections.deque(maxlen=1024)
        self.data_buffers = collections.deque(maxlen=4)
        self.dev = None
        # File-transfer pipes (original device.ftx/frx); None when unavailable.
        self.data_out = None
        self.data_in = None
        self.opened = False
        self.acquired = False
        import threading
        self.write_lock = threading.Lock()

        class Overlapped(C.Structure):
            _fields_ = [('Internal', C.c_size_t), ('InternalHigh', C.c_size_t),
                        ('Offset', W.DWORD), ('OffsetHigh', W.DWORD), ('hEvent', W.HANDLE)]
        self.Overlapped = Overlapped
        ovptr = C.POINTER(Overlapped)
        dwordptr = C.POINTER(W.DWORD)
        prototypes = {
            'CreateFileW': (W.HANDLE, [W.LPCWSTR, W.DWORD, W.DWORD, C.c_void_p, W.DWORD, W.DWORD, W.HANDLE]),
            'CreateEventW': (W.HANDLE, [C.c_void_p, W.BOOL, W.BOOL, W.LPCWSTR]),
            'CloseHandle': (W.BOOL, [W.HANDLE]),
            'DeviceIoControl': (W.BOOL, [W.HANDLE, W.DWORD, C.c_void_p, W.DWORD, C.c_void_p, W.DWORD, dwordptr, ovptr]),
            'ReadFile': (W.BOOL, [W.HANDLE, C.c_void_p, W.DWORD, dwordptr, ovptr]),
            'WriteFile': (W.BOOL, [W.HANDLE, C.c_void_p, W.DWORD, dwordptr, ovptr]),
            'GetOverlappedResult': (W.BOOL, [W.HANDLE, ovptr, dwordptr, W.BOOL]),
            'CancelIoEx': (W.BOOL, [W.HANDLE, ovptr]),
            'WaitForSingleObject': (W.DWORD, [W.HANDLE, W.DWORD]),
            'GetTickCount64': (C.c_ulonglong, []),
            'GetCurrentThreadId': (W.DWORD, []),
            'OpenThread': (W.HANDLE, [W.DWORD, W.BOOL, W.DWORD]),
            'CancelSynchronousIo': (W.BOOL, [W.HANDLE]),
            'GetFileSizeEx': (W.BOOL, [W.HANDLE, C.POINTER(C.c_longlong)]),
            'FlushFileBuffers': (W.BOOL, [W.HANDLE]),
        }
        for name, (restype, argtypes) in prototypes.items():
            function = getattr(self.k, name)
            function.restype = restype
            function.argtypes = argtypes

    def now(self):
        return self.k.GetTickCount64() / 1000.0

    def check_status(self, label, status):
        if status != 0:
            raise RuntimeError('%s failed with vendor status %r' % (label, status))

    def open_control(self):
        import usbcon.usbio as usbio
        import struct
        self.dev = usbio.USBIOInterface3()
        count = self.dev.EnumerateDevices(GUID)
        deadline = self.now() + 5
        while count == 0 and self.now() < deadline:
            import time
            time.sleep(0.1)
            count = self.dev.EnumerateDevices(GUID)
        print('Adapter interfaces:', count)
        if count != 1:
            raise RuntimeError('Expected exactly one adapter; no device selected automatically')
        self.check_status('OpenDevice', self.dev.OpenDevice(0))
        self.opened = True
        descriptor, length, status = self.dev.GetDeviceDescriptor(buffer('x' * 18), 18)
        self.check_status('GetDeviceDescriptor', status)
        if length != 18:
            raise RuntimeError('Invalid USB device descriptor length')
        fields = struct.unpack('<2BH4B3H4B', descriptor[:18])
        if fields[7:9] != (0x152a, 0x8463):
            raise RuntimeError('Refusing unexpected USB device %04x:%04x' % fields[7:9])
        print('Verified adapter: 152A:8463')

    def open(self):
        self.open_control()
        path = self.dev.DevicePathName
        self.check_status('AcquireDevice', self.dev.AcquireDevice())
        self.acquired = True
        self.check_status('AddInterface', self.dev.AddInterface(0, 0, 64512))
        self.check_status('SetConfiguration', self.dev.SetConfiguration(1))
        self.command = self.bind(path, 0x04, 0x40000000, 2, True)
        self.response = self.bind(path, 0x83, 0x80000000, 1, True)
        self.video = self.bind(path, 0x82, 0x80000000, 1, False)
        # Generation-2 adapters carry the virtual drive on endpoints 0x05/0x85
        # (original usbcon.win32usb.device, hwver 2). Video works without them.
        try:
            self.data_out = self.bind(path, 0x05, 0x40000000, 2, True)
            self.data_in = self.bind(path, 0x85, 0x80000000, 1, False)
        except Exception as error:
            print('File-transfer endpoints unavailable:', error)
            self.data_out = self.data_in = None
        print('Endpoints acquired: vendor-compatible synchronous I/O on owned worker threads')

    def bind(self, path, endpoint, access, share, no_buffering):
        import struct
        # Preserve the vendor's synchronous pipe ABI. Worker reads are canceled
        # by their owned thread handles, never by freeing a pending buffer.
        flags = 0x20000000 if no_buffering else 0
        handle = self.k.CreateFileW(unicode(path), access, share, None, 3, flags, None)
        if handle == self.C.c_void_p(-1).value or handle is None:
            raise self.C.WinError(self.C.get_last_error())
        self.sync_handles.add(handle)
        try:
            self.io(handle, 'ioctl', chr(endpoint), 1, 1000, 0x80942078)
            self.io(handle, 'ioctl', struct.pack('<I', 0x30000), 4, 1000, 0x80942090)
        except Exception:
            self.sync_handles.discard(handle)
            self.k.CloseHandle(handle)
            raise
        self.handles.append(handle)
        return handle

    def io(self, handle, kind, data, size, timeout_ms, ioctl=0):
        C, W = self.C, self.W
        if handle in self.sync_handles:
            out = C.create_string_buffer(max(size, 1))
            incoming = C.create_string_buffer(data, len(data)) if data else None
            if incoming is not None:
                # Keep a bounded submission tail alive across legacy-driver completions.
                # Drive transfers are large, so only their last few are kept.
                tail = self.data_buffers if handle == self.data_out else self.submitted_buffers
                tail.append(incoming)
            count = W.DWORD(0)
            if kind == 'write':
                ok = self.k.WriteFile(handle, incoming, len(data), C.byref(count), None)
            elif kind == 'ioctl':
                ok = self.k.DeviceIoControl(handle, ioctl, incoming, len(data), out, size, C.byref(count), None)
            else:
                ok = self.k.ReadFile(handle, out, size, C.byref(count), None)
            if not ok:
                error = C.get_last_error()
                if kind == 'read' and (error & 0xffffffff) in (995, 0xe0010000, 0xe000100c, 0xe0006000):
                    return None
                raise C.WinError(error)
            if count.value > (len(data) if kind == 'write' else size):
                raise RuntimeError('Invalid synchronous transfer length')
            return count.value if kind == 'write' else out.raw[:count.value]
        ov = self.Overlapped()
        ov.hEvent = self.k.CreateEventW(None, True, False, None)
        if not ov.hEvent:
            raise C.WinError(C.get_last_error())
        out = C.create_string_buffer(max(size, 1))
        incoming = C.create_string_buffer(data, len(data)) if data else None
        count = W.DWORD(0)
        try:
            if kind == 'read':
                ok = self.k.ReadFile(handle, out, size, C.byref(count), C.byref(ov))
            elif kind == 'write':
                ok = self.k.WriteFile(handle, incoming, len(data), C.byref(count), C.byref(ov))
            else:
                ok = self.k.DeviceIoControl(handle, ioctl, incoming, len(data), out, size, C.byref(count), C.byref(ov))
            error = C.get_last_error() if not ok else 0
            if not ok and error != 997:  # ERROR_IO_PENDING
                if (error & 0xffffffff) in (995, 0xe0010000, 0xe000100c, 0xe0006000):
                    return None
                raise C.WinError(error)
            if ok and self.k.GetOverlappedResult(handle, C.byref(ov), C.byref(count), False):
                # Completed operations need not wait for a pending-operation event.
                if count.value > (len(data) if kind == 'write' else size):
                    raise RuntimeError('Driver returned an impossible transfer length')
                return count.value if kind == 'write' else out.raw[:count.value]
            wait = self.k.WaitForSingleObject(ov.hEvent, timeout_ms)
            if wait == 258:  # WAIT_TIMEOUT
                self.k.CancelIoEx(handle, C.byref(ov))
                wait = self.k.WaitForSingleObject(ov.hEvent, 2000)
                if wait != 0:
                    # Never return/free buffers that a driver could still write into.
                    print('FATAL: driver did not acknowledge cancellation; terminating only this helper')
                    sys.stdout.flush()
                    import os
                    os._exit(86)
                completed = self.k.GetOverlappedResult(handle, C.byref(ov), C.byref(count), False)
                if completed:
                    return out.raw[:count.value] if kind != 'write' else count.value
                error = C.get_last_error()
                if (error & 0xffffffff) in (995, 0xe0010000):
                    # Windows ERROR_OPERATION_ABORTED or USBIO_ERR_CANCELED.
                    return None
                raise C.WinError(error)
            if wait != 0:
                raise C.WinError(C.get_last_error())
            if not self.k.GetOverlappedResult(handle, C.byref(ov), C.byref(count), False):
                error = C.get_last_error()
                if (error & 0xffffffff) in (995, 0xe0010000, 0xe000100c, 0xe0006000):
                    return None
                raise C.WinError(error)
            if count.value > (len(data) if kind == 'write' else size):
                raise RuntimeError('Driver returned an impossible transfer length')
            return count.value if kind == 'write' else out.raw[:count.value]
        finally:
            self.k.CloseHandle(ov.hEvent)

    def write(self, message):
        # This diagnostic bridge cannot send input or upload FPGA/firmware data.
        heartbeat = len(message) == 5 and message[0] == 'H'
        default_x = message == 'z\x01'
        # Volatile posX/posY steps only; never EEPROM saves or other settings.
        set_position = len(message) == 3 and message[0] == 'y' and message[1] in '\x01\x02'
        # VSP_FT_CON: media type (disk or CD-ROM) and block count. 'e' detaches.
        attach_drive = len(message) == 6 and message[0] == 'c' and message[1] in '\x01\x02'
        if (message not in ('v', 's', 'g', 'i', 'e') and not heartbeat and not default_x
                and not set_position and not attach_drive):
            raise ValueError('Command not permitted by video diagnostic allowlist')
        self._write(message)

    def initialize_fpga(self):
        # Approved operational initialization for verified VID/PID 152A:8463 only.
        # Never read .fc5 firmware files or expose an arbitrary upload operation.
        import gzip
        import hashlib
        import struct
        import cStringIO
        path = config['vendor_dir'] + '\\data\\ulcvm.fgz'
        with open(path, 'rb') as source:
            compressed = source.read(1000000)
        expected = 'c32c853b376aaf6db53cb2a972218c5f69e347c28a2e5bc98601b1d6f246fadc'
        if hashlib.sha256(compressed).hexdigest() != expected:
            raise ValueError('Installed FPGA configuration differs from the verified version')
        with gzip.GzipFile(fileobj=cStringIO.StringIO(compressed)) as source:
            raw = source.read(1000000)
        if len(raw) != 341160:
            raise ValueError('Invalid generation-2 FPGA configuration length')
        block_size = 507 if self.dev.IsOperatingAtHighSpeed else 59
        packet_format = '>HH%ds' % block_size
        sequence = 0
        offset = 0
        while True:
            block = raw[offset:offset + block_size]
            packet = 'f' + struct.pack(packet_format, sequence, len(block), block)
            self._write(packet)
            if not block:
                break
            offset += len(block)
            sequence += 1
        print('FPGA_WRITES_COMPLETE:', sequence, 'data blocks plus terminal; awaiting device acknowledgement')

    def send_input(self, packet):
        if not packet or not ((packet[0] == 'k' and len(packet) == 5) or (packet[0] == 'm' and len(packet) == 9)):
            raise ValueError('Invalid input packet shape')
        self._write(packet)

    def _write(self, message):
        with self.write_lock:
            count = self.io(self.command, 'write', message, 0, 1000)
            if count != len(message):
                raise RuntimeError('Incomplete command write: confirmed %r of %d bytes' % (count, len(message)))

    def write_data(self, data):
        # Sector data for one target read, sent whole as the original did.
        count = self.io(self.data_out, 'write', data, 0, 0)
        if count != len(data):
            raise RuntimeError('Incomplete drive data write: %r of %d bytes' % (count, len(data)))

    def read_data(self, size):
        return self.io(self.data_in, 'read', '', size, 0)

    def read_response(self, timeout=200):
        return self.io(self.response, 'read', '', 64, timeout)

    def read_video(self, timeout=500):
        return self.io(self.video, 'read', '', 2097152, timeout)

    def close(self):
        failures = []
        for handle in reversed(self.handles):
            if not self.k.CloseHandle(handle):
                failures.append('endpoint close failed')
        self.handles = []
        if self.dev is not None:
            if self.acquired:
                try:
                    self.check_status('ReleaseDevice', self.dev.ReleaseDevice())
                    self.acquired = False
                except Exception as error:
                    failures.append(str(error))
            if self.opened:
                try:
                    self.dev.CloseDevice()
                    self.opened = False
                except Exception as error:
                    failures.append(str(error))
            self.dev = None
        if failures:
            raise RuntimeError('Cleanup incomplete: ' + '; '.join(failures))
        print('DEVICE_CLOSED: this helper has no remaining endpoints or acquisition')


def parse_status(message):
    import struct
    if not message or message[0] != 'S':
        return None
    formats = {29: '>6B2H3BHLB9s', 35: '>6B2H3BHLB15s'}
    payload = message[1:]
    if len(payload) not in formats:
        raise ValueError('Unknown status payload length: %d' % len(payload))
    fields = struct.unpack(formats[len(payload)], payload)
    return dict(fpga_loaded=fields[0], fpga_powered=fields[1], keyboard_type=fields[2],
                keyboard_ready=fields[3], leds=fields[4], no_video=fields[5],
                width=fields[6], height=fields[7], hz=fields[8],
                pixels_per_clock=fields[9], saved_position=fields[10],
                phase=ord(fields[14][0]),
                position_x=ord(fields[14][1]) - (256 if ord(fields[14][1]) >= 128 else 0),
                position_y=ord(fields[14][2]) - (256 if ord(fields[14][2]) >= 128 else 0))


def save_frame(decoder, status, path):
    import os
    width, height = status['width'], status['height']
    if not (0 < width <= 1920 and 0 < height <= 1600):
        raise ValueError('Unsupported active dimensions')
    rgba, size = decoder.toRGBString(0, 0, (width + 15) // 16, (height + 15) // 16, 1.0)
    padded_width, padded_height = size
    if padded_width < width or padded_height < height or len(rgba) != padded_width * padded_height * 4:
        raise ValueError('Invalid decoded frame dimensions/length')
    rgb = bytearray(width * height * 3)
    pixels = bytearray(rgba)
    for y in xrange(height):
        row = pixels[y * padded_width * 4:y * padded_width * 4 + width * 4]
        start, end = y * width * 3, (y + 1) * width * 3
        rgb[start:end:3] = row[0::4]
        rgb[start + 1:end:3] = row[1::4]
        rgb[start + 2:end:3] = row[2::4]
    import struct
    import zlib
    def chunk(kind, data):
        return struct.pack('>I', len(data)) + kind + data + struct.pack('>I', zlib.crc32(kind + data) & 0xffffffff)
    scanlines = ''.join('\x00' + str(rgb[y * width * 3:(y + 1) * width * 3]) for y in xrange(height))
    png = '\x89PNG\r\n\x1a\n' + chunk('IHDR', struct.pack('>2I5B', width, height, 8, 2, 0, 0, 0))
    png += chunk('IDAT', zlib.compress(scanlines, 6)) + chunk('IEND', '')
    fd = os.open(path, os.O_CREAT | os.O_EXCL | os.O_WRONLY | os.O_BINARY, 0600)
    with os.fdopen(fd, 'wb') as output:
        output.write(png)
    print('FRAME_SAVED:', path, '%dx%d RGB PNG' % (width, height))


def probe():
    decoder = load_decoder()
    decoder.createBuffer(1920, 1600)
    transport = Transport()
    try:
        transport.open()
        transport.write('v')
        transport.write('s')
        status = None
        deadline = transport.now() + 5
        next_query = transport.now() + 1
        while transport.now() < deadline:
            if transport.now() >= next_query:
                transport.write('s')
                next_query = transport.now() + 1
            message = transport.read_response(800)
            if message and message[0] == 'H' and len(message) == 5:
                transport.write(message)
                print('Heartbeat acknowledged')
            parsed = parse_status(message)
            if parsed is not None:
                status = parsed
                break
            if message and message[0] == 'V':
                print('Version response:', len(message) - 1, 'payload bytes')
        if status is None:
            raise RuntimeError('No valid status response within deadline')
        print('STATUS:', repr(status))
        if not status['fpga_loaded']:
            raise RuntimeError('FPGA_NOT_LOADED: bitstream initialization needs targeted approval; no upload attempted')
        if status['no_video'] or not status['width'] or not status['height']:
            raise RuntimeError('NO_VIDEO_SIGNAL: adapter opened; no active target video reported')
        if status['width'] > 1920 or status['height'] > 1600:
            raise RuntimeError('Active mode exceeds verified decoder capacity')
        transport.write('g')
        transport.write('i')
        deadline = transport.now() + config['seconds']
        chunks, total, trouble_count, first_tile = 0, 0, 0, False
        while transport.now() < deadline:
            raw = transport.read_video()
            if not raw:
                continue
            trouble, has_first = decoder.videoSink(raw)
            chunks += 1
            total += len(raw)
            first_tile = first_tile or bool(has_first)
            if trouble:
                trouble_count += 1
                if trouble_count > 3:
                    raise RuntimeError('Decoder repeatedly requested I-frame recovery')
                transport.write('i')
        print('VIDEO:', chunks, 'chunks,', total, 'bytes, first tile:', first_tile, 'recoveries:', trouble_count)
        if not chunks or not first_tile:
            raise RuntimeError('No usable decoded video observed')
        if config['output']:
            save_frame(decoder, status, config['output'])
        print('VIDEO_OK: real endpoint data decoded; no keyboard/mouse input sent')
    finally:
        transport.close()


def reset_connection():
    # USB reconnect only, scoped to one descriptor-verified adapter. No rebinding/install.
    transport = Transport()
    try:
        transport.open_control()
        transport.check_status('CyclePort', transport.dev.CyclePort())
        print('USB_RECONNECT_REQUESTED: only verified adapter 152A:8463; no firmware/driver changes')
    finally:
        transport.close()


# Operation dispatch is owned by Rust after all trusted definitions are loaded.
