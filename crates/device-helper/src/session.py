# Loaded explicitly by the Rust helper; never added to ambient import paths.
from __future__ import print_function
import threading
import struct
import Queue


class StartupSilent(RuntimeError):
    pass


def status_mode(status):
    return (status['width'], status['height'], status['hz'])


class VideoSession(object):
    def __init__(self):
        self.transport = Transport()
        self.stop = threading.Event()
        self.status = None
        self.error = ''
        self.latest = None
        self.chunks = 0
        self.total_bytes = 0
        self.first_tile = False
        self.recoveries = 0
        self.video_thread = None
        self.response_thread = None
        self.control_thread = None
        self.thread_handles = {}
        self.controls = Queue.Queue(128)
        self.initializing = bool(config.get('initialize'))
        self.fpga_ack = False
        self.reset_frame_tracking = True
        self.edges = None
        # Picture alignment, ported from the original Adapter.fineTuneXY*.
        self.align_lock = threading.Lock()
        self.fine_tune_step = 0
        self.next_fine_tune = 0.0
        self.mode = None
        self.preferred = {}
        self.status_sequence = 0
        # Steps are relative and status confirms them up to 0.5 s later.
        self.position_expect = None
        self.position_expect_until = 0.0
        # Virtual USB drive (original Adapter.diskImage and its counters). The
        # response thread serves sectors under drive_lock; the service thread
        # never waits for it, so a stalled transfer cannot block the service.
        self.drive = None
        self.drive_lock = threading.Lock()
        self.drive_closing = False
        self.drive_locked = False
        self.drive_ejected = False
        self.drive_read = 0
        self.drive_written = 0
        self.drive_error = ''
        self.drive_sequence = 0

    def start(self):
        try:
            self.start_once()
        except StartupSilent:
            if not config.get('initialize'):
                raise
            # A single descriptor-verified USB reconnect clears stalled endpoints.
            # Never cycle a busy adapter or retry indefinitely.
            self.close()
            print('STARTUP_RECOVERY: reconnecting this adapter once after silent endpoints')
            reset_connection()
            self.__init__()
            self.start_once()

    def start_once(self):
        self.transport.open()
        # Drain video immediately: a previous viewer may have left streaming active.
        # Waiting for status before draining can leave firmware blocked on a full video FIFO.
        for attribute, target, name in (('response_thread', self.response_loop, 'cart-responses'),
                                        ('control_thread', self.control_loop, 'cart-control'),
                                        ('video_thread', self.video_loop, 'cart-video')):
            worker = threading.Thread(target=self.run_worker, args=(attribute, target), name=name)
            worker.daemon = True
            setattr(self, attribute, worker)
            worker.start()
        self.transport.write('v')
        self.transport.write('s')
        preflight = self.transport.now() + 2.5
        while self.status is None and self.transport.now() < preflight:
            if self.error:
                raise RuntimeError(self.error)
            self.stop.wait(0.05)
        if self.status is None:
            raise StartupSilent('Adapter endpoints did not answer initial status queries')
        if config.get('initialize'):
            self.initializing = True
            self.fpga_ack = False
            self.transport.write('e')
            self.transport.write('g')
            self.stop.wait(0.01)
            self.transport.initialize_fpga()
        deadline = self.transport.now() + 15
        next_query = self.transport.now()
        while (self.status is None or self.initializing) and self.transport.now() < deadline:
            if self.error:
                raise RuntimeError(self.error)
            if self.transport.now() >= next_query:
                self.enqueue('s')
                next_query = self.transport.now() + 0.75
            self.stop.wait(0.05)
        if self.status is None or self.initializing:
            raise RuntimeError('No ready status within deadline; FPGA acknowledgement=%r; video chunks=%d bytes=%d; %s' % (self.fpga_ack, self.chunks, self.total_bytes, self.error))
        print('STATUS:', repr(self.status))
        if not self.status['fpga_loaded']:
            raise RuntimeError('FPGA_NOT_LOADED: diagnostic mode did not initialize the adapter')

    def run_worker(self, label, target):
        try:
            handle = self.transport.k.OpenThread(1, False, self.transport.k.GetCurrentThreadId())
            if not handle:
                raise self.transport.C.WinError(self.transport.C.get_last_error())
            self.thread_handles[label] = handle
            target()
        except Exception as error:
            if not self.stop.is_set():
                self.error = 'Worker failed: ' + str(error)
                self.stop.set()

    def enqueue(self, message):
        try:
            self.controls.put_nowait(message)
        except Queue.Full:
            raise RuntimeError('Protocol control queue overflow')

    def control_loop(self):
        next_status = self.transport.now() + 0.5
        try:
            while not self.stop.is_set():
                if not self.initializing and self.transport.now() >= next_status:
                    self.enqueue('s')
                    next_status = self.transport.now() + 0.5
                try:
                    message = self.controls.get(True, 0.05)
                except Queue.Empty:
                    continue
                self.transport.write(message)
        except Exception as error:
            if not self.stop.is_set():
                self.error = 'Protocol write failed: ' + str(error)
                self.stop.set()

    def handle_response(self, message):
        if not message:
            return
        if message[0] == 'H' and len(message) == 5:
            self.enqueue(message)
            return
        if message[0] == 'F':
            self.fpga_ack = True
            print('FPGA_ACKNOWLEDGED: adapter reported configuration loaded')
            self.enqueue('s')
            return
        if message[0] == 'X':
            raise RuntimeError('Adapter rejected FPGA initialization: ' + message[1:].split('\x00')[0])
        if message[0] in 'AB' and len(message) == 9:
            # VSP_FT_READ / VSP_FT_WRITE: first block and byte count, little-endian.
            start, length = struct.unpack('<II', message[1:])
            self.drive_transfer(message[0] == 'A', start, length)
            return
        if message[0] == 'R' and len(message) == 2:
            # VSP_FT_MEDIAREMOVE: the target's prevent/allow medium removal.
            self.drive_locked = bool(ord(message[1]) & 1)
            return
        if message[0] == 'G' and len(message) == 2:
            # VSP_FT_STARTSTOP: the target ejected the medium (original ftEject).
            if ord(message[1]) & 3 == 2 and self.drive is not None:
                self.drive_ejected = True
                self.drive_detach()
            return
        if message[0] == 'P' and len(message) == 2:
            # Original Adapter.rxHandler: autophase completion starts ordinary
            # alignment only when no position was saved for this mode.
            status = self.status
            if (status is not None and not self.initializing and not status['saved_position']
                    and self.mode not in self.preferred):
                self.start_alignment(False)
            return
        parsed = parse_status(message)
        if parsed is not None:
            previous = self.status
            self.status = parsed
            self.status_sequence = (self.status_sequence + 1) & 0xffff
            if self.position_expect == (parsed['position_x'], parsed['position_y']):
                self.position_expect = None
            was_initializing = self.initializing
            if self.initializing and self.fpga_ack and parsed['fpga_loaded']:
                self.initializing = False
                self.reset_frame_tracking = True
            changed = previous is None or any(previous[k] != parsed[k] for k in ('width', 'height', 'no_video'))
            if (changed or (was_initializing and not self.initializing)) and not self.initializing:
                self.latest = None
                if not parsed['no_video'] and parsed['fpga_loaded']:
                    if parsed['width'] > 1920 or parsed['height'] > 1600:
                        raise ValueError('Active mode exceeds verified decoder capacity')
                    self.enqueue('g')
                    self.enqueue('i')
            if not self.initializing and parsed['fpga_loaded'] and not parsed['no_video']:
                # Original Adapter.parseStatusMsg: a mode is (width, height, Hz)
                # and survives no-signal periods; only a different mode realigns.
                mode = status_mode(parsed)
                if mode != self.mode:
                    self.mode = mode
                    self.mode_established(parsed)

    def mode_established(self, status):
        self.stop_alignment()
        self.position_expect = None
        if config.get('position_x') is not None:
            # Diagnostic override: an absolute X for this session only.
            self.move_to(status, (config['position_x'], status['position_y']))
        elif config.get('command') == 'probe':
            return  # Probes observe the picture; they never realign it.
        elif status_mode(status) in self.preferred:
            # A position chosen for this mode replaces automatic alignment.
            self.move_to(status, self.preferred[status_mode(status)])
        else:
            # MainFrame.statusChanged: every new mode starts fast fine-tuning.
            self.start_alignment(True)

    def start_alignment(self, fast):
        # Adapter.fineTuneXYStart: fast starts at 4; ordinary waits one tick at 5.
        with self.align_lock:
            self.fine_tune_step = 4 if fast else 5
            self.next_fine_tune = self.transport.now() + 1.0

    def stop_alignment(self):
        with self.align_lock:
            self.fine_tune_step = 0

    def aligning(self):
        return self.fine_tune_step > 0

    def position_base(self):
        # Until status confirms the last steps, build on what they will produce.
        if self.position_expect is not None and self.transport.now() < self.position_expect_until:
            return self.position_expect
        return (self.status['position_x'], self.status['position_y'])

    def nudge(self, dx, dy):
        # Adapter.sendMiscValue: posX/posY values are relative steps (verified
        # by readback). One step is one pixel at one pixel per clock. Volatile;
        # the separate EEPROM save operation is never issued.
        if not (-128 <= dx <= 127 and -128 <= dy <= 127):
            raise ValueError('Position step out of range')
        if not dx and not dy:
            return
        x, y = self.position_base()
        for index, delta in ((1, dx), (2, dy)):
            if delta:
                self.enqueue('y' + chr(index) + chr(delta & 255))
        self.position_expect = (x + dx, y + dy)
        self.position_expect_until = self.transport.now() + 1.5

    def move_to(self, status, target):
        x, y = target
        if not (-128 <= x <= 127 and -128 <= y <= 127):
            raise ValueError('Picture position out of range')
        base_x, base_y = self.position_base()
        self.nudge(x - base_x, y - base_y)

    def default_position(self, automatic):
        # Adapter.defaultXYPos sends only the mode's X default. Return Y to zero
        # with a relative step; Auto then runs the original fast fine-tune.
        self.stop_alignment()
        y = self.position_base()[1]
        self.enqueue('z\x01')
        if y:
            self.enqueue('y\x02' + chr(-y & 255))
        self.position_expect = None
        if automatic:
            self.start_alignment(True)

    def set_preferred(self, positions):
        if len(positions) > 64:
            raise ValueError('Too many saved picture positions')
        preferred = {}
        for width, height, hz, x, y in positions:
            if not (0 < width <= 1920 and 0 < height <= 1600 and 0 < hz < 256
                    and -128 <= x <= 127 and -128 <= y <= 127):
                raise ValueError('Invalid saved picture position')
            preferred[(width, height, hz)] = (x, y)
        self.preferred = preferred
        status = self.status
        if (status is not None and not self.initializing and not status['no_video']
                and status_mode(status) == self.mode and self.mode in preferred):
            self.stop_alignment()
            self.move_to(status, preferred[self.mode])

    def fine_tune_tick(self, decoder, status):
        # Adapter.fineTuneXYStep, once per second like MainFrame.SlowTick.
        with self.align_lock:
            if self.fine_tune_step <= 0 or self.transport.now() < self.next_fine_tune:
                return
            self.next_fine_tune = self.transport.now() + 1.0
            width, height = status['width'], status['height']
            if status['no_video'] or (width in (720, 640) and height <= 480):
                self.fine_tune_step = 0
                return
            self.fine_tune_step -= 1
            if self.fine_tune_step >= 4:
                return
            left, right, top, bot = decoder.getBlackEdges(width, height)
            if (not left and not right and not bot and not top) or (left and right and bot and top):
                self.fine_tune_step = 0
                return
            dx, dy = 0, 0
            if top or bot:
                if top and not bot:
                    dy += top
                elif bot and not top:
                    dy -= bot
                else:
                    dy = (top + bot) / 2
            if left or right:
                if left and not right:
                    dx += left
                elif right and not left:
                    dx -= right
                else:
                    dx = (left + right) / 2
            ppc = status['pixels_per_clock'] or 1
            if dx > 0 and ppc >= 2:
                dx = (dx + ppc / 2) / ppc
            else:
                dx /= ppc
            if not (-128 <= dx <= 127 and -128 <= dy <= 127):
                # The original would fail packing these; stop rather than wrap.
                self.fine_tune_step = 0
                return
        self.nudge(dx, dy)

    def drive_attach(self, encoded, kind):
        # encoded is the UTF-8 path as hexadecimal, so no path text enters source.
        error = ''
        try:
            if self.transport.data_out is None or self.transport.data_in is None:
                raise DriveError('This adapter does not offer file transfer.')
            if self.status is None or not self.status['fpga_loaded']:
                raise DriveError('The adapter is not ready yet.')
            image = open_drive(self.transport, encoded.decode('hex').decode('utf-8'), kind)
            if not self.drive_lock.acquire(False):
                image.close()
                raise DriveError('The drive is busy finishing a transfer. Try again.')
            try:
                if self.drive is not None:
                    self.enqueue('e')
                    self.drive.close()
                self.drive = image
                self.drive_closing = False
                self.drive_locked = self.drive_ejected = False
                self.drive_read = self.drive_written = 0
                # Original Adapter.sendFileTransferConnect: VSP_FT_CON '<BL'.
                self.enqueue('c' + struct.pack('<BL', image.media, image.blocks))
            finally:
                self.drive_lock.release()
            print('DRIVE_ATTACHED:', image.label, image.blocks, 'blocks of', image.block, 'bytes')
        except DriveError as failure:
            error = str(failure)
        except (IOError, OSError, UnicodeError, TypeError, ValueError) as failure:
            error = 'The disk image could not be opened: %s' % failure
        self.drive_error = error
        self.drive_sequence = (self.drive_sequence + 1) & 0xffff

    def drive_detach(self):
        # Original Adapter.ftEject: VSP_FT_DIS, then close the image. A transfer
        # in progress finishes first; the response thread then closes it. The
        # sequence always advances so the viewer sees the request answered.
        if self.drive is not None:
            self.drive_closing = True
            self.enqueue('e')
            if self.drive_lock.acquire(False):
                try:
                    self.close_drive_locked()
                finally:
                    self.drive_lock.release()
        self.drive_sequence = (self.drive_sequence + 1) & 0xffff

    def close_drive_locked(self):
        image, self.drive = self.drive, None
        self.drive_closing = self.drive_locked = False
        if image is not None:
            image.close()
            print('DRIVE_DETACHED: read', self.drive_read, 'written', self.drive_written)

    def drive_transfer(self, reading, start, length):
        with self.drive_lock:
            image = self.drive
            if image is None:
                return  # The original ignores requests with no medium inserted.
            try:
                if length > MAX_TRANSFER:
                    raise DriveError('The target requested an unsupported transfer size.')
                offset = start * image.block
                if reading:
                    self.transport.write_data(image.read(offset, length))
                    self.drive_read += length
                else:
                    done = 0
                    while done < length:
                        chunk = self.transport.read_data(min(length - done, 65536))
                        if not chunk:
                            if self.stop.is_set():
                                return
                            raise DriveError('The adapter stopped sending data the target wrote.')
                        # CD-ROM media are read-only; the data is consumed and discarded.
                        image.write(offset + done, chunk)
                        done += len(chunk)
                    self.drive_written += length
            except (DriveError, IOError, OSError) as failure:
                if self.stop.is_set():
                    return
                self.drive_error = 'The drive was unplugged: %s' % failure
                self.enqueue('e')
                self.close_drive_locked()
                self.drive_sequence = (self.drive_sequence + 1) & 0xffff
                return
            if self.drive_closing:
                self.close_drive_locked()

    def poll_drive(self):
        image = self.drive
        supported = self.transport.data_out is not None and self.transport.data_in is not None
        return struct.pack('<5BH3Q', supported, image is not None, image.media if image else 0,
                           self.drive_locked, self.drive_ejected, self.drive_sequence,
                           image.size if image else 0, self.drive_read, self.drive_written)

    def response_loop(self):
        try:
            while not self.stop.is_set():
                # Never wait for the write lock here: the device can backpressure
                # OUT while waiting for this thread to drain its IN response FIFO.
                self.handle_response(self.transport.read_response(1500))
        except Exception as error:
            if not self.stop.is_set():
                self.error = 'Status I/O failed: ' + str(error)
                self.stop.set()

    def video_loop(self):
        decoder = load_decoder()
        decoder.createBuffer(1920, 1600)
        last_publish = 0.0
        mode = None
        try:
            while not self.stop.is_set():
                raw = self.transport.read_video(1500)
                if not raw or self.initializing:
                    continue
                status = self.status
                active = None if status is None else (status['width'], status['height'], status['no_video'])
                if self.reset_frame_tracking or (mode is not None and active != mode):
                    # The stream's I-frame rebuilds tiles; do not call undocumented reset APIs.
                    self.first_tile = False
                    self.reset_frame_tracking = False
                mode = active
                trouble, has_first = decoder.videoSink(raw)
                self.chunks += 1
                self.total_bytes += len(raw)
                self.first_tile = self.first_tile or bool(has_first)
                if trouble:
                    self.recoveries += 1
                    if self.recoveries > 10:
                        raise RuntimeError('Repeated decoder recovery failure')
                    self.enqueue('i')
                    continue
                now = self.transport.now()
                if status is None or status['no_video'] or not self.first_tile or now - last_publish < (1.0 / 30):
                    continue
                width, height = status['width'], status['height']
                if not (0 < width <= 1920 and 0 < height <= 1600):
                    continue
                rows = []
                # Match the shipped drawing caller: the native decoder reads one
                # 16-pixel tile row at a time, not a speculative full-height rectangle.
                for tile_y in xrange((height + 15) // 16):
                    rgba, size = decoder.toRGBString(0, tile_y, (width + 15) // 16, 1, 1.0)
                    padded_width, tile_height = size
                    if padded_width < width or tile_height != 16 or len(rgba) != padded_width * 16 * 4:
                        raise ValueError('Invalid decoder tile-row shape')
                    visible_rows = min(16, height - tile_y * 16)
                    if padded_width == width:
                        rows.append(rgba[:width * visible_rows * 4])
                    else:
                        rows.append(''.join(rgba[y * padded_width * 4:y * padded_width * 4 + width * 4] for y in xrange(visible_rows)))
                self.latest = (''.join(rows), width, height)
                last_publish = now
                if self.fine_tune_step > 0:
                    self.fine_tune_tick(decoder, status)
                elif config.get('command') == 'probe':
                    # Read-only diagnostic of the original alignment input.
                    self.edges = tuple(decoder.getBlackEdges(width, height))
        except Exception as error:
            if not self.stop.is_set():
                self.error = 'Video I/O failed: ' + str(error)
                self.stop.set()

    def poll_frame(self):
        frame = self.latest
        self.latest = None
        if frame is None:
            return ''
        rgba, width, height = frame
        return struct.pack('<II', width, height) + rgba

    def poll_status(self):
        status = self.status
        if status is None:
            return ''
        return struct.pack('<HH6B2b2BH', status['width'], status['height'], status['hz'],
                           status['keyboard_ready'], status['leds'], status['fpga_loaded'],
                           status['no_video'], bool(self.error), status['position_x'],
                           status['position_y'], status['saved_position'], self.aligning(),
                           self.status_sequence)

    def close(self):
        self.stop.set()
        for handle in self.thread_handles.values():
            self.transport.k.CancelSynchronousIo(handle)
        for worker in (self.response_thread, self.video_thread, self.control_thread):
            if worker is not None:
                worker.join(3)
                if worker.is_alive():
                    print('FATAL: I/O thread did not stop; terminating only this helper')
                    sys.stderr.flush()
                    import os
                    os._exit(86)
        for handle in self.thread_handles.values():
            self.transport.k.CloseHandle(handle)
        self.thread_handles.clear()
        if self.drive is not None:
            # Original Adapter.close: eject an inserted disk before going away.
            try:
                self.transport.write('e')
            except Exception as error:
                print('Drive detach on close failed:', error)
            self.drive.close()
            self.drive = None
        self.transport.close()


def probe_session():
    session = VideoSession()
    frame = None
    try:
        session.start()
        if config.get('drive'):
            # Explicit hardware check of the virtual drive: attach, observe, detach.
            session.drive_attach(config['drive'], config['drive_kind'])
            if session.drive_error:
                raise RuntimeError('DRIVE_ATTACH_FAILED: ' + session.drive_error)
        deadline = session.transport.now() + config['seconds']
        while session.transport.now() < deadline:
            if session.error:
                raise RuntimeError(session.error)
            candidate = session.latest
            if candidate is not None:
                frame = candidate
            session.stop.wait(0.05)
        if frame is None:
            raise RuntimeError('No decoded frame available; status: ' + repr(session.status))
        rgba, width, height = frame
        if config['output']:
            save_rgba_png(rgba, width, height, config['output'])
        if config.get('drive'):
            print('DRIVE:', 'read', session.drive_read, 'written', session.drive_written,
                  'locked', session.drive_locked, 'ejected by target', session.drive_ejected,
                  'error', repr(session.drive_error))
            session.drive_detach()
            session.stop.wait(0.5)
        print('BLACK_EDGES:', repr(session.edges), 'position:', session.status['position_x'],
              session.status['position_y'], 'pixels/clock:', session.status['pixels_per_clock'])
        print('VIDEO_OK:', session.chunks, 'chunks,', session.total_bytes, 'bytes,',
              '%dx%d RGBA' % (width, height), 'recoveries:', session.recoveries,
              '; no keyboard/mouse input sent')
        print('FINAL_STATUS:', repr(session.status))
    finally:
        session.close()


def save_rgba_png(rgba, width, height, path):
    import os
    import zlib
    def chunk(kind, data):
        return struct.pack('>I', len(data)) + kind + data + struct.pack('>I', zlib.crc32(kind + data) & 0xffffffff)
    scanlines = ''.join('\x00' + rgba[y * width * 4:(y + 1) * width * 4] for y in xrange(height))
    png = '\x89PNG\r\n\x1a\n' + chunk('IHDR', struct.pack('>2I5B', width, height, 8, 6, 0, 0, 0))
    png += chunk('IDAT', zlib.compress(scanlines, 6)) + chunk('IEND', '')
    fd = os.open(path, os.O_CREAT | os.O_EXCL | os.O_WRONLY | os.O_BINARY, 0600)
    with os.fdopen(fd, 'wb') as output:
        output.write(png)
    print('FRAME_SAVED:', path)
