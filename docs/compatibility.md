# Compatibility and verification

## Current status

This is a development build for the adapter tested on this machine, not a general replacement for every USB crash cart.

Real **1920 × 1080 at 60 Hz** source video has been decoded and displayed by the Rust viewer. The 60 Hz figure is the adapter's reported source mode, not a measured viewer frame rate. The helper publishes at most 30 decoded frames per second.

**Paste is experimental.** Short keyboard and paste checks have produced exact output, but longer tests have intermittently repeated characters. A completed device write, or a completed paste progress counter, does not prove that the target received the intended text. Check a short sample in an editor before using paste, and verify the result. Do not use this build for unattended command entry.

The target console has also displayed USB descriptor/EMI errors (`-71`). Those errors and the repeated characters are observations; a causal link has not been established. Increasing the paste pause is not a verified fix.

## Tested installation

| Component | Observed value |
| --- | --- |
| Host | Windows 11 Pro, x64 |
| Adapter USB identity | `152A:8463` |
| USB description | `Analog VGA+KM to USB Adapter` |
| Existing driver | Signed `ulcvm_x64`, version `2.80.0.0` |
| Device interface GUID | `{B50CA981-518E-4a24-9759-07234F0AC6EE}` |
| Vendor installation | `C:\Program Files (x86)\USB Crash Cart Adapter` |
| Vendor transport/runtime | 32-bit COM transport and Python 2.7.3 |
| Vendor decoder | Installed `fbext_windows.pyd`, loaded with its module name `fbext` |
| Viewer/helper | x64 Rust viewer; x86 Rust helper |
| Target keyboard mapping | US English |

The installation is associated with the StarTech NOTECONS02 family, but the exact retail SKU has not been proven. Matching a product name is not enough to establish compatibility with another adapter.

The helper requires exactly one enumerated interface and checks the USB descriptor before acquiring the device. It does not choose among multiple adapters. Close the original viewer normally before connecting; the device is exclusive. A busy adapter is reported, not taken away from its current application.

## Runtime boundary

The original installation stays in place. The helper loads its installed dependencies from explicit paths; the new package does not contain proprietary DLLs, drivers, firmware, or FPGA data.

The legacy runtime runs only inside the child helper. Site imports, user-site imports, ambient Python environment settings, and bytecode writes are disabled. The helper changes its working directory to its own executable directory, restricts DLL search, and supplies the vendor library paths explicitly. It imports the generated COM declarations, not the original viewer's startup path or its registration/recovery routines.

The current legacy bridge requires an ASCII vendor installation path. Moving or replacing the vendor installation is not supported by the packaging script.

### Device startup

The verified acquisition sequence is enumeration, open, descriptor check, acquisition, interface selection, and configuration. Endpoint roles are:

- `0x04`: command OUT.
- `0x83`: status/response IN.
- `0x82`: compressed video IN.
- `0x05`: virtual-drive data OUT (sectors the target reads).
- `0x85`: virtual-drive data IN (sectors the target writes).

The two drive pipes exist on generation-2 adapters (`8463`), which the original binds as `ftx`/`frx` (`usbcon.win32usb.device`, hardware version 2). If they cannot be bound, video and input still start and the drive reports itself unavailable.

Normal viewer startup includes **volatile FPGA initialization**, as approved for this adapter. The helper checks the installed `data\ulcvm.fgz` SHA-256 before using it:

```text
c32c853b376aaf6db53cb2a972218c5f69e347c28a2e5bc98601b1d6f246fadc
```

The decompressed configuration is 341,160 bytes. High-speed transfers use 507-byte data blocks in fixed 512-byte packets, followed by a terminal packet. Startup requires the device's configuration acknowledgement and loaded status. This is operational configuration, not a persistent firmware upgrade. The helper does not read or upload `.fc5` firmware.

If initial endpoints remain silent, startup can close its own handles, request one descriptor-checked USB reconnection, and retry once. It does not reset a busy device or retry indefinitely. This reconnect can interrupt the adapter's target-side USB connection; input must not be assumed reliable merely because video has returned.

### Video

The installed decoder owns a 1920 × 1600 backing buffer. The helper continuously consumes the compressed stream and reconstructs each published image from the decoder's documented-by-caller 16-pixel tile rows. Active dimensions and buffer lengths are checked before copying to the viewer. Decoder trouble requests an I-frame; repeated recovery failures close the session.

The newest completed frame replaces older unpublished frames. There is no growing queue of decoded frames. The GUI uses one authoritative viewport for rendering and absolute mouse mapping. Explicit 1:1 aligns that transform to the physical framebuffer pixel grid; fractional-scale animated zoom remains continuous.

