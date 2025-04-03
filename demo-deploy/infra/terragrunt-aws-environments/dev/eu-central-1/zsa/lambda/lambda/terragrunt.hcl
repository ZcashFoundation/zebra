locals {
  # Automatically load environment-level variables
  environment_vars = read_terragrunt_config(find_in_parent_folders("env.hcl"))

  region_vars  = read_terragrunt_config(find_in_parent_folders("region.hcl"))
  account_vars = read_terragrunt_config(find_in_parent_folders("account.hcl"))
  
  # Extract out common variables for reuse
  env = local.environment_vars.locals.environment
}

terraform {
  source = "github.com/terraform-aws-modules/terraform-aws-lambda?ref=v7.20.1"
}

include {
  path = find_in_parent_folders()
}

dependency "ecr" {
  config_path = "../ecr-logs"
}

dependency "lambda-role" {
  config_path = "../iam/lambda-role"
}


inputs = {
  function_name = "watch-zebra-logs"
  description   = "A simple lambda function to publicy expouse logs from the Zebra ECS task"
  memory_size   = 128
  timeout       = 300
  architectures = ["x86_64"]
  package_type  = "Image"
  image_uri     = "${dependency.ecr.outputs.ecr-url}:latest"
  create_role   = false
  lambda_role   = dependency.lambda-role.outputs.iam_role_arn
  ephemeral_storage_size = 512
  tracing_config_mode    = "PassThrough"

  # To create a version after the initial
  publish = true

  # Add allowed triggers for API Gateway
  allowed_triggers = {
    APIGatewayAny = {
      service    = "apigateway"
      source_arn = "arn:aws:execute-api:${local.region_vars.locals.aws_region}:${local.account_vars.locals.aws_account_id}:*/*/*"
    }
  }

  create_package = false
  create_lambda_function_url = true

  cloudwatch_log_group_name        = "/aws/lambda/zebra-logs"
  cloudwatch_log_retention_in_days = 14

  tags = {
    Environment = local.env
    Terraform     = "true"
  }
}
