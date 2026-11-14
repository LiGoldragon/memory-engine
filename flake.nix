{
  description = "The durable memory engine.";

  inputs.base.url = "github:LiGoldragon/build-base";

  outputs =
    { self, base }:
    base.lib.workspaceOutputs {
      cargoNix = ./Cargo.nix;
      extraChecks =
        { pkgs, binaries, ... }:
        let
          library = binaries "memory-engine" [ "default" ];
          graph = builtins.genericClosure {
            startSet = [
              {
                key = library.drvPath;
                crate = library;
              }
            ];
            operator =
              node:
              map (crate: {
                key = crate.drvPath;
                inherit crate;
              }) node.crate.dependencies;
          };
          names = map (node: node.crate.crateName) graph;
          # These were cargo-tree assertions in dependency_boundary.rs.
          # The effective normal build graph is the object being checked;
          # the sandbox runs no package resolver and compiles nothing.
          forbidden = [
            "signal-frame"
            "nota-next"
            "nota_next"
            "schema-rust"
            "sema-translator"
            "core-ethos"
            "core-nomos"
          ];
          oneFamily = builtins.length names == builtins.length (pkgs.lib.unique names);
        in
        {
          dependency-boundary =
            assert pkgs.lib.assertMsg (builtins.all (name: !(builtins.elem name names)) forbidden)
              "Retired envelopes, NOTA, and source-generation crates must stay outside the normal Memory engine graph";
            assert pkgs.lib.assertMsg oneFamily
              "The Memory engine normal graph contains duplicate package families";
            pkgs.runCommand "memory-engine-dependency-boundary" { } ''
              test ! -d ${library}/bin
              mkdir -p $out
              cp ${pkgs.writeText "normal-dependencies.json" (builtins.toJSON names)} $out/normal-dependencies.json
            '';
        };
    };
}
