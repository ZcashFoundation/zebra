locals {
  # Automatically load environment-level variables
  environment_vars = read_terragrunt_config(find_in_parent_folders("env.hcl"))

  region_vars  = read_terragrunt_config(find_in_parent_folders("region.hcl"))
  account_vars = read_terragrunt_config(find_in_parent_folders("account.hcl"))
  # Extract out common variables for reuse
  env = local.environment_vars.locals.environment
}

# Terragrunt will copy the Terraform configurations specified by the source parameter, along with any files in the
# working directory, into a temporary folder, and execute your Terraform commands in that folder.
terraform {
  source = "../../../../../terraform-aws-modules/ecr-public"
}

# Include all settings from the root terragrunt.hcl file
include {
  path = find_in_parent_folders()
}

inputs = {
  environment      = local.env
  name             = "zebra-server"
  description      = "QEDIT Zebra Server is a Zcash node for the ZSA testnet"
  usage_text       = "Run the Docker image with the Zebra configuration for the ZSA testnet"
  about_text       = "QEDIT Zebra Server"
  architecture     = "ARM"
  operating_system = "Linux"
  aws_profile      = local.account_vars.locals.aws_profile
  aws_account_id   = local.account_vars.locals.aws_account_id
}