The viewer now caches a full GPU mip chain for each source size. Lower levels are area-weighted in linear light using sRGB views/attachments; egui still samples the full chain through its expected gamma-encoded `Rgba8Unorm` view. Mips regenerate once after an upload, not for unchanged redraws or individual zoom events. Reduced images use trilinear sampling. Crisp enlargement changes only magnification, not the minification filter. Native mip-0 bytes remain unchanged. This improves downscaled stroke stability and visibility; it cannot preserve readable glyph detail at arbitrarily small scales.

On the current VGA source, the captured image appears to lose approximately the first 48 horizontal pixels. A volatile X-position experiment did not establish a correction, so no guessed calibration is hardcoded. Source alignment, decoder alignment, and viewer scaling must not be confused. The helper can use the adapter's volatile mode default when no saved position exists; it does not save alignment to EEPROM.

**Horizontal edges.** The original application corrects position, not capture width, and the same control fixes the tested target. Static inspection of the original (`Adapter.parseStatusMsg`, `MainFrame.statusChanged`, `Adapter.fineTuneXYStart/Step`, `framebuffer.getBlackEdges`, `MainFrame.SlowTick`) shows that every new `(width, height, Hz)` mode starts fast fine-tuning, advanced once per second for up to four steps. Each step reads the native black-edge measurement and sends relative `posX`/`posY` corrections through the volatile `y` command. Autophase completion starts ordinary fine-tuning only without a saved position. The helper now does the same, including `pixPerClk` rounding and the 720/640 × ≤480 exclusion. It no longer sends an X default when a mode appears; the original never did.

Hardware readback confirmed that position values are relative steps of one captured pixel at one pixel per clock: +1 then −1 moved X from −45 to −44 and back. On the tested console, the native detector returned `(0, 0, 0, 0)`, so automatic alignment stops without changes, exactly as the original would. The console's 8 px glyph grid showed the picture 45 px left of true. At −45, every line starts at x = 0 and the grid phase matches. The viewer stores that position per mode in `%APPDATA%\USB Crash Cart\settings.json` and the helper reapplies it whenever the mode appears, including after an adapter reset to X = 0. EEPROM save is never sent.

Earlier status, kept for context: the request for 15 additional source pixels at each horizontal edge was unverified. A bounded static trace now follows `actions.pyo:489` (`Actions.CMD_videoSettings`) to `wxer.VideoSettingsWindow`, its position callbacks at original source lines 190–216, and `adapter.pyo:1273` (`Adapter.sendMiscValue`). Left/right controls request opposite X-position deltas; Auto requests `autopos`; Default requests `defaultXY`. The sender packs the chosen miscellaneous field and value, encoding negative values with +256. This establishes positioning, not a wider sampling window. A reference scan for override/timing fields found no matching code objects within `actions`, `wxer`, or `adapter`; other modules were not examined, so this is not proof that no such mechanism exists elsewhere.

No new hardware adjustment, speculative decoder-width increase, or artificial border has been applied. Offline edge tests and the four-corner demo markers verify that Fit retains all columns already supplied to the renderer. They do not prove recovery of pixels missing upstream. `--inspect-video-settings` repeats the bounded Python-2 code-object inspection without importing the original GUI modules or opening a device.

### Virtual USB drive

The original **Disk Control** window (`wxer.DiskCtrlSettingsWindow`, `Adapter.ftInsert/ftEject/ftRead/ftWrite`, read statically) presents a disk image to the target as USB mass storage. The FPGA configuration already loaded for video carries it; no other bitstream, firmware or driver is involved. The protocol, as the original uses it:

| Message | Direction | Payload | Meaning |
| --- | --- | --- | --- |
| `c` | host → adapter | `<BL`: media (1 disk, 2 CD-ROM), block count | Plug in (`VSP_FT_CON`) |
| `e` | host → adapter | none | Unplug (`VSP_FT_DIS`); also sent at startup and on close |
| `A` | adapter → host | `<II`: first block, byte count | Target read: the host sends that many bytes on `0x05` |
| `B` | adapter → host | `<II`: first block, byte count | Target write: the host reads that many bytes from `0x85` |
| `R` | adapter → host | `>B`, bit 0 | Target prevented (1) or allowed (0) medium removal |
| `G` | adapter → host | `>B`, `& 3 == 2` | Target ejected the medium |

