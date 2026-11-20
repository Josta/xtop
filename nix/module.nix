{ config, lib, pkgs, ... }:

{
  options.programs.xtop.enable = lib.mkEnableOption "the xtop process explorer";

  config = lib.mkIf config.programs.xtop.enable {
    environment.systemPackages = [ (pkgs.callPackage ./package.nix { }) ];
  };
}
