# TMOP — Task Manager OverPowered

TMOP shows what your computer is doing: how busy the processor, memory, disk, network and
graphics card are, which programs are running, and much more. It runs on **Windows, Linux and
macOS**, is free and needs no installation.

TMOP itself sends no data. On Linux and macOS it makes no internet requests at all. On Windows its
only internet requests go to GitHub: downloading TMOP's own Docker engine when you set it up, and
checking for a newer engine release (switch the check off on the Docker page's *Engine* tab). A
Docker engine downloads only what you ask it to (for example an image you pull).

![TMOP overview](docs/screenshots/summary.png)

## Get TMOP

**[Download TMOP](https://tekikaito.github.io/tmop/)**: the page picks the right file for your
computer. Or install it with one command, which also keeps it up to date when you run it again.

### Windows 10 and 11

In PowerShell:

```powershell
irm https://tekikaito.github.io/tmop/install.ps1 | iex
```

That puts TMOP in the Start menu, and you remove it in *Settings → Apps*. No administrator
permission is needed.

To download it by hand instead:

1. Open the **[release page](https://github.com/tekikaito/tmop/releases/latest)**.
2. Under **Assets**, click **`TMOP-windows-x64.exe`**.
   (Only if your PC has an ARM processor, for example a Snapdragon laptop, take
   `TMOP-windows-arm64.exe` instead. Not sure? Choose x64.)
3. Move it to a place you like, such as your desktop, and double-click it.

**Windows shows a blue "Windows protected your PC" window?** That happens with new programs
that Windows doesn't know yet. Click **More info**, then **Run anyway**. You only have to do this
once.

### Linux (x86_64 and ARM64)

In a terminal:

```sh
curl -fsSL https://tekikaito.github.io/tmop/install.sh | sh
```

That puts `tmop` in `~/.local/bin`, adds TMOP to your applications menu with its icon, and
checks the download's SHA-256. It needs no root. Add `-s -- --appimage` after `sh` to install
the AppImage instead of the plain program.

To download it by hand instead, take one of these from the
**[release page](https://github.com/tekikaito/tmop/releases/latest)** (`arm64` for ARM computers
such as a Raspberry Pi with a 64-bit system, `x64` for everything else):

- **`TMOP-linux-x64.AppImage`**: one file. Make it executable (*Properties → Allow executing file
  as program*, or `chmod +x TMOP-linux-x64.AppImage`) and double-click it. AppImages need FUSE;
  without it, start it with `--appimage-extract-and-run` or use the install command.
- **`TMOP-linux-x64.tar.gz`**: the `tmop` program, `tmop.desktop` and the icons, to unpack
  wherever you like.

TMOP needs glibc 2.35 or newer (Ubuntu 22.04, Debian 12, Fedora 36 or later) and runs on X11 and
Wayland.

### macOS 11 or newer (Apple Silicon and Intel)

In Terminal:

```sh
curl -fsSL https://tekikaito.github.io/tmop/install.sh | sh
```

That puts TMOP.app in `~/Applications` (the *Applications* folder in your home folder) and checks
the download's SHA-256. Apps installed this way start without the "could not verify" prompt.

To download it by hand instead, take **`TMOP-macos-universal.dmg`** from the
**[release page](https://github.com/tekikaito/tmop/releases/latest)**, open it and drag TMOP onto
*Applications*. One app runs on Apple Silicon and Intel Macs.

**macOS says "Apple could not verify TMOP is free of malware"?** TMOP isn't notarized by Apple,
so macOS asks before the first start. Click **Done**, open *System Settings → Privacy &
Security*, scroll down to *Security* and click **Open Anyway** next to *"TMOP" was blocked*.
Confirm with your password and click **Open Anyway** once more. You only have to do this once.
(On macOS 14 and older, Control-click TMOP in *Applications*, choose *Open* and confirm.)

## What you can do with it

| See your computer's performance live | Find and close programs |
|---|---|
| ![Performance graphs](docs/screenshots/performance.png) | ![List of running programs](docs/screenshots/processes.png) |
| **Test how fast your computer is** | **Keep a small monitor on your desktop** |
| ![Speed test results](docs/screenshots/benchmarks.png) | ![Small monitor window](docs/screenshots/popout.png) |

- **Close a program that hangs:** go to *Processes*, right-click it and choose *End task*.
- **Find out what fills your disk:** go to *Disk Space* and start a scan.
- **See which programs start with your computer, which apps are installed** and a lot more in the
  menu on the left.
- **Open the small desktop monitor** with the *Popout* button at the bottom left.
- **Manage your Docker containers:** a *Docker* page shows your containers, images and volumes, with logs, stats and files; you can start, stop and remove them. On Windows it works with Docker Desktop or with TMOP's own engine, which you set up on the page's *Engine* tab (it needs WSL 2). On Linux and macOS it uses the Docker you already run (Docker Engine, rootless Docker, Podman, Docker Desktop, Colima or OrbStack); its *Local Docker* tab shows which one TMOP found and how to start it.

## Platforms

TMOP looks and works the same everywhere. Where a system can't provide something, TMOP hides it
instead of showing zeros.

| | Windows | Linux | macOS |
|---|---|---|---|
| Summary and Performance (CPU, memory, disk, network) | ✔ | ✔ (*Swap* instead of *Page file*) | ✔ (*Swap*; no committed memory) |
| Graphics card | ✔ with engines (3D, copy, video …) | AMD and NVIDIA (with NVIDIA's own driver), with engines where the driver reports them; not Intel yet | Usage and memory, no engines |
| Clock speeds | ✔ | ✔ | Intel Macs; none on Apple Silicon |
| Temperatures (*Thermals*) | ✔ | ✔ | — |
| Power & Freq | ✔ power plans | Battery, governor, power profile | Battery and power settings |
| Processes: end task, end tree, suspend, priority | ✔ | ✔ (raising priority needs root; no *Realtime*) | ✔ (no *Realtime*) |
| Processes: efficiency mode | ✔ | ✔ as idle scheduling | ✔ as *Background priority* |
| Processes: other users' processes | ✔ as administrator | End or renice asks for your password (pkexec) | Some details need root |
| Users, App history, System info, Benchmarks, Flight recorder | ✔ | ✔ | ✔ |
| Startup apps | ✔ | Autostart entries and user services | Launch agents and daemons (no Login Items) |
| Services | ✔ | systemd units | launchd jobs |
| Drivers | ✔ | as *Kernel modules* | as *Kernel extensions* |
| Installed apps | ✔ | Packages (dpkg, rpm, pacman, Flatpak, Snap) | .app bundles and Homebrew |
| Connections | ✔ | ✔ (owners of other users' sockets need root) | ✔ (your own processes) |
| Disk Space: scan and treemap | ✔ (fast MFT scan as administrator) | ✔ | ✔ (grant *Full Disk Access* for everything) |
| Disk Space: delete and Clean up | ✔ Recycle Bin | ✔ Trash | ✔ Trash |
| Docker: containers, logs, terminal, files, stacks | ✔ | ✔ | ✔ |
| Docker: TMOP's own engine | ✔ (WSL 2) | — (*Local Docker* status instead) | — (*Local Docker* status instead) |
| Popout always on top, header pin | ✔ | X11 only (not on Wayland) | ✔ |
| Skins, palettes, fonts, system light/dark theme | ✔ | ✔ | ✔ |
| Open TMOP instead of Task Manager | ✔ | — | — |
| Screensaver | ✔ (separate build) | — | — |

### Where TMOP keeps its files

| | Windows | Linux | macOS |
|---|---|---|---|
| Settings and palettes | `%LOCALAPPDATA%\TMOP` | `~/.config/tmop` (`$XDG_CONFIG_HOME`) | `~/Library/Application Support/TMOP` |
| Recordings, benchmarks, logs | `%LOCALAPPDATA%\TMOP` | `~/.local/share/tmop` (`$XDG_DATA_HOME`) | `~/Library/Application Support/TMOP` |
| Your skins | `%LOCALAPPDATA%\TMOP\Skins` | `~/.local/share/tmop/skins` | `~/Library/Application Support/TMOP/Skins` |
| Installed by the install command | `%LOCALAPPDATA%\Programs\TMOP` | `~/.local/bin/tmop`, `~/.local/share/applications/tmop.desktop`, icons in `~/.local/share/icons/hicolor` | `~/Applications/TMOP.app` |

## Open TMOP instead of Windows Task Manager

*Windows only.* In *Settings*, under *Task Manager*, turn on **Open TMOP instead of Windows Task
Manager**. From then on, Ctrl+Shift+Esc, *Task Manager* in the taskbar menu and `taskmgr` all
open TMOP. Windows asks for administrator permission, and the change applies to every user of
the PC.

- Pressing Ctrl+Shift+Esc again brings the open TMOP window to the front instead of opening a
  second one.
- You can still reach the real Task Manager: *File* → *Open Windows Task Manager*.
- Windows remembers where TMOP.exe is. **Before you move or delete TMOP.exe, turn the setting
  off**; otherwise Ctrl+Shift+Esc opens nothing. If you start a moved copy, TMOP offers to fix it.
- To turn it off without the window, run `TMOP.exe --restore-taskmgr`.
- If TMOP.exe is already gone, open *Command Prompt* as administrator and run:

  ```
  reg delete "HKLM\SOFTWARE\Microsoft\Windows NT\CurrentVersion\Image File Execution Options\taskmgr.exe" /v Debugger /f
  ```

## Update or remove

TMOP doesn't update itself. Your settings are kept when you update.

- **Installed with a command?** Run the same command again to update.
  - Windows: remove TMOP in *Settings → Apps → Installed apps* (that also hands Ctrl+Shift+Esc
    back to Task Manager).
  - Linux and macOS: remove it with
    `curl -fsSL https://tekikaito.github.io/tmop/install.sh | sh -s -- --uninstall`.
    It removes exactly the files it installed.
- **Downloaded by hand?** Download the newest version and replace the old file (TMOP.exe, the
  AppImage, or TMOP in *Applications*). On Windows, keep the new TMOP.exe in the same place if
  TMOP opens instead of Task Manager. To remove TMOP, delete that file; on Windows, if TMOP opens
  instead of Task Manager, turn that off first (see above).
- **To also remove your settings**, delete the folders listed under
  [Where TMOP keeps its files](#where-tmop-keeps-its-files).

## Good to know

- The Docker page appears when Docker is installed or TMOP's own engine can run on this computer; you can hide it in *Settings*.
- Run TMOP as your normal user. On Linux and macOS some details (other users' processes, socket
  owners, protected folders) need root or, on macOS, *Full Disk Access*; TMOP says so where it
  matters. After updating TMOP on macOS, remove it from *Full Disk Access* and add it again:
  macOS ties the permission to the exact build, so the switch can stay on but no longer apply.
- TMOP is an independent project. It is inspired by *Task Manager TMOG* but is not connected to
  TMOG or its makers.
- Free and open source under the [GPL-3.0](LICENSE) license. To build it yourself, install
  [Rust](https://rustup.rs) and run `cargo build --release`.
