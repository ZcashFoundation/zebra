{
  configFile,
  lib,
  package,
}: {
  Program = lib.getExe package;
  ProgramArguments = ["--config" (toString configFile) "start"];
  ProcessType = "Background";
  KeepAlive = true;
  RunAtLoad = true;
}