Disk blocks are 512 bytes; CD-ROM blocks are 2048 bytes. The helper serves the requests on its response thread, as the original did, under a drive lock that the service thread never waits on: an unplug requested during a transfer completes after it. Transfers larger than 16 MiB are treated as a protocol fault and unplug the drive. Requests arriving with no medium are ignored, matching the original. The command allowlist admits only `c` with a valid media byte and `e`.

Images are opened with positioned I/O on a handle that denies other writers, so no program (including the viewer) can change sectors while the target uses them. Supported formats: raw sector images; ISO 9660 (CD-ROM, read-only; written data is consumed and discarded); fixed VHD; and dynamic VHD, including block allocation that moves the footer, sector bitmaps (clear bits read as zero), and footer and header checksums. Differencing VHDs and VHDX are refused. The original accepted only its own dynamic VHDs and ISOs.

The **shared drive** is the viewer's own image at `%LOCALAPPDATA%\USB Crash Cart\Shared drive.img`: an MBR with one FAT32 (LBA) partition at 1 MiB, labelled `CRASH CART`. It is created at 512 MB or more (what is sent plus a quarter, at least 256 MiB, for the target), rebuilt larger when new files do not fit, and limited to 32 GB. FAT32 limits single files to under 4 GiB. Files are only added, saved or erased while the drive is unplugged; sending unplugs first. Saving copies everything into a new folder and never merges into or overwrites an existing one.

When the target holds the drive (prevent-removal), the viewer asks before unplugging: removing a mounted drive can lose the target's unwritten changes, exactly as with a physical stick. Linux locks a CD-ROM as soon as it opens the device.

### Private IPC

The implemented transport differs from the initial shared-memory/named-pipe proposal: the viewer starts its own child with inherited private stdin/stdout pipes. There is no public pipe name, listening network endpoint, or shared-memory mapping.

Commands are bounded, typed JSON lines. Input queues are bounded; overflow fails closed. Cancel, release, and quit invalidate pending work through an epoch barrier rather than waiting behind ordinary queued input. Video output uses a replaceable latest-frame slot and an independent pipe writer so a slow display does not build a frame backlog.

Output records use the fixed 24-byte header `<4sHHIIII`: `CART`, version 1, kind, sequence, width, height, and payload length. Kind 1 contains tightly packed RGBA; kind 2 contains status JSON. Consumers reject invalid dimensions, lengths, kinds, versions, and regressing sequences. Reconnecting creates a new child and reader state; input is not replayed into the next connection.

### Keyboard, mouse, and paste

The viewer handles physical key transitions before GUI text events. Control mode acquires input automatically only over the actual image in the focused window, with matching frame/status dimensions, fresh video, and a ready keyboard. Menus, local text editing, clipboard preview, paste, and special-key sequences block acquisition. Host-held input is tracked separately from forwarded input: keys/buttons pressed locally are not inherited, and a local press retains its local release.

Navigate mode releases target input and keeps Ctrl+scroll zoom and drag-pan local. F12 switches Control/Navigate and never reaches the target; returning to Navigate cannot be undone by automatic capture. Remote F12 is under More → Special keys. Duplicate key-down events are not forwarded, leaving repeat to the target. Left/right modifiers remain distinct. Existing drags preserve their final release outside the image but inside the window; cursor exit, focus loss, and readiness failure release input.

The verified physical keyboard wire path uses separate modifier key events and a zero wire-meta field. Paste uses explicit Shift transitions, a 50 ms hold after the press write completes, and a configurable pause after confirmed releases. The default pause is 150 ms, with a UI range of 50–500 ms. These timings are conservative defaults, not a target-delivery guarantee.

Paste reads the local clipboard only when **Paste clipboard…** is selected. It snapshots and validates the text before submission, previews it, and requires confirmation. Enter/Tab require an extra acknowledgement. Printable ASCII, Tab, CR, LF, and CRLF are supported for a US layout. CR and CRLF become logical line breaks, intentional trailing line breaks are retained, and no Enter is added. Unsupported characters reject the entire paste. The current limit is 16 KiB.

Validated target LED status determines Caps Lock mapping. The viewer does not toggle Caps Lock blindly. While paste is active, normal remote input is blocked; F12, Cancel, and focus loss cancel pending paste and attempt to release held keys. Clipboard text is not written to logs or settings.

Mouse packets use absolute source-pixel coordinates, three button bits, and signed big-endian fields. New letterbox clicks are not mapped to the target; releases for an existing drag are preserved. Full hardware validation of mouse corners, dragging, and wheel direction remains outstanding.

Release is best effort after an unplug, driver failure, or unresponsive target. This build cannot promise delivery of an all-up packet through a broken connection. Windows-reserved shortcuts are not all interceptable; use the explicit special-key controls where appropriate.

