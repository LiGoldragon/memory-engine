{
  description = "The durable memory engine.";
  inputs.base.url = "github:LiGoldragon/build-base";
  outputs = { self, base }: base.lib.workspaceOutputs { cargoNix = ./Cargo.nix; };
}
