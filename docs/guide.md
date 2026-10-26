# Guide

A Windows Rust viewer for the connected `152A:8463` VGA/keyboard/mouse adapter. The new application lives separately from the vendor software and uses its existing driver and decoder.

## Install

Run **`USB Crash Cart Setup 0.3.0.exe`** and accept the StarTech license for the adapter components. Setup needs administrator rights and installs:

- the viewer and its helper in `C:\Program Files\USB Crash Cart Viewer`;
- the adapter's signed Digital Multitools ULC driver 2.80, through `pnputil`;
- the StarTech runtime the helper uses (Python 2.7 runtime, decoder, registered COM transport and FPGA configuration), private to the viewer;
- Microsoft's Visual C++ 2008 x86 runtime, only when it is missing;
- a Start menu shortcut, an optional desktop shortcut, and an Apps & features entry.

The original StarTech application is not needed. If the adapter was plugged in during setup, unplug it and plug it back in. Remove the viewer from Apps & features; the shared driver stays installed. The setup contains StarTech's components, so keep it to machines covered by their license.

`USB Crash Cart Setup 0.3.0.exe --silent --accept-license [--desktop]` installs without a window; `--uninstall --silent` removes. `--list` and `--extract NEW_FOLDER` inspect the embedded files without installing anything.

## Run

Close any other viewer first; the adapter is acquired exclusively. Open **USB Crash Cart** from the Start menu.

For development, `.\scripts\run.ps1` or `dist\cart-viewer.exe` uses the original vendor installation instead of a private runtime. Keep `cart-device-helper.exe` beside the viewer.

The window keeps standard Windows controls, with a matching dark native title bar on Windows 11 and a dedicated monitor/terminal icon in the caption, taskbar, and executable.

- **Control** captures input automatically while the viewer is focused and your pointer is over ready video. Moving to the toolbar or losing focus releases it.
- **Navigate** keeps input local: hold **Ctrl** and scroll for smooth, pointer-centered zoom; drag to pan. **F12** switches between Control and Navigate.
- **Middle-drag** pans in both modes, like other image viewers. The press and release never reach the target. To send the middle button to the target instead, turn on **More → Send middle button to target** (Control mode only, until you disconnect).
- **Display → Picture position** moves the captured picture, like the original Video Settings arrows, in 1 px or 8 px steps. The position is remembered per video mode on this PC and reapplied whenever that mode appears. **Auto align** runs the original adapter's automatic alignment; **Reset** returns to the adapter default.
- **USB drive** plugs a virtual USB drive into the target. **Send files…**, **Send folder…** or dropping files on the window copies them onto the shared FAT32 drive and plugs it in. **Disk image…** plugs in a `.img` or `.vhd` as a USB flash drive, or an `.iso` as a CD-ROM. **Save all to PC…** copies everything on the shared drive, including what the target saved, into a new folder. Unplug asks first while the target is using the drive.
- **Wake display · press Shift** appears on the No video signal screen and in More. It sends one Shift press, even without video, to wake a sleeping target display.
- **Fit** shows the complete captured frame. **1:1** uses one source pixel per physical display pixel, aligned to the pixel grid even on high-DPI monitors. The zoom percentage is the actual physical scale.
- Reduced video uses GPU-prefiltered scaling to keep fine text and lines more stable. **Display → Crisp enlargement** controls pixelated enlargement; reduced images always stay filtered.
- **More → Special keys** sends keys reserved by the local viewer, including remote F12.
- **Paste…** reads plain text once, shows a preview, and types it on the target after confirmation. On narrow windows it moves into More. Captured Ctrl+V stays a target shortcut; it never reads the local clipboard.

The first version supports a **US English** target layout, printable ASCII, Tab, and line breaks. CRLF and CR are normalized, and no extra Enter is appended. Unsupported characters are rejected before sending anything. The limit is 16 KiB per paste.

