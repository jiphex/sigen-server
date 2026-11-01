# Sigen server

Sigenergy web dashboard and Prometheus exporter, with a Modbus TCP client and
live inverter simulator. See the [server documentation](crates/sigen-server/README.md)
for configuration and the [Modbus documentation](crates/sigen-modbus/README.md)
for the library and simulator.

See [release notes](CHANGELOG.md) for changes and upgrade notes.

Licensed under the [BSD 3-Clause License](LICENSE).

## Nix

With Nix flakes enabled, run these commands from the repository root:

```sh
nix develop                          # Rust, Cargo, rustfmt, Clippy, rust-analyzer
nix build                            # result/bin/{sigen-server,sigen-simulator,sigen-journal}
nix flake check                      # Build and run the workspace tests
nix run . -- --address 192.168.1.50:502
nix run .#sigen-simulator -- --scenario cycle
```

The flake supports aarch64 and x86_64 Linux, and Apple Silicon macOS. `flake.lock` pins
nixpkgs (including the Rust toolchain); `Cargo.lock` pins Rust dependencies.
Use `nix flake update` to update nixpkgs.

## NixOS service

Add this repository as an input named `sigen-server` in your NixOS flake, then
import its module in your host configuration:

```nix
{
  imports = [ inputs.sigen-server.nixosModules.default ];

  services.sigen-server = {
    enable = true;
    address = "192.168.1.50:502";
    unitId = 1;
    bind = "127.0.0.1:3000";
    environment = {
      SIGEN_POLL_SECONDS = "15";
      RUST_LOG = "info";
    };
    # Optional: provision this file separately, outside the Nix store.
    environmentFile = "/run/secrets/sigen-server.env";
    # For an authenticated reverse proxy, allow its Host authority:
    # extraArgs = [ "--allowed-host" "sigen.example" ];
  };
}
```

Pass `inputs` to your host modules using `specialArgs = { inherit inputs; };`
in `nixpkgs.lib.nixosSystem`. The package can be overridden with
`services.sigen-server.package`.

The `sigen-server.service` unit starts at boot, restarts on failure, and runs as
a dynamic unprivileged user. Its persistent event journal, alert settings and write audit log
live in `/var/lib/sigen-server`. It listens on localhost by default and does
not open firewall ports. For remote access, use an authenticated HTTPS reverse
proxy or a trusted private network; the server has no user authentication.

`environment` accepts any server environment option. Use `environmentFile` for
credentials such as `SIGEN_MQTT_PASSWORD` and `SIGEN_PUSHOVER_TOKEN`; values
written directly in Nix configuration are stored in the world-readable Nix
store. The environment file uses systemd `KEY=value` syntax and should be
readable only by root. Its settings override `environment`; `extraArgs` override
environment settings.

After rebuilding NixOS, inspect the service with:

```sh
systemctl status sigen-server
journalctl -u sigen-server -f
```
