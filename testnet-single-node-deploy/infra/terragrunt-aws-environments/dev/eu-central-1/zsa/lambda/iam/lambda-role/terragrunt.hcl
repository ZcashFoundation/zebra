locals {
  # Automatically load environment-level variables
  environment_vars = read_terragrunt_config(find_in_parent_folders("env.hcl"))
  region_vars      = read_terragrunt_config(find_in_parent_folders("region.hcl"))
  account_vars     = read_terragrunt_config(find_in_parent_folders("account.hcl"))

  # Extract out common variables for reuse
  env = local.environment_vars.locals.environment
}

terraform {
  source = "github.com/terraform-aws-modules/terraform-aws-iam//modules/iam-assumable-role?ref=v5.52.2"
}

include {
  path = find_in_parent_folders()
}

dependency "lambda-ecr-policy" {
  config_path = "../lambda-ecr-policy"
}

dependency "lambda-read-policy" {
  config_path = "../lambda-read-policy"
}

inputs = {
  create_role           = true
  role_name            = "lambda-zebra-logs-role"
  trusted_role_services = ["lambda.amazonaws.com"]
  max_session_duration = 3600
  description          = "Role for accessing Zebra logs for the lambda function"

  role_requires_mfa = false
  custom_role_policy_arns = [
    "arn:aws:iam::aws:policy/service-role/AWSLambdaBasicExecutionRole",
    dependency.lambda-ecr-policy.outputs.arn,
    dependency.lambda-read-policy.outputs.arn,
  ]

  tags = {
    Environment = local.env
    Project     = "zebra-logs"
    Terraform   = "true"
  }
}
