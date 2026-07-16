.PHONY: help

# Keep `make` with no target printing help: the first rule in an included
# file would otherwise become the default goal.
.DEFAULT_GOAL := help

include make/zcashd-compat.mk

help:
	@echo "Available targets:"
	@echo ""
	@echo "  zcashd-compat:"
	@echo "  compat-docker-build              Build Docker zcashd-compat image"
	@echo "  compat-zcashd-prepare            Fetch/verify the zcashd-compat sidecar artifact (for tests)"
	@echo "  compat-docker-start              Start Docker zcashd-compat with mounted snapshots"
	@echo "  compat-zebrad-start-supervised   Start zebrad with zcashd supervision enabled"
	@echo "  compat-zebrad-start-unsupervised Start zebrad with zcashd supervision disabled"
	@echo "  compat-zcashd-start-standalone   Start sidecar zcashd as a standalone process"
	@echo "  compat-zebrad-status             Check zebrad liveness and RPC health"
	@echo "  compat-zcashd-status             Check zcashd liveness and RPC health"
	@echo "  compat-status-sync               Run both status checks and enforce max drift"
