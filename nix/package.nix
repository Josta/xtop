{ lib, rustPlatform, makeWrapper, coreutils }:

let
  manifest = (lib.importTOML ../Cargo.toml).package;
in
rustPlatform.buildRustPackage {
  pname = manifest.name;
  inherit (manifest) version;

  src = lib.fileset.toSource {
    root = ../.;
    fileset = lib.fileset.unions [
      ../Cargo.toml
      ../Cargo.lock
      ../LICENSE
      ../examples/config.toml
      ../src
    ];
  };
  cargoLock.lockFile = ../Cargo.lock;

  nativeBuildInputs = [ makeWrapper ];
  postInstall = ''
    # xtop uses timeout to bound optional queries to the user's tmux server.
    wrapProgram "$out/bin/xtop" --prefix PATH : ${lib.makeBinPath [ coreutils ]}
  '';

  meta = {
    inherit (manifest) description;
    license = lib.licenses.mit;
    mainProgram = "xtop";
    platforms = lib.platforms.linux;
  };
}
