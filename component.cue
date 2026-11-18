id: "jj-hooks"
release: {
	base: "0.7.0"
	artifacts: {
		"jj-hooks-bin": {
			kind:     "nix"
			attr:     "jj-hooks-static"
			binaries: ["jj-hooks", "jj-hp"]
		}
	}
}
