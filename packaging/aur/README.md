# AUR package for TMOP

`tmop-bin/` is a ready `PKGBUILD` and `.SRCINFO` for `tmop-bin` 0.11.0 (x86_64 and aarch64),
**not submitted**. It repackages the release's `TMOP-v0.11.0-linux-{x64,arm64}.tar.gz`.

## What it installs

Checked against the real tarball listing (and by running `package()` against the extracted
x64 archive into a scratch folder): `/usr/bin/tmop`, `/usr/share/applications/tmop.desktop`,
`/usr/share/icons/hicolor/{64x64,256x256,512x512,1024x1024}/apps/tmop.png`, the README under
`/usr/share/doc/tmop-bin` and `LICENSE` and `Michroma-OFL.txt` under `/usr/share/licenses/tmop-bin`.
`provides=tmop` and `conflicts=tmop` leave room for a source-built `tmop` package later.

- License: `GPL-3.0-only`, as `Cargo.toml` says (the `LICENSE` file is the GPLv3 text).
- Dependencies, from `readelf -d` of the x64 binary (needs `libgcc_s.so.1`, `libm`, `libc`; highest
  glibc symbol version used is 2.35, which every Arch system exceeds) plus
  `packaging/linux/package.sh`/`release.yml`: the window library loads `libxkbcommon-x11` with
  `dlopen` and panics without it, hence `depends=(glibc gcc-libs libxkbcommon-x11)`. X11, Wayland,
  OpenGL and the file-dialog portal are `dlopen`ed too and are `optdepends`.

## Not validated

`makepkg`, `namcap` and `makepkg --printsrcinfo` are not available here (WSL has only Ubuntu), so
`.SRCINFO` was written by hand in the format `makepkg --printsrcinfo` produces, and the package was
never built or started on Arch. The `optdepends` names and `!debug` should be checked by namcap.
Please run the steps below on an Arch machine before pushing.

## What you do to submit

1. Create an account on [aur.archlinux.org](https://aur.archlinux.org/register) and add your SSH
   public key under *My Account*.
2. Test the build in a clean environment:

   ```sh
   cd packaging/aur/tmop-bin
   makepkg --printsrcinfo > /tmp/srcinfo && diff /tmp/srcinfo .SRCINFO   # should match
   makepkg -si          # or: extra-x86_64-build (devtools) for a clean chroot
   namcap PKGBUILD tmop-bin-*.pkg.tar.zst
   tmop                 # starts?
   ```

3. Publish (the AUR repository is created by the first push; the name must be free):

   ```sh
   git clone ssh://aur@aur.archlinux.org/tmop-bin.git
   cp PKGBUILD .SRCINFO tmop-bin/ && cd tmop-bin
   git add PKGBUILD .SRCINFO
   git commit -m "tmop-bin 0.11.0"
   git push        # the AUR accepts the branch `master` only
   ```

   Rules: [AUR submission guidelines](https://wiki.archlinux.org/title/AUR_submission_guidelines),
   [PKGBUILD](https://wiki.archlinux.org/title/PKGBUILD).

## Updating for a new release

Bump `pkgver` (reset `pkgrel=1`), put the two `.sha256` values of the new
`TMOP-vX.Y.Z-linux-{x64,arm64}.tar.gz` into `sha256sums_*` (or run `updpkgsums`), regenerate
`.SRCINFO` with `makepkg --printsrcinfo > .SRCINFO`, and push. TMOP's own update check on Linux
only shows the command to update; for AUR installs that is your AUR helper.
