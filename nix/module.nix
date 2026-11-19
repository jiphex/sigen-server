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

    energy = {
      octopusAccount = lib.mkOption {
        type = lib.types.nullOr lib.types.str;
        default = null;
        example = "A-12345678";
        description = "Octopus account number for automatic electricity meter and tariff discovery. Requires octopusApiKeyFile; no Home Assistant settings are needed.";
      };
      octopusApiKeyFile = lib.mkOption {
        type = lib.types.nullOr lib.types.str;
        default = null;
        example = "/run/secrets/octopus-api-key";
        description = "Runtime path to a file containing only the Octopus API key. Loaded as a systemd credential for the dynamic service user. Keep the file outside the Nix store.";
      };
      octopusImportTariff = lib.mkOption {
        type = lib.types.nullOr lib.types.str;
        default = null;
        example = "E-1R-AGILE-24-10-01-J";
        description = "Public import tariff code, as an alternative to authenticated account discovery or Home Assistant.";
      };
      octopusExportTariff = lib.mkOption {
        type = lib.types.nullOr lib.types.str;
        default = null;
        description = "Optional public export tariff code, without account discovery or Home Assistant.";
      };
      homeAssistantUrl = lib.mkOption {
        type = lib.types.nullOr lib.types.str;
        default = null;
        example = "https://homeassistant.example.ts.net";
        description = "Home Assistant HTTPS origin, as an alternative to direct Octopus access. Requires homeAssistantTokenFile.";
      };
      homeAssistantTokenFile = lib.mkOption {
        type = lib.types.nullOr lib.types.str;
        default = null;
        example = "/run/secrets/home-assistant-token";
        description = "Runtime path to a file containing only the HA token. Loaded as a systemd credential for the dynamic service user. Keep the file outside the Nix store.";
      };
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
      }
      // lib.optionalAttrs (cfg.energy.octopusAccount != null) { SIGEN_OCTOPUS_ACCOUNT = cfg.energy.octopusAccount; }
      // lib.optionalAttrs (cfg.energy.octopusApiKeyFile != null) { SIGEN_OCTOPUS_API_KEY_FILE = "%d/octopus-api-key"; }
      // lib.optionalAttrs (cfg.energy.octopusImportTariff != null) { SIGEN_OCTOPUS_IMPORT_TARIFF = cfg.energy.octopusImportTariff; }
      // lib.optionalAttrs (cfg.energy.octopusExportTariff != null) { SIGEN_OCTOPUS_EXPORT_TARIFF = cfg.energy.octopusExportTariff; }
      // lib.optionalAttrs (cfg.energy.homeAssistantUrl != null) { SIGEN_HA_URL = cfg.energy.homeAssistantUrl; }
      // lib.optionalAttrs (cfg.energy.homeAssistantTokenFile != null) { SIGEN_HA_TOKEN_FILE = "%d/home-assistant-token"; }
      // cfg.environment;
      serviceConfig = {
        Type = "notify";
        NotifyAccess = "main";
        WatchdogSec = "30s";
        ExecStart = lib.escapeShellArgs ([ (lib.getExe cfg.package) ] ++ cfg.extraArgs);
        EnvironmentFile = lib.mkIf (cfg.environmentFile != null) cfg.environmentFile;
        LoadCredential =
          lib.optional (cfg.energy.octopusApiKeyFile != null) "octopus-api-key:${cfg.energy.octopusApiKeyFile}"
          ++ lib.optional (cfg.energy.homeAssistantTokenFile != null) "home-assistant-token:${cfg.energy.homeAssistantTokenFile}";
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
