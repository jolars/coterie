{
  description = "Project-native orchestration for coding agents";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    rust-overlay = {
      url = "github:oxalica/rust-overlay";
      inputs.nixpkgs.follows = "nixpkgs";
    };
  };

  outputs =
    {
      self,
      nixpkgs,
      rust-overlay,
    }:
    let
      systems = [
        "aarch64-linux"
        "x86_64-linux"
      ];
      forAllSystems = nixpkgs.lib.genAttrs systems;
      packageFor =
        system:
        let
          pkgs = import nixpkgs {
            inherit system;
            overlays = [ rust-overlay.overlays.default ];
          };
          toolchain = pkgs.rust-bin.fromRustupToolchainFile ./rust-toolchain.toml;
          rustPlatform = pkgs.makeRustPlatform {
            cargo = toolchain;
            rustc = toolchain;
          };
        in
        rustPlatform.buildRustPackage {
          pname = "coterie";
          version = (builtins.fromTOML (builtins.readFile ./Cargo.toml)).package.version;

          src = pkgs.lib.fileset.toSource {
            root = ./.;
            fileset = pkgs.lib.fileset.unions [
              ./Cargo.lock
              ./Cargo.toml
              ./LICENSE-APACHE
              ./LICENSE-MIT
              ./README.md
              ./docs
              ./examples
              ./schemas
              ./src
              ./tests
            ];
          };
          cargoLock.lockFile = ./Cargo.lock;
          # The submission tests execute real Git hooks.
          nativeCheckInputs = [ pkgs.gitMinimal ];

          meta = {
            description = "Project-native orchestration for coding agents";
            homepage = "https://github.com/jolars/coterie";
            license = with pkgs.lib.licenses; [
              asl20
              mit
            ];
            mainProgram = "coterie";
            platforms = pkgs.lib.platforms.linux;
          };
        };
    in
    {
      packages = forAllSystems (
        system:
        let
          coterie = packageFor system;
        in
        {
          inherit coterie;
          default = coterie;
        }
      );

      apps = forAllSystems (
        system:
        let
          app = {
            type = "app";
            program = "${self.packages.${system}.coterie}/bin/coterie";
            meta.description = "Run Coterie";
          };
        in
        {
          coterie = app;
          default = app;
        }
      );

      checks = forAllSystems (system: {
        default = self.packages.${system}.coterie;
      });

      formatter = forAllSystems (system: (import nixpkgs { inherit system; }).nixfmt);
    };
}
