# Verification record — 2026-10-26

## Version 0.3.0: virtual USB drive

### Automated checks

| Check | Result |
| --- | --- |
| Formatting and strict Clippy (x64 and x86) | Passed; warnings denied |
| x64 core tests / x86 core tests | 48 / 49 passed |
| x86 helper tests | 21 passed |
| x64 viewer tests | 39 passed, plus 2 opt-in (copy speed, hardware fixture) |
| x64 installer tests | 7 passed |
| `--drive-selftest` (no device) | Raw, fixed VHD, dynamic VHD growth, reopen and sector bitmap, ISO read-only, wrong-format refusals, other writers denied while attached |

New tests cover the drive commands (typed, absolute paths only, the path reaching Python as hexadecimal), the 31-byte drive status layout, the shared drive (MBR and FAT32 layout, hidden sectors, Unicode and nested names, replace on resend, growth that keeps existing files, cancellation leaving no partial file, saving into a new folder without overwriting), image identification by content, Windows-safe names when saving, and Explorer-style sizes. Copying 400 MB onto a fresh drive took about 2 s; listing a 700 MB drive about 24 ms.

### Original protocol, read statically

`--inspect-video-settings --outline MODULE` lists every code object in an original module without importing it. It established the Disk Control window (`wxer.DiskCtrlSettingsWindow`), `Adapter.ftInsert/ftEject/ftRead/ftWrite/ftMediaRemove/ftStartStop`, `vsproto` (`c`/`e` commands, `A`/`B`/`R`/`G` responses, `<BL` and `<II` layouts, media types) and the generation-2 data endpoints `0x05`/`0x85` in `usbcon.win32usb.device`. `usbip.fgz` is the generation-1 FPGA configuration, not a separate drive mode.

### Real adapter (Linux console target)

- Helper probe with a 512 MB shared-drive image: plugged in as a USB flash drive; the target read 3,290,112 bytes; unplugged cleanly; video continued.
- Helper probe with a 22-block ISO: plugged in as a CD-ROM; the target read 57,344 bytes and locked the tray (`R` = 1); unplugged cleanly.
- Connected viewer smoke test with `--smoke-attach`: the viewer sent the attach, the panel showed the image plugged in with 3.14 MB read, the status bar showed it, and the remembered −45 px position was applied (`viewer-attached-2.png`).
- Not exercised: target writes, target-side eject, and Windows or macOS targets.

## Version 0.2.0: setup, picture position, middle-drag

### Automated checks

| Check | Result |
| --- | --- |
| Formatting | Passed |
| Strict Clippy, x64 core/viewer/installer and x86 helper | Passed; warnings denied |
| x64 core tests / x86 core tests | 48 / 49 passed |
| x86 helper tests | 19 passed |
| x64 viewer tests | 31 passed |
| x64 installer tests | 7 passed |

New tests cover: middle-drag ownership through release, cancellation that never resumes, a missed release that fails closed until the next middle click, physical-pixel pan deltas at 100–200% DPI with 1:1 grid alignment, the picture-position settings file (round trip, replacement, invalid files), the typed alignment and wake commands (bounds, no extra fields, integer-only Python source), fresh-status keyboard readiness without video, and the 16-byte helper status layout. Installer tests run the whole sequence against a fake system in a temporary folder: file layout, one driver and one COM call, skipped or installed VC++ runtime, restart codes, a failed driver step stopping later changes, a running viewer blocking setup, path-escape rejection, and uninstall removing only what setup created.

### Original alignment, read statically

`--inspect-video-settings --symbol MODULE:Name` now prints any code object from the original archive with line numbers, full opcode names and jump targets, without importing the original modules. It established the original fine-tune sequence (`Adapter.fineTuneXYStart/Step`, `MainFrame.statusChanged`, `MainFrame.SlowTick` every 250 ms with fine-tuning on every fourth tick), `Adapter.parseStatusMsg` mode detection by width, height and Hz, autophase handling, and the `vsproto` constants (`y` set, `z` default, `>BB` packing, misc indexes 1 = X and 2 = Y).

### Real adapter

