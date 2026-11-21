# Chocolatey package for TMOP

Sources for the `tmop` package (0.15.0) on the
[Chocolatey Community Repository](https://community.chocolatey.org/). Tracked in
[#119](https://github.com/tekikaito/tmop/issues/119). **Status: prepared, not published.**
Nothing here has been pushed; the maintainer needs a Chocolatey account first.

| File | Purpose |
| --- | --- |
| `tmop/tmop.nuspec` | Package metadata |
| `tmop/tools/chocolateyInstall.ps1` | Downloads the release zip, checks its SHA256, unpacks it, adds the `.gui` shim marker |

## Design

Portable package, same idea as the winget and Scoop packages: TMOP has no installer.
`Install-ChocolateyZipPackage` downloads the versioned zip from the GitHub release
(`TMOP-v0.15.0-windows-x64.zip`) and unpacks it into the package's `tools` folder. Chocolatey
shims `TMOP.exe` as the command `tmop`. The install script writes an empty `TMOP.exe.gui` next to
it so the shim does not block the console while the GUI runs
([shim docs](https://docs.chocolatey.org/en-us/create/create-packages/)). No binaries are inside
the `.nupkg`, so the `LICENSE.txt`/`VERIFICATION.txt` rules for embedded binaries do not apply.
The checksum is passed as `-Checksum64`/`-ChecksumType64`, as the validator rules CPMR0027 and
CPMR0073 require.

- **x64 only.** `Install-ChocolateyZipPackage` only knows `Url` (32-bit) and `Url64bit`; it has no
  ARM64 selection. On Windows on ARM the x64 build runs under emulation. The native ARM64 zip stays
  on the GitHub release (Scoop and winget offer it).
- **Icon.** The validator flags raw GitHub icon URLs (CPMR0076) and the project site does not host
  the icon, so `iconUrl` uses jsDelivr, pinned to the release tag.
- TMOP is not code-signed (stated in the description). Chocolatey's scanner may flag an unsigned
  exe; explain it in a reply if asked.

## What was and was not verified

Verified: SHA256 value equals the release's `.sha256` file. Not verified by the author of this
package: `choco pack` and `choco install` were not run (Chocolatey was not installed on the
authoring machine). Run the checks below before pushing.

## Steps for the maintainer

1. Create an account at <https://community.chocolatey.org/account/Register> and confirm the email
   (moderator messages are sent by email).
2. Copy your API key from <https://community.chocolatey.org/account> (Your API Key).
3. Install Chocolatey ([instructions](https://chocolatey.org/install)). Use a test VM for step 5.
4. Build the package:

   ```powershell
   cd packaging\chocolatey\tmop
   choco pack
   ```

   This creates `tmop.0.15.0.nupkg` in the current folder. Do not commit it.
5. Test it (in a VM or Sandbox, as administrator):

   ```powershell
   choco install tmop --source . -y
   tmop            # starts TMOP, the console returns immediately
   choco uninstall tmop -y
   ```
6. Save the key once, then push:

   ```powershell
   choco apikey --api-key <YOUR_KEY> --source https://push.chocolatey.org/
   choco push tmop.0.15.0.nupkg --source https://push.chocolatey.org/
   ```

## What to expect from moderation

Per the [moderation docs](https://docs.chocolatey.org/en-us/community-repository/moderation/):

- The package shows as *Pending*; automated checks start about 30 minutes later: validator (rules
  and guidelines), verifier (install and uninstall in a clean machine), scanner (virus scan).
- If all pass, a human moderator reviews it (*Ready*). A new package is not trusted, so expect a
  review and possibly questions; they appear on the package page and by email.
- *Waiting* means you must act: fix the listed **requirements** and push the same version again, or
  reply on the package page. **Guidelines** are suggestions and cannot block approval.
- Reminder after 20 days of silence, automatic rejection 15 days later (reversible within a year).
- Once approved the package is public. Check the final result at
  `https://community.chocolatey.org/packages/tmop`.
- After approval, add `choco install tmop` to the README ("Get TMOP") and mention it in the release
  notes.

## Updating for each release

1. Get the new checksum from the release (the same file the other packages use):

   ```powershell
   gh release download vX.Y.Z -R tekikaito/tmop -p "TMOP-vX.Y.Z-windows-x64.zip.sha256" -D $env:TEMP
   Get-Content $env:TEMP\TMOP-vX.Y.Z-windows-x64.zip.sha256
   ```
2. In `tmop/tmop.nuspec` change `<version>`, the `v0.15.0` in `iconUrl`, `licenseUrl`, `docsUrl`,
   `releaseNotes` and the zip name in the description.
3. In `tmop/tools/chocolateyInstall.ps1` change `Url64bit` (version twice) and `Checksum64`.
4. `choco pack`, test as above, `choco push`. Each pushed version is moderated again.
