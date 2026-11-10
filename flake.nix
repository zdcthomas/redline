{
  description = "redline — a keyboard-driven TUI spec editor where humans write and agents suggest";

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";

  outputs = { self, nixpkgs }:
    let
      systems = [ "x86_64-linux" "aarch64-linux" "x86_64-darwin" "aarch64-darwin" ];
      forAll = f: nixpkgs.lib.genAttrs systems (system: f nixpkgs.legacyPackages.${system});
    in
    {
      packages = forAll (pkgs: rec {
        redline = pkgs.rustPlatform.buildRustPackage {
          pname = "redline";
          version = (pkgs.lib.importTOML ./Cargo.toml).package.version;
          src = pkgs.lib.cleanSource ./.;
          cargoLock.lockFile = ./Cargo.lock;
          postInstall = ''
            mkdir -p $out/share/redline
            cp -r skill $out/share/redline/skill
            cp -r nvim $out/share/redline/nvim
          '';
          meta = {
            description = "Keyboard-driven TUI spec editor: humans write, agents suggest";
            mainProgram = "redline";
          };
        };
        # The native app (egui). It needs the window-system libraries at run time.
        redline-app = pkgs.rustPlatform.buildRustPackage {
          pname = "redline-app";
          version = (pkgs.lib.importTOML ./Cargo.toml).package.version;
          src = pkgs.lib.cleanSource ./.;
          cargoLock.lockFile = ./Cargo.lock;
          cargoBuildFlags = [ "-p" "redline-app" ];
          cargoTestFlags = [ "-p" "redline-app" ];
          postFixup = pkgs.lib.optionalString pkgs.stdenv.hostPlatform.isLinux ''
            patchelf --add-rpath ${pkgs.lib.makeLibraryPath (with pkgs; [
              wayland libxkbcommon libGL libx11 libxcursor libxi libxrandr
            ])} $out/bin/redline-app
          '';
          meta = {
            description = "Native app for redline";
            mainProgram = "redline-app";
          };
        };
        # The Neovim plugin. It calls the `redline` binary on $PATH.
        redline-nvim = pkgs.vimUtils.buildVimPlugin {
          pname = "redline-nvim";
          version = (pkgs.lib.importTOML ./Cargo.toml).package.version;
          src = ./nvim;
        };
        default = redline;
      });

      apps = forAll (pkgs: {
        default = {
          type = "app";
          program = "${self.packages.${pkgs.system}.redline}/bin/redline";
        };
      });

      devShells = forAll (pkgs: {
        default = pkgs.mkShell {
          packages = with pkgs; [ cargo rustc rustfmt clippy rust-analyzer ];
        };
      });
    };
}