- Native `getBlackEdges` on the tested console returned `(0, 0, 0, 0)`. The original algorithm therefore stops without changes on this content.
- The console's 8 px glyph grid placed the picture 45 px left of true (three word gaps and the grid phase agree). A volatile −45 X position put every line at x = 0 with the grid phase matching: `diagnostics\edges-probe-002.png` before and `edges-x-minus45.png` after.
- +1 then −1 moved X from −45 to −44 and back: position commands are relative.
- After a descriptor-verified reconnect reset X to 0, the release viewer reapplied the remembered −45 for 1920 × 1080 at 60 Hz; the smoke run reported `position=-45,0` with fresh video (`remembered-position-001.png`).
- The trimmed runtime (19 vendor files plus `ulcvm.fgz`) ran FPGA initialization, the COM transport and 1080p decoding with no other vendor files present.

### Setup program

`USB Crash Cart Setup 0.2.0.exe` embeds 302 files. Run without elevation through `--extract`, every extracted file matched its staged SHA-256, and the extracted helper loaded its private runtime (`RUNTIME_OK`). The executable carries a `requireAdministrator` manifest, icon and version resources. The setup window was captured and inspected (`setup-welcome-002.png`).

Not run here: the elevated install itself (driver, COM registration, shortcuts, Apps & features entry). It requires administrator approval and changes this PC, so it is left for the person installing. VC++ runtime installation is covered by tests only; this PC already has it.

### Middle-drag in the real window

In the demo at 1:1 and in Control mode, a middle drag moved the view (`pan=131,-133`, scale 1.0000, `input_captured=false`; `middle-drag-003.png`). The distance did not match the scripted drag because a remote-desktop session was moving the real pointer at the same time, so that run's distance assertion failed. Exact deltas are covered by unit tests.

## Previous update — 2026-10-25

The navigation, automatic-capture policy, compact UI, GPU filtering, and native icon/title-bar update is built and tested. **Reliable long paste and the requested extra 15 captured pixels per horizontal side remain unresolved.** This is still a development package.

### Build and automated checks

| Check | Result |
| --- | --- |
| Release x64 viewer and x86 helper | Built |
| Formatting check | Passed |
| Strict Clippy, x64 core/viewer and x86 helper | Passed; warnings denied |
| x64 core tests | 48 passed |
| x86 core tests | 49 passed |
| x86 helper tests | 17 passed |
| x64 viewer tests | 28 passed |

The x86 core run repeats the shared suite and adds one address-space test; these are not separate implementations.

New coverage includes capture eligibility and local-mode latching, remote drag-out releases, no repeated cancellation while blocked, continuous zoom anchors at 100/125/150/200% DPI, frame-rate-independent damping, native-grid alignment and fractional pan accumulation. A headless egui test checks that toolbar-button focus does not prevent automatic hover acquisition while actual text editing does.

GPU readback tests verify linear-light mip averages, constant colors, odd dimensions/edge retention, refreshed uploads, resource reuse/release, unchanged native-size bytes, sampler behavior, and egui/screenshot render parity. The generated console fixture contains text in the video pixels, both text polarities, one-pixel patterns, and four corner markers; it is explicitly labeled synthetic and does not stand in for adapter compatibility.

### Native GUI checks

The actual viewer was launched in `--demo --smoke-test`, which cannot start a helper or capture target input. Tests exercised the window-event/control paths and inspected the app's own GPU screenshots. A separate capture renders only the PID/title-verified demo window, including the real Windows caption; it never captures the desktop.

The icon/chrome release was checked again at 1:1, with Ctrl+scroll, and at minimum width. Both native window/taskbar icon handles were present. The rendered caption and toolbar backgrounds matched exactly at RGB **14, 16, 21**; the icon, title text, border, and standard window controls were also inspected visually. Windows SDK marks these DWM color attributes as set-only, so the harness checks rendered pixels rather than attempting unsupported attribute readback.

Static PE inspection found one icon group with **16, 20, 24, 32, 48, 64, 128, and 256**-pixel entries, all matched to their image resources, plus version metadata identifying the viewer. Runtime checks cover window icon loading separately; the static inventory alone does not establish Explorer's icon-cache behavior.

- Final release Ctrl+scroll: physical scale changed from approximately **0.7083 to 0.9372**, with `input_captured=false`.
- Final release 1:1: the button produced physical scale **1.0000**, with `input_captured=false`.
- Minimum-width layout: **760 × 520** client area, Fit scale **0.3958**; mode/view controls, overflow menu, fullscreen, and all four source corner markers remained visible.
- Toolbar zoom and F12 mode changes were exercised; changing mode stops animation at the displayed transform rather than snapping to an unfinished target.

