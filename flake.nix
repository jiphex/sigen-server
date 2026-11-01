{
  description = "Sigenergy dashboard, Prometheus exporter and Modbus simulator";

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixpkgs-unstable";

  outputs = { self, nixpkgs }:
    let
      systems = [ "aarch64-linux" "x86_64-linux" "aarch64-darwin" ];
      forAllSystems = nixpkgs.lib.genAttrs systems;
    in
    {
      packages = forAllSystems (system:
        let pkgs = nixpkgs.legacyPackages.${system};
        in {
          default = self.packages.${system}.sigen-server;
          sigen-server = pkgs.rustPlatform.buildRustPackage {
            pname = "sigen-server";
            version = (builtins.fromTOML (builtins.readFile ./crates/sigen-server/Cargo.toml)).package.version;
            src = pkgs.lib.fileset.toSource {
              root = ./.;
              fileset = pkgs.lib.fileset.unions [ ./Cargo.toml ./Cargo.lock ./LICENSE ./crates ];
            };
            cargoLock.lockFile = ./Cargo.lock;
            cargoBuildFlags = [ "--workspace" ];
            cargoTestFlags = [ "--workspace" ];
            # MQTT's TLS defaults load platform certificates, including in tests.
            env.SSL_CERT_FILE = "${pkgs.cacert}/etc/ssl/certs/ca-bundle.crt";
            meta = {
              description = "Sigenergy dashboard, Prometheus exporter and Modbus simulator";
              mainProgram = "sigen-server";
              license = pkgs.lib.licenses.bsd3;
              platforms = systems;
            };
          };
        });

      apps = forAllSystems (system: {
        default = self.apps.${system}.sigen-server;
        sigen-server = {
          type = "app";
          program = "${self.packages.${system}.sigen-server}/bin/sigen-server";
        };
        sigen-simulator = {
          type = "app";
          program = "${self.packages.${system}.sigen-server}/bin/sigen-simulator";
        };
        sigen-journal = {
          type = "app";
          program = "${self.packages.${system}.sigen-server}/bin/sigen-journal";
        };
      });

      devShells = forAllSystems (system:
        let pkgs = nixpkgs.legacyPackages.${system};
        in {
          default = pkgs.mkShell {
            inputsFrom = [ self.packages.${system}.sigen-server ];
            packages = with pkgs; [ cargo rustc rustfmt clippy rust-analyzer ];
            SSL_CERT_FILE = "${pkgs.cacert}/etc/ssl/certs/ca-bundle.crt";
          };
        });

      checks = forAllSystems (system: {
        inherit (self.packages.${system}) sigen-server;
      });

      nixosModules.default = import ./nix/module.nix self;
    };
}
