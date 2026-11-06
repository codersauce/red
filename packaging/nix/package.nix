{
  lib,
  source,
  rustPlatform,
  makeWrapper,
  ripgrep,
  gitMinimal,
}:

rustPlatform.buildRustPackage (finalAttrs: {
  pname = "red-editor";
  version = (builtins.fromTOML (builtins.readFile "${source}/Cargo.toml")).package.version;
  src = source;

  cargoLock.lockFile = "${source}/Cargo.lock";
  cargoBuildFlags = [
    "-p"
    "red"
    "--bin"
    "red"
  ];
  cargoTestFlags = [
    "-p"
    "red"
    "--test"
    "self_check"
  ];

  nativeBuildInputs = [ makeWrapper ];
  nativeCheckInputs = [
    ripgrep
    gitMinimal
  ];

  postInstall = ''
    mv "$out/bin/red" "$out/bin/red-editor"
    wrapProgram "$out/bin/red-editor" \
      --suffix PATH : ${
        lib.makeBinPath [
          ripgrep
          gitMinimal
        ]
      }
  '';

  doInstallCheck = true;
  installCheckPhase = ''
    runHook preInstallCheck
    export HOME="$TMPDIR/red-home"
    mkdir -p "$HOME"
    cd "$HOME"
    "$out/bin/red-editor" --version | grep -Fx "red ${finalAttrs.version}"
    NO_COLOR=1 "$out/bin/red-editor" --self-check > self-check.log
    tail -n 1 self-check.log | grep -Fx "red self-check ok"
    runHook postInstallCheck
  '';

  meta = {
    description = "Modal terminal text editor with built-in Husk plugins";
    homepage = "https://getred.dev";
    license = lib.licenses.mit;
    mainProgram = "red-editor";
    platforms = [
      "x86_64-linux"
      "aarch64-linux"
      "aarch64-darwin"
    ];
  };
})
