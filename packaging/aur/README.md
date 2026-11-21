# AUR package for TMOP

`tmop-bin/` holds a `PKGBUILD` and `.SRCINFO` for `tmop-bin` 0.14.1 (x86_64 and aarch64). It
repackages the release's `TMOP-v0.14.1-linux-{x64,arm64}.tar.gz`.

**Status: NOT uploaded to the AUR.** There is no AUR account yet.

## What it installs

Checked against the real 0.14.1 tarball listings (both architectures have the same layout under
`TMOP-v0.14.1-linux-{x64,arm64}/`): `/usr/bin/tmop`, `/usr/share/applications/tmop.desktop`,
`/usr/share/icons/hicolor/{64x64,256x256,512x512,1024x1024}/apps/tmop.png`, the README under
`/usr/share/doc/tmop-bin`, and `LICENSE` and `Michroma-OFL.txt` under `/usr/share/licenses/tmop-bin`.
That is every file in the tarball; no other licence file is shipped.
`provides=tmop` and `conflicts=tmop` leave room for a source-built `tmop` package later.

- License: `GPL-3.0-only`, as `Cargo.toml` says (the `LICENSE` file is the GPLv3 text).
- sha256 sums are the release's `.sha256` assets and were re-checked by hashing the downloaded
  tarballs.
- Dependencies: `readelf -d` of the x64 binary shows `libgcc_s.so.1`, `libm`, `libc` (highest glibc
  symbol version 2.35). The window library loads `libxkbcommon-x11` with `dlopen` and panics
  without it, hence `depends=(glibc gcc-libs libxkbcommon-x11)`. X11, Wayland, OpenGL, the
  file-dialog portal, `libdbus-1` and `libnvidia-ml` are `dlopen`ed (strings in the binary) and are
  `optdepends`.

## Verified and not verified

Verified: tarball layout vs. every path `package()` installs; sha256 sums; `readelf -d`/`ldd` and
`tmop --version` (prints `tmop 0.14.1`) of the x64 binary in WSL Ubuntu.

Not verified: `makepkg`, `namcap` and `makepkg --printsrcinfo` are not available here (WSL has only
Ubuntu). **`.SRCINFO` was written by hand** in the format `makepkg --printsrcinfo` produces and must
be regenerated with `makepkg --printsrcinfo > .SRCINFO` before the first upload. The package was
never built or started on Arch, and the aarch64 binary was not run. The `optdepends` names and
`!debug` should be checked with namcap.

## What you do to submit

1. Create an account on [aur.archlinux.org](https://aur.archlinux.org/register) and add your SSH
   public key under *My Account*.
2. Test the build in a clean environment:

   ```sh
   cd packaging/aur/tmop-bin
   makepkg --printsrcinfo > .SRCINFO     # regenerate; check with git diff that nothing changed
   makepkg -si          # or: extra-x86_64-build (devtools) for a clean chroot
   namcap PKGBUILD tmop-bin-*.pkg.tar.zst
   tmop                 # starts?
   ```

3. Publish (the AUR repository is created by the first push; the name must be free):

   ```sh
   git clone ssh://aur@aur.archlinux.org/tmop-bin.git
   cp PKGBUILD .SRCINFO tmop-bin/ && cd tmop-bin
   git add PKGBUILD .SRCINFO
   git commit -m "tmop-bin 0.14.1"
   git push        # the AUR accepts the branch `master` only
   ```

   Rules: [AUR submission guidelines](https://wiki.archlinux.org/title/AUR_submission_guidelines),
   [PKGBUILD](https://wiki.archlinux.org/title/PKGBUILD).

## Updating for a new release

Bump `pkgver` (reset `pkgrel=1`), put the two `.sha256` values of the new
`TMOP-vX.Y.Z-linux-{x64,arm64}.tar.gz` into `sha256sums_*` (or run `updpkgsums`), regenerate
`.SRCINFO` with `makepkg --printsrcinfo > .SRCINFO`, and push. TMOP's own update check on Linux
only shows the command to update; for AUR installs that is your AUR helper.
