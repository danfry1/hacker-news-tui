{
  description = "A delightful terminal UI for browsing Hacker News";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    flake-utils.url = "github:numtide/flake-utils";
  };

  outputs =
    {
      self,
      nixpkgs,
      flake-utils,
    }:
    flake-utils.lib.eachDefaultSystem (
      system:
      let
        pkgs = import nixpkgs { inherit system; };
        # Single source of truth: read name/version/metadata straight from
        # Cargo.toml so the flake never drifts from the crate and needs no
        # manual bump on release.
        cargoToml = (builtins.fromTOML (builtins.readFile ./Cargo.toml)).package;
      in
      {
        packages.default = pkgs.rustPlatform.buildRustPackage {
          pname = cargoToml.name;
          inherit (cargoToml) version;
          src = ./.;
          cargoLock.lockFile = ./Cargo.lock;

          # Keep build-time and host tool closures distinct, so the package
          # stays correct under cross-compilation.
          strictDeps = true;

          meta = {
            inherit (cargoToml) description homepage;
            license = pkgs.lib.licenses.mit;
            mainProgram = cargoToml.name;
          };
        };

        apps.default = flake-utils.lib.mkApp {
          drv = self.packages.${system}.default;
        };

        # `nix flake check` builds the package, which runs the test suite via
        # buildRustPackage's check phase.
        checks.default = self.packages.${system}.default;

        # `nix fmt` formats the flake with the official RFC-style formatter.
        formatter = pkgs.nixfmt-rfc-style;

        devShells.default = pkgs.mkShell {
          packages = [
            pkgs.cargo
            pkgs.rustc
            pkgs.clippy
            pkgs.rustfmt
            pkgs.rust-analyzer
          ];
        };
      }
    );
}
