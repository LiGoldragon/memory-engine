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
          actualGraph = builtins.genericClosure {
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
          graph = actualGraph ++ [
            (let original = builtins.head (builtins.filter
              (node: node.crate.crateName == "syn"
                && builtins.head (pkgs.lib.splitVersion node.crate.version) == "2")
              actualGraph);
             in { key = original.key + "-mutation"; crate = original.crate // { version = "4.0.0"; }; })
          ];
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
          # Cargo compatibility: major for 1+, minor for 0.x, patch for 0.0.x.
          family =
            crate:
            let
              version = pkgs.lib.splitVersion crate.version;
              major = builtins.elemAt version 0;
              minor = builtins.elemAt version 1;
              patch = builtins.elemAt version 2;
              compatible =
                if major != "0" then
                  major
                else if minor != "0" then
                  "0.${minor}"
                else
                  "0.0.${patch}";
            in
            "${crate.crateName}@${compatible}";
          families = builtins.groupBy (node: family node.crate) graph;
          oneFamily = builtins.all (
            entries:
            builtins.length (
              pkgs.lib.unique (
                map (node: {
                  derivation = node.crate.drvPath;
                  source = toString node.crate.src;
                }) entries
              )
            ) == 1
          ) (builtins.attrValues families);
          approvedSynMajors = [
            "2"
            "3"
          ];
          approvedSyn = builtins.all (
            node:
            node.crate.crateName != "syn"
            || builtins.elem (builtins.head (pkgs.lib.splitVersion node.crate.version)) approvedSynMajors
          ) graph;
          dependencies = map (node: {
            name = node.crate.crateName;
            version = node.crate.version;
            family = family node.crate;
            derivation = node.crate.drvPath;
            source = toString node.crate.src;
          }) graph;
        in
        {
          dependency-boundary =
            assert pkgs.lib.assertMsg (builtins.all (name: !(builtins.elem name names)) forbidden)
              "Retired envelopes, NOTA, and source-generation crates must stay outside the normal Memory engine graph";
            assert pkgs.lib.assertMsg oneFamily
              "The Memory engine normal graph contains distinct derivations or sources in one semver-compatible family";
            assert pkgs.lib.assertMsg approvedSyn
              "The Memory engine normal graph contains an unapproved syn major (approved: 2, 3)";
            pkgs.runCommand "memory-engine-dependency-boundary" { } ''
              test ! -d ${library}/bin
              mkdir -p $out
              cp ${pkgs.writeText "normal-dependencies.json" (builtins.toJSON dependencies)} $out/normal-dependencies.json
            '';
        };
    };
}
