# NixOS module for silph. Takes the flake's `self` so package options can
# default to the flake's own package.
self:
{
  config,
  lib,
  pkgs,
  ...
}:
let
  collectorCfg = config.services.silph.collector;
  serverCfg = config.services.silph.server;

  settingsFormat = pkgs.formats.toml { };

  packageOption = lib.mkPackageOption self.packages.${pkgs.stdenv.hostPlatform.system} "silph" {
    pkgsText = "silph.packages.\${system}";
  };

  mkSettingsOption =
    {
      name,
      example,
      extraOptions ? { },
    }:
    lib.mkOption {
      type = lib.types.submodule {
        freeformType = settingsFormat.type;
        options = extraOptions;
      };
      default = { };
      example = lib.literalExpression example;
      description = ''
        Configuration for the silph ${name}, rendered verbatim to TOML;
        see `examples/${name}.toml` in the silph repository for the
        available keys. Omitted keys use the ${name}'s defaults; unknown
        keys are rejected at service start.

        Any string value may instead be `{ _secret = "/path"; }` to
        load it from a file at service start, keeping it out of the
        Nix store.
      '';
    };

  # Any string value in `settings` may instead be { _secret = "/path"; }.
  # Such values are replaced by a placeholder in the store config and
  # substituted with the file's content at service start, via systemd
  # credentials, so the secret never enters the Nix store.
  isSecret = v: lib.isAttrs v && v ? _secret;

  collectSecrets =
    v:
    if isSecret v then
      [ v._secret ]
    else if lib.isAttrs v then
      lib.concatMap collectSecrets (lib.attrValues v)
    else if lib.isList v then
      lib.concatMap collectSecrets v
    else
      [ ];

  hideSecrets =
    v:
    if isSecret v then
      "@_secret:${v._secret}@"
    else if lib.isAttrs v then
      lib.mapAttrs (_: hideSecrets) v
    else if lib.isList v then
      map hideSecrets v
    else
      v;

  hardening = {
    DynamicUser = true;
    NoNewPrivileges = true;
    PrivateTmp = true;
    PrivateDevices = true;
    ProtectSystem = "strict";
    ProtectHome = true;
    ProtectClock = true;
    ProtectControlGroups = true;
    ProtectHostname = true;
    ProtectKernelLogs = true;
    ProtectKernelModules = true;
    ProtectKernelTunables = true;
    RestrictAddressFamilies = [
      "AF_INET"
      "AF_INET6"
    ];
    RestrictNamespaces = true;
    RestrictRealtime = true;
    RestrictSUIDSGID = true;
    LockPersonality = true;
    MemoryDenyWriteExecute = true;
    CapabilityBoundingSet = [ "" ];
    SystemCallArchitectures = "native";
    SystemCallFilter = [
      "@system-service"
      "~@privileged"
    ];
    UMask = "0077";
  };

  mkSilphService =
    {
      name,
      description,
      cfg,
      extraServiceConfig,
    }:
    let
      # Also the binary name: the package ships silph-collector/silph-server.
      fullName = "silph-${name}";
      secrets = lib.unique (collectSecrets cfg.settings);
      hasSecrets = secrets != [ ];
      credentials = lib.imap0 (i: path: {
        name = "secret-${toString i}";
        inherit path;
      }) secrets;
      configFile = settingsFormat.generate "${fullName}.toml" (hideSecrets cfg.settings);
      configPath = if hasSecrets then "/run/${fullName}/config.toml" else configFile;
    in
    {
      inherit description;
      wantedBy = [ "multi-user.target" ];
      after = [ "network.target" ];

      preStart = lib.mkIf hasSecrets (
        ''
          install -m 600 ${configFile} "$RUNTIME_DIRECTORY/config.toml"
        ''
        + lib.concatStrings (
          map (cred: ''
            ${pkgs.replace-secret}/bin/replace-secret ${lib.escapeShellArg "@_secret:${cred.path}@"} "$CREDENTIALS_DIRECTORY/${cred.name}" "$RUNTIME_DIRECTORY/config.toml"
          '') credentials
        )
      );

      serviceConfig =
        hardening
        // {
          ExecStart = "${lib.getExe' cfg.package fullName} --config ${configPath}";
          Restart = "on-failure";
        }
        // extraServiceConfig
        // lib.optionalAttrs hasSecrets {
          RuntimeDirectory = fullName;
          LoadCredential = map (cred: "${cred.name}:${cred.path}") credentials;
        };
    };

  defaultDataDir = "/var/lib/silph";
  serverUsesStateDirectory = serverCfg.settings.data_dir == defaultDataDir;
in
{
  options.services.silph = {
    collector = {
      enable = lib.mkEnableOption "the silph metrics collector";

      package = packageOption;

      settings = mkSettingsOption {
        name = "collector";
        example = ''
          {
            listen = "0.0.0.0:9100";
            token._secret = "/run/secrets/silph-token";
            disk.mounts = [ "/" "/home" ];
          }
        '';
      };
    };

    server = {
      enable = lib.mkEnableOption "the silph monitoring server";

      package = packageOption;

      settings = mkSettingsOption {
        name = "server";
        example = ''
          {
            listen = "127.0.0.1:8080";
            scrape_interval = "15s";
            targets = [
              {
                name = "web-1";
                url = "http://10.0.0.2:9100";
                token._secret = "/run/secrets/silph-token";
              }
            ];
          }
        '';
        extraOptions.data_dir = lib.mkOption {
          type = lib.types.str;
          default = defaultDataDir;
          description = ''
            Directory for the embedded time-series database. The default
            is managed as a systemd state directory; if you point this
            elsewhere, the directory must already exist and be writable
            by the service.
          '';
        };
      };
    };
  };

  config = lib.mkMerge [
    (lib.mkIf collectorCfg.enable {
      systemd.services.silph-collector = mkSilphService {
        name = "collector";
        description = "silph metrics collector";
        cfg = collectorCfg;
        extraServiceConfig = {
          # The collector reads /proc/stat, /proc/meminfo and /proc/mounts
          # and calls statvfs() on real mounts, so /proc must stay fully
          # visible and /home must not be masked with an empty tmpfs.
          ProtectHome = "read-only";
        };
      };
    })

    (lib.mkIf serverCfg.enable {
      systemd.services.silph-server = mkSilphService {
        name = "server";
        description = "silph monitoring server";
        cfg = serverCfg;
        extraServiceConfig = {
          # AF_UNIX for glibc DNS resolution (nscd socket).
          RestrictAddressFamilies = [
            "AF_UNIX"
            "AF_INET"
            "AF_INET6"
          ];
        }
        // lib.optionalAttrs serverUsesStateDirectory {
          StateDirectory = "silph";
          StateDirectoryMode = "0700";
        }
        // lib.optionalAttrs (!serverUsesStateDirectory) {
          ReadWritePaths = [ serverCfg.settings.data_dir ];
        };
      };
    })
  ];
}
