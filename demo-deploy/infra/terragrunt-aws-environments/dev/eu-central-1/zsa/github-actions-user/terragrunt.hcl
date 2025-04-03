locals {
  # Automatically load environment-level variables
  environment_vars = read_terragrunt_config(find_in_parent_folders("env.hcl"))

  region_vars = read_terragrunt_config(find_in_parent_folders("region.hcl"))
  account_vars = read_terragrunt_config(find_in_parent_folders("account.hcl"))
  # Extract out common variables for reuse
  env = local.environment_vars.locals.environment
}

# Terragrunt will copy the Terraform configurations specified by the source parameter, along with any files in the
# working directory, into a temporary folder, and execute your Terraform commands in that folder.
terraform {
  source = "../../../../../terraform-aws-modules//cicd-user"
}

# Include all settings from the root terragrunt.hcl file
  include {
path = find_in_parent_folders()
}

inputs = {
  env = local.env
  aws_region = local.region_vars.locals.aws_region
  aws_account_id = local.account_vars.locals.aws_account_id
  user_name = "zebra-github-actions-user"
}
