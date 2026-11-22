{
  description = "TMOP (Task Manager OverPowered): a skinnable system monitor";

  # nixos-26.05: the newest stable release; its rustc (1.95 or newer) meets the 1.95 that
  # egui/eframe 0.37 declare as rust-version. The exact revision is pinned in flake.lock.
  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-26.05";

  outputs =
    { self, nixpkgs }:
    let
      systems = [
        "x86_64-linux"
        "aarch64-linux"
      ];
      forAllSystems = f: nixpkgs.lib.genAttrs systems (system: f nixpkgs.legacyPackages.${system});

      # The version lives in Cargo.toml only.
      version = (builtins.fromTOML (builtins.readFile ./Cargo.toml)).package.version;

      mkTmop =
        pkgs:
        pkgs.rustPlatform.buildRustPackage {
          pname = "tmop";
          inherit version;

          src = pkgs.lib.cleanSource ./.;
          # No git dependencies, so no outputHashes are needed.
          cargoLock.lockFile = ./Cargo.lock;

          nativeBuildInputs = [
            pkgs.patchelf
            pkgs.addDriverRunpath
          ];

          # `cargo test` already runs in the normal CI (Windows and Linux). Several of its tests
          # need a Docker daemon, an X server or the host's /proc and /sys, none of which exist
          # in the Nix sandbox, so they are not run again here.
          doCheck = false;

          postInstall = ''
            # The cargo binary is called TMOP; the .desktop file, the .deb and the AppImage use tmop.
            mv $out/bin/TMOP $out/bin/tmop

            install -Dm644 packaging/linux/tmop.desktop $out/share/applications/tmop.desktop
            for size in 64 256 512 1024; do
              install -Dm644 assets/icon/tmop-$size.png \
                $out/share/icons/hicolor/''${size}x$size/apps/tmop.png
            done
            install -Dm644 LICENSE README.md assets/fonts/Michroma-OFL.txt -t $out/share/doc/tmop
          '';

          # The binary links only libc, libm and libgcc_s. winit, glutin and xkbcommon load
          # X11, Wayland, EGL/GL and xkbcommon with dlopen at start (the .deb lists the same).
          # They go on the binary's RUNPATH, so nothing leaks into the programs TMOP starts.
          postFixup = ''
            patchelf --add-rpath ${
              pkgs.lib.makeLibraryPath [
                pkgs.libGL # libGL.so.1 and libEGL.so.1 (libglvnd)
                pkgs.libxkbcommon # libxkbcommon.so.0 and libxkbcommon-x11.so.0
                pkgs.wayland # libwayland-client.so.0, libwayland-egl.so.1
                pkgs.libx11
                pkgs.libxcursor
                pkgs.libxi
                pkgs.libxrender
                pkgs.libxcb
              ]
            } $out/bin/tmop
            # Adds /run/opengl-driver/lib: the graphics driver's GL/EGL and, optional, NVML
            # (libnvidia-ml.so.1) on NixOS.
            addDriverRunpath $out/bin/tmop
          '';

          meta = {
            description = "Task Manager OverPowered: a skinnable system monitor";
            homepage = "https://tekikaito.github.io/tmop/";
            license = pkgs.lib.licenses.gpl3Only;
            mainProgram = "tmop";
            platforms = systems;
          };
        };
    in
    {
      packages = forAllSystems (pkgs: rec {
        tmop = mkTmop pkgs;
        default = tmop;
      });

      apps = forAllSystems (pkgs: {
        default = {
          type = "app";
          program = "${self.packages.${pkgs.stdenv.hostPlatform.system}.default}/bin/tmop";
        };
      });
    };
}
