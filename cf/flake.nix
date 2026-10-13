{
  # Fault tests for the nerudo Worker on this machine, with pinned tools (cf/faults/README.md):
  #
  #   nix run .#faults              from cf/: the default cases, a few minutes
  #   nix run .#faults -- --help    the cases and their options
  #   nix develop                   a shell with the same tools
  #
  # Version 1 is not hermetic: it runs the harness and the Worker build from the working tree
  # (cf/nerudo-worker/build, cf/target/debug/nerudo), and builds them if they are missing.
  description = "nerudo fault tests: local Worker, CI stub, Gitea and toxiproxy";

  inputs.nixpkgs.url = "github:nixos/nixpkgs/nixpkgs-unstable";

  outputs = { self, nixpkgs }:
    let
      systems = [ "aarch64-darwin" "aarch64-linux" "x86_64-linux" ];
      each = f: nixpkgs.lib.genAttrs systems (system: f nixpkgs.legacyPackages.${system});
      # Forgejo is marked broken on Darwin in nixpkgs. Gitea has the same API for what
      # nerudo calls (repos, forks, tokens, smart HTTP), so it stands in for Forgejo here.
      tools = pkgs: [ pkgs.wrangler pkgs.gitea pkgs.toxiproxy pkgs.git pkgs.python3 pkgs.curl ];
    in
    {
      packages = each (pkgs: {
        faults = pkgs.writeShellApplication {
          name = "nerudo-faults";
          runtimeInputs = tools pkgs;
          text = ''
            top=$(git rev-parse --show-toplevel 2>/dev/null) || {
              echo "nerudo-faults: run it from the nerudo repository (cd cf; nix run .#faults)" >&2
              exit 2
            }
            exec python3 -I "$top/cf/faults/run.py" "$@"
          '';
        };
        default = self.packages.${pkgs.stdenv.hostPlatform.system}.faults;
      });

      apps = each (pkgs: {
        faults = { type = "app"; program = "${self.packages.${pkgs.stdenv.hostPlatform.system}.faults}/bin/nerudo-faults"; };
        default = self.apps.${pkgs.stdenv.hostPlatform.system}.faults;
      });

      devShells = each (pkgs: {
        default = pkgs.mkShell { packages = tools pkgs; };
      });
    };
}