There is also a known software release-latency limitation: the active synchronous transport path does not enforce its timeout argument. An unrelated protocol write can hold the common write lock ahead of a key-up. Frame/status polling on the interpreter-owning thread can also postpone a scheduled release. The 50 ms hold is therefore a due time, not an enforced maximum. These are code-grounded delay exposures, not proof of what caused the observed repetitions; they remain unresolved in this development build.

## Verification record

Automated checks on both Windows architectures cover geometry/DPI, all 95 printable ASCII characters, newlines and rejection without partial submission, key state and modifier releases, fake-clock scheduling, bounded IPC, latest-frame replacement, stale input barriers, queue overflow, and failed writes. The current suites contain 48 core tests on x64 (49 on x86, including a 32-bit address-space check), 17 helper tests, and 27 viewer tests. Viewer coverage includes automatic-capture gates, drag-out ownership, a latched navigation mode, fractional zoom anchors, native-pixel alignment, local editing focus, and GPU mip/readback tests. Formatting and Clippy are checked separately.

Real-device observations include decoded 1080p video, FPGA acknowledgement, actual GPU-rendered viewer screenshots, clean session closure, exact short key/paste samples, and longer samples with repeated characters. Return, keypad Enter, and Ctrl+J were exercised in the identified scratch console using harmless commands.

The all-ASCII byte-count test dated 2026-10-25 **did not complete**: its controller permit arrived after the readiness deadline. Its payload was not sent. The preparation command itself contained repeated characters; no successful full byte-for-byte hardware result is claimed.

A ten-minute video soak and its measured results are recorded separately in `verification.md` when available. Memory samples show process allocation, not end-to-end latency or target input correctness.

The following are not yet fully verified on real hardware:

- Reliable long paste, including the complete printable ASCII/Tab/newline byte check.
- Mouse corners, drags outside the image, and wheel behavior.
- Every modifier/special-key combination and sustained interactive repeat.
- Video mode changes, unplug/replug, no-signal recovery, and moving between monitors with different DPI.
- Color calibration and measured input/presentation latency.
- Automatic alignment on a target whose content reaches the edges; the native detector has only been observed on a console.
- Waking a sleeping target display with the Shift tap.
- Target writes to the virtual drive (`B` requests). Plugging in, target reads, CD-ROM locking, unplugging and the viewer's status were observed on hardware; no write was requested because nothing was typed on the target. The write path and dynamic VHD growth are covered by the helper's `--drive-selftest`.
- A Windows or macOS target using the virtual drive; only a Linux console was available.

## Reproducing checks

```powershell
# Build both executable architectures and run their tests.
.\scripts\build.ps1 -Test

# No hardware: load the installed runtime and inspect decoder/API metadata.
.\target\i686-pc-windows-msvc\release\cart-device-helper.exe --runtime-info

# Real video/status only. Close any other viewer first.
.\target\i686-pc-windows-msvc\release\cart-device-helper.exe --probe --initialize --seconds 10

# UI/GPU only; the demo is explicitly labeled and never opens the adapter.
.\scripts\run.ps1 -Demo

# No hardware: every disk image format, read/write/reopen, in a new folder.
.\target\i686-pc-windows-msvc\release\cart-device-helper.exe --drive-selftest "$env:TEMP\cart-drive-check"

# Plug an image into the target for the probe's duration, then unplug.
.\target\i686-pc-windows-msvc\release\cart-device-helper.exe --probe --initialize --seconds 20 --drive C:\Images\check.img
```

For the real GUI soak, keep the matching helper beside the packaged viewer, choose a new screenshot path, and run:

```powershell
.\scripts\soak.ps1 -Executable "$PWD\dist\cart-viewer.exe" -Screenshot "$PWD\diagnostics\soak-new.png" -Seconds 600
```

The explicit-input harness `scripts\hardware-smoke.py` is for the previously identified scratch console only. Its `--allow-input` switch is mandatory. Never use its console-specific stages against an unknown editor, login screen, or production terminal. A successful harness submission still requires inspection of the actual target output.

## Out of scope

The viewer itself performs no persistent firmware update, driver changes, COM registration or registry edits. Only the separate setup program installs the driver (`pnputil`), registers the 32-bit COM transport (`regsvr32`), writes its Apps & features entry and creates shortcuts, and only after the person installing accepts. Networking, recording, EEPROM saves and automatic termination of other viewers are not provided. The virtual USB drive serves image files from this PC only; it never reads the target's own disks. There is no fully native decoder in this version.
