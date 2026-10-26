# Feature coverage against the original viewer

Compared with StarTech USB Crash Cart Adapter 124.0415, using its menu and handler code (`actions.Actions.buildMenu`, `wxer.VideoSettingsWindow`, `adapter.Adapter`), read as static Python 2 bytecode. The original was never run for this comparison. Menu entries prove intent, not that an entry works on this adapter. `updateMenuState` model gating was not traced, so some original entries may be unavailable here anyway.

**Covered** works in the new viewer. **Hardware-checked** was observed on the real adapter. **Partial**, **gap** and **out of scope** say what is missing.

## Viewing

| Original | New viewer | Status |
| --- | --- | --- |
| Fullscreen, maximize, window resize | Fullscreen button, resizable window | Covered |
| Shrink to fit, fixed zoom steps | Fit, physical 1:1, smooth Ctrl+scroll and ± zoom | Covered; filtered reduction is new |
| Center, automatic edge scrolling | Centered Fit; drag and middle-drag panning | Covered differently |
| Video Settings: position arrows, Auto, Default | Display → Picture position: arrows (1 or 8 px), Auto align, Reset | Covered; X position hardware-checked |
| Video Settings: Save position (adapter EEPROM) | Position remembered per mode on this PC and reapplied | Covered differently; EEPROM is never written |
| Fine-tune after every mode change | Ported, same triggers, steps and limits | Covered; native edge detector seen only on a console, where it correctly makes no change |
| Video Settings: phase, noise, flatness, RGB gain and offset | Not available | Gap; ranges and readback not yet established |
| PNG snapshot | Not available; Windows screenshots work | Gap (minor) |
| Recording, loop recording, playback, movie | Not available | Out of scope |
| Video processing reset, timing report, DDC resolution choices | Not available | Out of scope (diagnostic) |

## Keyboard and mouse

| Original | New viewer | Status |
| --- | --- | --- |
| Transparent keyboard mode | Control mode captures automatically over focused video; F12 switches to Navigate | Covered |
| Ctrl+Alt+Del, special-key window | More → Special keys (F12, Ctrl+Alt+Del, Ctrl+Alt+Backspace, Print Screen, Pause) | Covered; fixed set |
| Keyboard layout choices | Physical key positions for typing; paste assumes US English | Partial |
| Keyboard lights | Caps Lock read for paste; lights not shown | Partial |
| Simulated hotplug | Not available | Gap |
| Disable mouse | Navigate mode keeps all input local | Covered |
| Capture release key and gestures | F12, leaving the video, focus loss | Covered |
| Middle button | Pans by default; More → Send middle button to target | Covered |
| Left/right button swap | Not available | Gap (minor) |
| Relative, macOS and forced-relative mouse modes | Absolute mouse only | Gap for targets that need relative mouse |

## Recovery and setup

| Original | New viewer | Status |
| --- | --- | --- |
| Reconnect after silent endpoints | One descriptor-checked USB reconnect at startup | Covered; hardware-checked |
| No-signal indication | No video signal screen with a wake action | Covered |
| — | Wake display · press Shift (works without video) | New; waking a sleeping target not yet verified |
| — | Paste clipboard as typed keys, with preview | New; experimental, see compatibility notes |
| Installer with driver preinstall | Setup installs driver, private runtime, VC++ runtime, shortcuts, uninstall | Covered |
| Firmware update, help, update check | Not available | Out of scope |

## Virtual USB drive (original Disk Control)

| Original | New viewer | Status |
| --- | --- | --- |
| Insert a disk image (.vhd) as a USB drive | USB drive → Disk image… (.img, fixed or dynamic .vhd) | Covered; hardware-checked with a raw image |
| Insert an .iso as a CD-ROM | USB drive → Disk image… (.iso) | Covered; hardware-checked, including the target's tray lock |
| Eject | Unplug, with a warning while the target holds the drive | Covered; hardware-checked |
| Target-side eject | Detected and shown | Covered; not yet observed on hardware |
| Drop files on the window | Drop files or folders: copied to the shared FAT32 drive, then plugged in | Covered |
| New disk image, add files, view, extract all | Send files… / Send folder…, file list, Save all to PC…, Erase… | Covered differently: one shared drive, grown automatically |
| Read/write speed and byte counters | Shown while plugged in | Covered |
| Target writes to the image | Served and stored | Covered by self-tests; not yet exercised by a target |
