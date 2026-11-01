flake:
{ config, lib, pkgs, ... }:
let
  cfg = config.services.sigen-server;
in
{
  options.services.sigen-server = {
    enable = lib.mkEnableOption "the Sigenergy dashboard and Prometheus exporter";

    package = lib.mkOption {
      type = lib.types.package;
      default = flake.packages.${pkgs.stdenv.hostPlatform.system}.sigen-server;
      defaultText = lib.literalExpression "sigen-server.packages.\${pkgs.stdenv.hostPlatform.system}.sigen-server";
      description = "Package providing the sigen-server executable.";
    };

    address = lib.mkOption {
      type = lib.types.str;
      example = "192.168.1.50:502";
      description = "Modbus TCP address of the inverter (IP:port).";
    };

    unitId = lib.mkOption {
      type = lib.types.ints.between 1 246;
      default = 1;
      description = "Inverter unit ID to monitor.";
    };

    bind = lib.mkOption {
      type = lib.types.str;
      default = "127.0.0.1:3000";
      description = "HTTP listening address. Use an authenticated reverse proxy for remote access.";
    };

    environment = lib.mkOption {
      type = lib.types.attrsOf lib.types.str;
      default = { };
      example = { SIGEN_POLL_SECONDS = "15"; RUST_LOG = "info"; };
      description = "Additional environment variables. Do not put secrets here: these values enter the Nix store.";
    };

    environmentFile = lib.mkOption {
      type = lib.types.nullOr lib.types.str;
      default = null;
      example = "/run/secrets/sigen-server.env";
      description = "Runtime path to a systemd environment file for credentials and other settings. Keep this file outside the Nix store.";
    };

    extraArgs = lib.mkOption {
      type = lib.types.listOf lib.types.str;
      default = [ ];
      example = [ "--allowed-host" "sigen.example" ];
      description = "Additional command-line arguments, which override environment settings.";
    };
  };

  config = lib.mkIf cfg.enable {
    systemd.services.sigen-server = {
      description = "Sigenergy dashboard and Prometheus exporter";
      wantedBy = [ "multi-user.target" ];
      wants = [ "network-online.target" ];
      after = [ "network-online.target" ];
      environment = {
        SIGEN_ADDRESS = cfg.address;
        SIGEN_UNIT_ID = toString cfg.unitId;
        SIGEN_BIND = cfg.bind;
        SIGEN_ALERTS_FILE = "/var/lib/sigen-server/alerts.json";
        SIGEN_WRITE_AUDIT_FILE = "/var/lib/sigen-server/writes.jsonl";
        SIGEN_JOURNAL_FILE = "/var/lib/sigen-server/events.sqlite3";
        SSL_CERT_FILE = "${pkgs.cacert}/etc/ssl/certs/ca-bundle.crt";
      } // cfg.environment;
      serviceConfig = {
        ExecStart = lib.escapeShellArgs ([ (lib.getExe cfg.package) ] ++ cfg.extraArgs);
        EnvironmentFile = lib.mkIf (cfg.environmentFile != null) cfg.environmentFile;
        DynamicUser = true;
        StateDirectory = "sigen-server";
        StateDirectoryMode = "0700";
        WorkingDirectory = "/var/lib/sigen-server";
        Restart = "on-failure";
        RestartSec = "5s";
        UMask = "0077";
        NoNewPrivileges = true;
        PrivateTmp = true;
        ProtectSystem = "strict";
        ProtectHome = true;
        ProtectKernelTunables = true;
        ProtectKernelModules = true;
        ProtectControlGroups = true;
        RestrictSUIDSGID = true;
        RestrictAddressFamilies = [ "AF_UNIX" "AF_INET" "AF_INET6" ];
        CapabilityBoundingSet = "";
      };
    };
  };
}
