# winget manifests for TMOP

Prepared for `tekikaito.TMOP` 0.11.0, **not submitted**. Submitting is a pull request to
[microsoft/winget-pkgs](https://github.com/microsoft/winget-pkgs) from your own GitHub account.

| File | Manifest type |
| --- | --- |
| `tekikaito.TMOP.yaml` | `version` |
| `tekikaito.TMOP.installer.yaml` | `installer` |
| `tekikaito.TMOP.locale.en-US.yaml` | `defaultLocale` |

All three use manifest schema **1.28.0**, the newest folder under
[doc/manifest/schema](https://github.com/microsoft/winget-pkgs/tree/master/doc/manifest/schema)
in winget-pkgs when this was written. Installing from them needs a winget client that knows 1.28.0;
`winget validate` will say if yours does not (then update *App Installer*).

## Why a zip with a portable exe

TMOP has no installer: `TMOP.exe` is one self-contained program. winget handles that with
`InstallerType: zip` plus `NestedInstallerType: portable`
([installer schema](https://github.com/microsoft/winget-pkgs/blob/master/doc/manifest/schema/1.28.0/installer.md):
zip support since client 1.5, portable since 1.3, `PortableCommandAlias` only valid for portable).
winget unpacks the zip into `%LOCALAPPDATA%\Microsoft\WinGet\Packages\tekikaito.TMOP_...`, links
`TMOP.exe` as the command `tmop` and registers it in *Settings > Apps* for uninstall.

I chose the versioned zips (`TMOP-v0.11.0-windows-x64.zip`, `...-arm64.zip`) over the bare
`TMOP-windows-x64.exe` because winget needs a URL whose file never changes (the hash is pinned),
and the bare exe is published under a version-less name that is replaced by every release. The zip
holds `TMOP.exe`, `LICENSE`, `README.md`, `Michroma-OFL.txt` and `PawnIO-Modules-LGPL-2.1.txt`
(checked by listing the real archives). The `-server.zip` (Mesa software OpenGL) is not included;
it can become a second package later.

## What I validated, and what not

- SHA256 values are the ones in the release's `.sha256` files and were re-checked against the
  downloaded zips (`sha256sum -c`). winget wants them upper case; they are.
- Each file validates against the official JSON schema
  `winget-cli/schemas/JSON/manifests/v1.28.0/manifest.{version,installer,defaultLocale}.1.28.0.json`
  (checked with ajv; the only complaint was `ReleaseDate`, which the YAML library read as a date
  object instead of the string winget expects, a limitation of my checker).
- **Not validated:** `winget validate`, a real `winget install --manifest`, Windows Sandbox, and
  winget-pkgs' own validation pipeline. winget is not run from this repository's tooling; do that
  step yourself (below).

## What you do to submit

Needs: a GitHub account (yours: tekikaito), the winget client, and the
[CLA](https://cla.opensource.microsoft.com/microsoft/winget-pkgs) signed when the bot asks.
Rules and checks are described in
[doc/Authoring.md](https://github.com/microsoft/winget-pkgs/blob/master/doc/Authoring.md),
[doc/Validation.md](https://github.com/microsoft/winget-pkgs/blob/master/doc/Validation.md) and
[doc/Policies.md](https://github.com/microsoft/winget-pkgs/blob/master/doc/Policies.md).

1. Check the files locally (also try the real install, ideally in Windows Sandbox, see
   [SandboxTest](https://github.com/microsoft/winget-pkgs/blob/master/doc/tools/SandboxTest.md)):

   ```powershell
   winget validate --manifest packaging\winget
   winget settings --enable LocalManifestFiles
   winget install --manifest packaging\winget
   tmop        # starts TMOP; then: winget uninstall tekikaito.TMOP
   ```

   (If `winget validate` objects to this README sitting in the folder, copy the three `.yaml`
   files to a fresh folder and point it there.)

2. Fork `microsoft/winget-pkgs`, clone your fork, create a branch such as `tekikaito.TMOP-0.11.0`
   and copy the three files to

   ```
   manifests/t/tekikaito/TMOP/0.11.0/
   ```

   (partition `t` = first letter of the identifier, lower case; folder names are `Publisher/Package/version`).

3. Commit, push the branch, open a pull request against `master`. Fill in the PR template. The
   bot adds labels; watch the checks ([doc/Validation.md](https://github.com/microsoft/winget-pkgs/blob/master/doc/Validation.md)
   explains each `Validation-*` label; `@wingetbot run` re-triggers a run).

Alternative to steps 2 and 3, with [wingetcreate](https://github.com/microsoft/winget-create)
(`winget install wingetcreate`), which also fills hashes itself and opens the PR:

```powershell
wingetcreate new https://github.com/tekikaito/tmop/releases/download/v0.11.0/TMOP-v0.11.0-windows-x64.zip https://github.com/tekikaito/tmop/releases/download/v0.11.0/TMOP-v0.11.0-windows-arm64.zip
```

(then pick the nested `TMOP.exe` as portable and review each prompt against these files).

**Later releases**, once the package exists, one command per release:

```powershell
wingetcreate update tekikaito.TMOP --version 0.11.1 `
  --urls https://github.com/tekikaito/tmop/releases/download/v0.11.1/TMOP-v0.11.1-windows-x64.zip `
         https://github.com/tekikaito/tmop/releases/download/v0.11.1/TMOP-v0.11.1-windows-arm64.zip `
  --submit
```

(`--submit` needs a GitHub token; `wingetcreate token --store` saves one.) This could later become a
release-workflow step, as the Scoop one is; it was left out because it posts to a repository
outside this one.

## Risks specific to TMOP

winget-pkgs scans every installer with several antivirus engines, a Microsoft Defender scan after
installing, and the Potentially Unwanted Application criteria. **A package flagged by any scan
cannot be accepted, whatever its intent** ([Policies.md](https://github.com/microsoft/winget-pkgs/blob/master/doc/Policies.md#security-scans-and-potentially-unwanted-applications-pua),
[Validation.md](https://github.com/microsoft/winget-pkgs/blob/master/doc/Validation.md)).
Things in TMOP that can trip heuristics (read from the source):

- **The exe is not code-signed.** SmartScreen reputation of the download URL (GitHub releases) and
  of an unsigned, rarely-downloaded exe is low, and Windows shows "Windows protected your PC" on a
  by-hand download (README says so). winget downloads do not carry the web mark, but the
  validation scanners still see an unsigned 20 MB Rust exe.
- **It can replace Task Manager.** `src/taskmgr.rs`: when the user turns it on, TMOP (through an
  elevated copy of itself, `--set-taskmgr-replacement=on`) writes the `Debugger` value under
  `HKLM\SOFTWARE\Microsoft\Windows NT\CurrentVersion\Image File Execution Options\taskmgr.exe`,
  and can remove the "run as administrator" compatibility flag. Image File Execution Options
  debugger hijacking is a known malware persistence technique, so behavioural engines may react to
  the strings or the code path. It is strictly opt-in, and `--restore-taskmgr` undoes it.
- **It embeds PawnIO modules.** `src/metrics/pawnio.rs`: two signed PawnIO modules
  (`assets/pawnio`, LGPL-2.1-or-later) are bundled to read CPU temperature through the separately
  installed PawnIO driver. Opt-in, needs administrator rights, never opens the driver while off.
  The driver itself is not part of TMOP. A binary that talks to a kernel driver and contains
  driver payload-like blobs is the sort of thing heuristics flag.
- It also starts elevated copies of itself (UAC prompt) for these helpers and for the "open the
  real Task Manager" feature (`--open-taskmgr`).

If validation reports a Defender/AV detection, submit the zip to
[Microsoft Defender file submission](https://www.microsoft.com/wdsi/filesubmission) as a false
positive with the PR URL and ask a moderator to re-run with `@wingetbot run` (procedure from
Validation.md). Code signing the release (for example with Azure Trusted Signing or SignPath for
open source) would remove most of the risk; it is the real fix.

Also note: winget-installed TMOP lives in a package folder, and TMOP's own *Update now* replaces
`TMOP.exe` in place, which winget does not know about (`winget upgrade` still compares against the
manifest version). Updating through `winget upgrade tekikaito.TMOP` is the consistent way.
