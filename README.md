# TMOP — Task Manager OverPowered

TMOP shows what your Windows PC is doing: how busy the processor, memory, disk, network and
graphics card are, which programs are running, and much more. It is free and needs no installation.
TMOP itself sends no data and makes no internet requests. If you use the Docker page, your Docker
engine downloads only what you ask it to (for example an image you pull).

![TMOP overview](docs/screenshots/summary.png)

## Get TMOP

1. Open the **[download page](https://github.com/tekikaito/tmop/releases/latest)**.
2. Under **Assets**, click **`TMOP-…-windows-x64.zip`**.
   (Only if your PC has an ARM processor, for example a Snapdragon laptop, take
   `TMOP-…-windows-arm64.zip` instead. Not sure? Choose x64.)
3. Open the downloaded zip file and drag **TMOP.exe** to a place you like, such as your desktop.
4. Double-click **TMOP.exe**.

That's it. There is no installer.

**Windows shows a blue "Windows protected your PC" window?** That happens with new programs
that Windows doesn't know yet. Click **More info**, then **Run anyway**. You only have to do this
once.

## What you can do with it

| See your PC's performance live | Find and close programs |
|---|---|
| ![Performance graphs](docs/screenshots/performance.png) | ![List of running programs](docs/screenshots/processes.png) |
| **Test how fast your PC is** | **Keep a small monitor on your desktop** |
| ![Speed test results](docs/screenshots/benchmarks.png) | ![Small monitor window](docs/screenshots/popout.png) |

- **Close a program that hangs:** go to *Processes*, right-click it and choose *End task*.
- **Find out what fills your disk:** go to *Disk Space* and start a scan.
- **See which programs start with Windows, which apps are installed** and a lot more in the menu
  on the left.
- **Open the small desktop monitor** with the *Popout* button at the bottom left.
- **Manage your Docker containers:** if Docker Desktop is installed, a *Docker* page shows your containers, images and volumes, with logs, stats and files; you can start, stop and remove them.

## Update or remove

- **Update:** download the newest version as above and replace your old TMOP.exe with it.
  Your settings are kept.
- **Remove:** delete TMOP.exe. To also remove its settings, delete the folder
  `%LOCALAPPDATA%\TMOP` (paste that into the address bar of File Explorer).

## Good to know

- TMOP runs on Windows 10 and 11. Versions for Linux and Mac are planned.
- The Docker page appears only when Docker is installed; you can hide it in *Settings*.
- TMOP is an independent project. It is inspired by *Task Manager TMOG* but is not connected to
  TMOG or its makers.
- Free and open source under the [GPL-3.0](LICENSE) license. To build it yourself, install
  [Rust](https://rustup.rs) and run `cargo build --release`.
