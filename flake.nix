{
	inputs = {
		nixCargoIntegration.url = "github:yusdacra/nix-cargo-integration";
	};
	outputs = inputs: inputs.nixCargoIntegration.lib.makeOutputs {
		root = ./.;
		overrides = {
			shell = common: prev: with common.pkgs; {
				# hardeningDisable = [ "fortify" ];
				env = prev.env ++ [
					{
						# Vulkan doesn't work without this for some reason :(
						name = "LD_LIBRARY_PATH";
						eval = "$LD_LIBRARY_PATH${pkgs.vulkan-loader}/lib";
					}
					{
						name = "NIX_CFLAGS_LINK";
						eval = "-fuse-ld=lld";
					}
				];
			};
		};
	};
}