Some early synthetic-only interaction attempts did not activate controls under Windows foreground/hover behavior. The harness now asserts actual scale, not just exit/screenshot existence. Toolbar clicks are guarded by the identified demo PID/title and a window-under-cursor check; the pointer is restored when it has not been moved by the user. Keyboard/wheel messages stay window-targeted. No global keyboard presses or target input are injected.

Screenshots remain local in ignored `diagnostics`, including:

```text
navigation-fit-001.png
navigation-narrow-002.png
navigation-release-wheel-003.png
navigation-release-native-003.png
chrome-window-002.png
chrome-native-002.png
chrome-window-004.png
chrome-wheel-005.png
chrome-narrow-005.png
```

### Updated renderer on the real adapter

The updated renderer/UI also completed a **61.8-second** real-video session and normal shutdown:

- Exit code **0**; fresh real video still present at completion.
- **2,771 GUI redraws**, not unique decoded source frames or a source-frame-rate measurement.
- `input_captured=false`; video-check mode explicitly disables target input actions.
- **43** memory samples after warm-up.

| Private memory | Viewer | Helper |
| --- | ---: | ---: |
| First 30-sample window, mean | 366.8 MiB | 31.4 MiB |
| Last 30-sample window, mean | 366.9 MiB | 29.6 MiB |
| Maximum sampled | 380.6 MiB | 38.9 MiB |

The windows overlap in this short run, so these figures are allocation observations, not a long-term leak test. The recorded helper minimum of zero occurs around shutdown and is not a live-process measurement.

The inspected screenshot, `diagnostics\navigation-live-001.png`, shows real 1920 × 1080 video, the compact toolbar without the old brand heading, navigation mode, and the explicit “target input disabled” test status. It contains the target's screen and is not included in the package. The target still displays USB `-71` errors; this task did not establish their cause or fix long paste.

### Horizontal-edge investigation

The original action was traced statically to its actual Video Settings window and position callbacks, then the miscellaneous-value packet sender. That establishes horizontal positioning and Auto/Default actions, **not a verified symmetric width expansion**. The bounded timing/override reference search covered only `actions`, `wxer`, and `adapter` and found no matching code objects. Other modules were not examined.

No guessed X correction, larger native decoder allocation, artificial border, firmware operation, or persistent save was applied. Fit's edge-preservation tests verify the supplied frame, not recovery of upstream pixels. The 15-pixel request therefore remains open; see [compatibility.md](compatibility.md).

## Earlier ten-minute baseline

Before this UI/filtering update, the real-device viewer passed a **601.8-second** viewing session with 552 memory samples and normal closure. The CLI reported 34,981 GUI redraws and fresh real video at completion.

| Private memory | Viewer | Helper |
| --- | ---: | ---: |
| First 30-sample window, mean | 346.4 MiB | 32.2 MiB |
| Last 30-sample window, mean | 305.8 MiB | 31.1 MiB |
| Maximum sampled | 354.0 MiB | 46.8 MiB |

This baseline is not a ten-minute test of the new GPU mip implementation. Its screenshot is `diagnostics\soak-20261025-001.png` and is not packaged.

## Input checks and limits

Earlier tests against the identified scratch console produced exact short manual-key and scheduled-paste output (`ab12cd34`). Longer mixed-case/punctuation runs were inconsistent, with repeated characters in some runs. Successful paste progress reflected completed host-side operations, not verified target text.

The full printable-ASCII/Tab/newline byte-count check did not complete: its controller permit missed the readiness deadline, so the payload was not sent. Its preparation command already contained repeated characters. This is **incomplete**, not passed.

No additional target typing was needed for this display/navigation update. Automatic input routing has policy/unit coverage; a complete new real-target key/mouse/paste verification remains outstanding. Existing release-latency and target-USB limitations are unchanged.

## Package boundary

The package contains the x64 viewer, x86 helper, launcher, documentation, third-party notices, cached dependency/font license texts, and checksums. The notice inventory contains **153** resolved Windows dependency/build-dependency entries; it is not a count of libraries linked into the executable. The viewer's icon images and Windows icon/version resources are embedded, so no external icon files are needed at runtime.

The package does **not** contain the proprietary runtime, decoder, driver, bitstream, or firmware. Keep the existing vendor installation.

No persistent firmware update, driver change, COM registration, registry edit, or forced termination of the original vendor viewer was performed by this update workflow.
