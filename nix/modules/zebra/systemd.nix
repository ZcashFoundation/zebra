## TODO: This should instead read zebrad/systemd/zebrad.service, and modify the
##       few things that need to be.
{
  configFile,
  lib,
  package,
}: let
  programPath = lib.getExe package;
in {
  Unit.AssertPathExists = programPath;

  Service = {
    ExecStart = "${programPath} --config ${lib.escapeShellArg configFile} start";
    Restart = "always";
    PrivateTmp = true;
    NoNewPrivileges = true;
    StandardOutput = "journal";
    StandardError = "journal";
  };

  Install.WantedBy = ["default.target"];
}
