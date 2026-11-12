{
  description = "ytmtui — a full-featured YouTube Music TUI client";

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";

  outputs = { self, nixpkgs }:
    let
      systems = [ "x86_64-linux" "aarch64-linux" ];
      forAllSystems = nixpkgs.lib.genAttrs systems;
    in
    {
      packages = forAllSystems (system:
        let
          pkgs = nixpkgs.legacyPackages.${system};
        in
        {
          default = pkgs.callPackage ./nix/package.nix { };
          ytmtui = self.packages.${system}.default;
        });

      apps = forAllSystems (system: {
        default = {
          type = "app";
          program = "${self.packages.${system}.default}/bin/ytmtui";
        };
      });

      overlays.default = final: prev: {
        ytmtui = final.callPackage ./nix/package.nix { };
      };

      nixosModules.default = { pkgs, ... }: {
        environment.systemPackages = [
          self.packages.${pkgs.stdenv.hostPlatform.system}.default
        ];
      };

      homeManagerModules.default = { pkgs, ... }: {
        home.packages = [
          self.packages.${pkgs.stdenv.hostPlatform.system}.default
        ];
      };
    };
}
