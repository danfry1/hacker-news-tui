{
  description = "A delightful terminal UI for browsing Hacker News";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    flake-utils.url = "github:numtide/flake-utils";
  };

  outputs =
    { self, nixpkgs, flake-utils }:
    flake-utils.lib.eachDefaultSystem (
      system:
      let
        pkgs = import nixpkgs { inherit system; };
      in
      {
        packages.default = pkgs.rustPlatform.buildRustPackage {
          pname = "hacker-news-tui";
          version = "0.1.1";
          src = ./.;
          cargoLock.lockFile = ./Cargo.lock;

          meta = {
            description = "A delightful terminal UI for browsing Hacker News";
            homepage = "https://github.com/danfry1/hacker-news-tui";
            license = pkgs.lib.licenses.mit;
            mainProgram = "hacker-news-tui";
          };
        };

        apps.default = flake-utils.lib.mkApp {
          drv = self.packages.${system}.default;
        };

        devShells.default = pkgs.mkShell {
          packages = [ pkgs.cargo pkgs.rustc pkgs.clippy pkgs.rustfmt ];
        };
      }
    );
}