Pasting is keyboard emulation, not clipboard sharing. It needs no software on the connected machine. Enter can run commands; Tab can autocomplete or move focus. Control keys require an additional acknowledgement in the preview. The default pause is conservative and can be increased. F12, cancellation, or losing window focus stops a paste and attempts to release held keys.

**Check a short sample in an editor before pasting commands.** Device-write completion does not acknowledge the text received by the target. Hardware testing on this setup has shown intermittent repeated characters, and the target console reports USB descriptor/EMI errors (`-71`). Do not treat the current development build as unattended command automation. Details and test limits are in [compatibility.md](compatibility.md).

## Requirements

- Windows x64 and a GPU supported by wgpu.
- The installed **USB Crash Cart Adapter** software at `C:\Program Files (x86)\USB Crash Cart Adapter`, including its registered 32-bit COM transport, Python 2.7 runtime, decoder, and signed driver.
- The supported adapter must be connected and have target-side VGA and keyboard/mouse USB connections.

This release is Rust-based, not independent of the vendor runtime: the x64 viewer starts a private x86 Rust helper, which loads the installed decoder. Proprietary DLLs, drivers, bitstreams, and firmware are **not** included in the new package. Do not remove the original installation.

## Build and test

Rust MSVC, Visual Studio C++ build tools, and the Windows SDK are required. Install the x86 Rust target if needed:

```powershell
rustup target add i686-pc-windows-msvc
.\scripts\build.ps1 -Test
```

The script builds the x86 helper and x64 viewer separately. Use `-Configuration Debug` for development. It never installs a driver or changes COM registration.

To assemble a local development package after building, use Python 3:

```powershell
python -I .\scripts\package.py --dry-run
python -I .\scripts\package.py
```

The packager creates `dist`, checks both executable architectures, and includes documentation, third-party notices, available dependency license files, and checksums. It refuses to replace an existing output folder. Use `--output NEW_FOLDER` for another package. Vendor components are never copied.

```powershell
# UI only; never opens the adapter
.\scripts\run.ps1 -Demo

# Real video/status probe, with the adapter's normal volatile initialization
.\target\i686-pc-windows-msvc\debug\cart-device-helper.exe --probe --initialize --seconds 10

# Verify runtime dependencies without opening hardware
.\target\i686-pc-windows-msvc\debug\cart-device-helper.exe --runtime-info
```

`scripts\hardware-smoke.py` is an explicit-input test harness and requires `--allow-input`. It never reads the local clipboard. Do not run its console-specific stages against an unknown target application. Screenshots remain local under the ignored `diagnostics` directory.

## Boundaries

The new viewer does not update persistent firmware, install or replace drivers, self-register COM components, elevate itself, or open a network server. The virtual USB drive serves image files from this PC; it never reads the target's own disks. Connection startup uses the vendor's verified volatile FPGA configuration. A silent adapter can receive one descriptor-checked USB reconnection attempt; a busy adapter is not reset or taken away from another application.

Automatic input is limited to the focused video image in Control mode, with fresh video and ready keyboard status. Local menus, paste preview, and ongoing paste/special-key work block acquisition. Held local keys or buttons are never inherited into the target. An existing remote drag keeps its final release within the window; leaving the window releases input. Windows-reserved shortcuts cannot all be intercepted; use the explicit remote special-key controls instead. Releases are best effort after an unplug or driver failure.

The missing horizontal edge was a capture offset, not a narrow capture. On the tested 1920 × 1080 at 60 Hz target, the adapter default placed the picture **45 px left**: the first five console characters were cut off and the right edge carried blanking. A −45 px picture position restores the full left edge, verified against the console's 8 px character grid, with the right edge intact. Picture position stores this per video mode. The original's automatic alignment is ported, but its native edge detector finds nothing on a mostly black console, so a text target needs the manual position once.

The font is Inter; its OFL license is included under `assets\fonts`. Dependency and vendor licensing remain separate.
