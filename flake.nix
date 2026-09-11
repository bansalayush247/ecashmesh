{
  description = "EcashMesh development environment";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
  };

  outputs =
    { nixpkgs, ... }:
    let
      systems = [
        "aarch64-darwin"
        "x86_64-darwin"
        "x86_64-linux"
        "aarch64-linux"
      ];

      forAllSystems = nixpkgs.lib.genAttrs systems;
    in
    {
      devShells = forAllSystems (
        system:
        let
          pkgs = import nixpkgs { inherit system; };
        in
        {
          default = pkgs.mkShell {
            packages = [
              pkgs.cargo
              pkgs.cargo-watch
              pkgs.clippy
              pkgs.git
              pkgs.nodejs_22
              pkgs.openssl
              pkgs.pkg-config
              pkgs.pnpm
              pkgs.rust-analyzer
              pkgs.rustc
              pkgs.rustfmt
              pkgs.sqlite
            ];

            env = {
              RUST_BACKTRACE = "1";
            };

            shellHook = ''
              echo "EcashMesh dev shell"
              echo "Rust: $(rustc --version)"
              echo "Cargo: $(cargo --version)"
            '';
          };
        }
      );
    };
}
