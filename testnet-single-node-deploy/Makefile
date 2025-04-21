# This file is a Makefile for the testnet-single-node-deploy project.
# It is used to plan, apply, and destroy changes to the infrastructure using Terragrunt.
# Define variables
ACCOUNT_DIR := dev
REGION := eu-central-1
ENVIRONMENT := zsa
AWS_PROFILE := qed-it
BASE_DIR := ./zebra/testnet-single-node-deploy/infra/terragrunt-aws-environments

# Check prerequisites
.PHONY: check-prerequisites plan-all apply-all destroy-all

check-prerequisites:
	@echo "Checking prerequisites..."
	@if ! command -v aws >/dev/null 2>&1; then \
		echo "Error: AWS CLI is not installed."; \
		exit 1; \
	fi
	@if ! command -v terragrunt >/dev/null 2>&1; then \
		echo "Error: Terragrunt is not installed."; \
		exit 1; \
	fi
	@if ! grep -q "\[$(AWS_PROFILE)\]" ~/.aws/credentials; then \
		echo "Error: AWS profile '$(AWS_PROFILE)' does not exist."; \
		exit 1; \
	fi
	@echo "All prerequisites are met."

# Plan changes for all modules in an environment
plan-all: check-prerequisites
	@echo "Planning changes for all modules in $(BASE_DIR)/$(ACCOUNT_DIR)/$(REGION)/$(ENVIRONMENT)..."
	cd $(BASE_DIR)/$(ACCOUNT_DIR)/$(REGION)/$(ENVIRONMENT) && \
	terragrunt plan-all

# Apply changes for all modules in an environment
apply-all: check-prerequisites
	@echo "Applying changes for all modules in $(BASE_DIR)/$(ACCOUNT_DIR)/$(REGION)/$(ENVIRONMENT)..."
	cd $(BASE_DIR)/$(ACCOUNT_DIR)/$(REGION)/$(ENVIRONMENT) && \
	terragrunt apply-all

# Destroy all resources in an environment
destroy-all: check-prerequisites
	@echo "Destroying all resources in $(BASE_DIR)/$(ACCOUNT_DIR)/$(REGION)/$(ENVIRONMENT)..."
	cd $(BASE_DIR)/$(ACCOUNT_DIR)/$(REGION)/$(ENVIRONMENT) && \
	terragrunt destroy-all

