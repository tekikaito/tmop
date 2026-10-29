# Handoff: TMOP v0.6.2 taggen und veröffentlichen

Repository: https://github.com/tekikaito/tmop (Default-Branch `main`)

## Hintergrund

Der AppImage-Katalog will TMOP aufnehmen: https://github.com/AppImage/appimage.github.io/pull/10866.
Der Test dort ist mit v0.6.1 gescheitert („The application exited within 11 seconds instead of
showing a window“). Ursache: winit lädt `libxkbcommon-x11` per dlopen, und die Bibliothek fehlt auf
dem Testrechner.

Behoben ist das in https://github.com/tekikaito/tmop/pull/25, gemergt in `main` als Merge-Commit
`5c682ab997221004f1898441520398fb1901853a`:

- Das AppImage bündelt jetzt `libxkbcommon.so.0`, `libxkbcommon-x11.so.0` und `libxcb-xkb.so.1`
  unter `usr/lib`; das macht `packaging/linux/package.sh`, `AppRun` setzt `LD_LIBRARY_PATH`.
- Der Release-Workflow installiert dafür `libxkbcommon-x11-0` auf dem Runner. Der Schritt „Check the
  AppImage“ prüft, dass die drei Bibliotheken im AppImage liegen.
- Theme „System“ ohne erkennbare Systemeinstellung ergibt jetzt Light statt Dark.

Es fehlt nur noch ein Release, das diese Änderungen enthält.

## So lief das letzte Release (v0.6.1), daran orientieren

- Ein Commit „TMOP 0.6.1“, der nur `version` in `Cargo.toml` und den Eintrag `name = "tmop"` in
  `Cargo.lock` ändert.
- Der Tag `v0.6.1` ist annotiert, mit der Nachricht `TMOP v0.6.1`, und sitzt auf dem Commit in
  `main`, der die neue Version enthält.
- Ein Tag-Push `v*` startet `.github/workflows/release.yml`. Der Workflow prüft, dass der Tag zur
  Version in `Cargo.toml` passt, baut alles für Windows, Linux (x64 und arm64), macOS und Engine,
  vergleicht die Dateien mit der erwarteten Liste und veröffentlicht das GitHub-Release.

## Schritte

1. Den aktuellen Stand holen und prüfen, dass der Fix enthalten ist:
   ```bash
   git checkout main && git pull origin main
   git merge-base --is-ancestor 5c682ab997221004f1898441520398fb1901853a HEAD && echo ok
   grep -n 'libxkbcommon-x11' packaging/linux/package.sh .github/workflows/release.yml
   ```
2. CI auf `main` prüfen: Der Workflow „CI“ für den Merge-Commit muss grün sein. Ist er rot, nicht
   taggen, sondern erst die Ursache klären. Besonders wichtig ist der Schritt „Test install.sh“: er
   führt `package.sh` aus, das jetzt `libxkbcommon-x11` auf dem Runner braucht.
3. Version anheben: in `Cargo.toml` `version = "0.6.1"` in `"0.6.2"` ändern, dann
   ```bash
   cargo update -p tmop --offline   # passt nur den tmop-Eintrag in Cargo.lock an
   git diff --stat                  # erwartet: nur Cargo.toml und Cargo.lock, je eine Zeile
   cargo build --locked             # muss ohne Lockfile-Fehler durchlaufen
   ```
4. Committen und pushen. Die Commit-Nachricht lautet `TMOP 0.6.2`.
   ```bash
   git commit -am "TMOP 0.6.2"
   git push origin main
   ```
   Lehnt GitHub den direkten Push nach `main` ab (Branch-Schutz), stattdessen einen Branch pushen,
   einen PR öffnen und mergen. Danach `main` erneut pullen und den Tag auf den Merge-Commit setzen.
5. Annotierten Tag setzen und pushen:
   ```bash
   git tag -a v0.6.2 -m "TMOP v0.6.2"
   git push origin v0.6.2
   ```
6. Den Workflow „Release“ für `v0.6.2` beobachten, bis er durch ist. Er dauert eine Weile, weil er
   Windows, Linux, macOS und Engine baut. Bei einem Fehler: Log lesen, Ursache beheben. Den Tag erst
   nach Rücksprache löschen und neu setzen.
7. Das Release prüfen:
   - https://github.com/tekikaito/tmop/releases/tag/v0.6.2 existiert und ist das neueste Release
     („Latest“).
   - Die Dateien `TMOP-v0.6.2-linux-x64.AppImage` und `TMOP-linux-x64.AppImage` sind dabei (arm64
     ebenso), jeweils mit `.sha256`.
   - Optional, auf einem Linux-Rechner:
     ```bash
     curl -fsSLO https://github.com/tekikaito/tmop/releases/download/v0.6.2/TMOP-v0.6.2-linux-x64.AppImage
     chmod +x TMOP-v0.6.2-linux-x64.AppImage
     ./TMOP-v0.6.2-linux-x64.AppImage --appimage-extract >/dev/null
     ls squashfs-root/usr/lib        # libxcb-xkb.so.1  libxkbcommon-x11.so.0  libxkbcommon.so.0
     ```
     Das Fenster muss erscheinen, auch ohne Netzwerk.

## Danach (nur nach Rückfrage bei Marc)

Im Katalog-PR https://github.com/AppImage/appimage.github.io/pull/10866 einen Kommentar mit genau
`/retest` schreiben. Der Katalog testet dann das neueste Release. Das ist ein öffentlicher Kommentar
in einem fremden Repository: vorher bestätigen lassen.

Erwartetes Ergebnis: Der Test besteht. Zwei Warnungen dürfen bleiben, beide sind bewusst so gewollt
und lassen den Test nicht scheitern:

- „linux“ im Dateinamen: Die Namen werden absichtlich nicht geändert, weil `install.sh`, die
  Download-Seite und bestehende Links an `TMOP-linux-<arch>.AppImage` hängen.
- Der Hinweis auf glibc 2.35.

Meldet der Test etwas anderes, das Log und die Fehlermeldung des Bots an Marc zurückgeben, nichts
auf eigene Faust ändern.

## Nicht tun

- Asset-Namen oder `package.sh` ändern.
- Einen bestehenden Tag überschreiben oder einen Force-Push auf `main` machen.
- Ohne Rückfrage im Katalog-Repository kommentieren.
