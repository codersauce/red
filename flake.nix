{
  description = "Red terminal editor";

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-26.05";

  outputs =
    { self, nixpkgs }:
    let
      systems = [
        "x86_64-linux"
        "aarch64-linux"
        "aarch64-darwin"
      ];
    in
    {
      packages = nixpkgs.lib.genAttrs systems (
        system:
        let
          red-editor = nixpkgs.legacyPackages.${system}.callPackage ./packaging/nix/package.nix {
            source = self;
          };
        in
        {
          inherit red-editor;
          default = red-editor;
        }
      );
    };
}
