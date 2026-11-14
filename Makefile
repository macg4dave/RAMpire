.PHONY: help local-gate clean

help:
	@echo "Repository validation delegates"
	@echo "  make local-gate    - tools/build.sh (the gate, in the build container)"
	@echo "  make clean         - remove Cargo build output"

local-gate:
	bash tools/build.sh

clean:
	cargo clean
