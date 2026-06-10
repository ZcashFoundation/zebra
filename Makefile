.PHONY: help

include make/zcashd-compat.mk

help:
	@echo "Available targets:"
	@echo "  compat-docker-build              Build Docker zcashd-compat image"
	@echo "  compat-zcashd-prepare            Fetch/verify zcashd-compat artifact for Docker build"
	@echo "  compat-docker-start              Start Docker zcashd-compat with mounted snapshots"
	@echo "  compat-zebrad-start-supervised   Start zebrad with zcashd supervision enabled"
	@echo "  compat-zebrad-start-unsupervised Start zebrad with zcashd supervision disabled"
	@echo "  compat-zcashd-start-standalone   Start zcashd -zebra-compat as a standalone process"
	@echo "  compat-zebrad-status             Check zebrad liveness and Zebra RPC health"
	@echo "  compat-zcashd-status             Check zcashd liveness and zebra-compat RPC health"
	@echo "  compat-status-sync               Run both status checks and enforce max drift"
	@echo "  compat-test-regtest              Run full zcashd-compat test suite (regtest, spawns processes)"
	@echo "  compat-test-mainnet              Run read-only zcashd-compat tests against live mainnet"
	@echo "  compat-test-testnet              Run read-only zcashd-compat tests against live testnet"
