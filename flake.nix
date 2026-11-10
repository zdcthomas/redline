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
          '';
          meta = {
            description = "Keyboard-driven TUI spec editor: humans write, agents suggest";
            mainProgram = "redline";
          };
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
