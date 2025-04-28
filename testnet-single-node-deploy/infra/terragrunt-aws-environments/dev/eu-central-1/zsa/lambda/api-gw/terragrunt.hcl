locals {
  name = "watch-logs"
  domain_name = "zsa-test.net"

  # Automatically load environment-level variables
  environment_vars = read_terragrunt_config(find_in_parent_folders("env.hcl"))

  region_vars  = read_terragrunt_config(find_in_parent_folders("region.hcl"))
  account_vars = read_terragrunt_config(find_in_parent_folders("account.hcl"))
  
  # Extract out common variables for reuse
  env = local.environment_vars.locals.environment
}

terraform {
  source = "github.com/terraform-aws-modules/terraform-aws-apigateway-v2?ref=v5.2.1"
}

include {
  path = find_in_parent_folders()
}

dependency "lambda" {
  config_path = "../lambda"
}

inputs = {
  name        = local.name
  description = "Simple HTTP API Gateway forwarding to Lambda"


  # Create and configure the domain
  domain_name           = "logs.zebra.${local.domain_name}"
  create_domain_records = true
  create_certificate    = true
  hosted_zone_name = local.domain_name

  # Basic CORS configuration
  cors_configuration = {
    allow_headers = ["content-type", "x-amz-date", "authorization", "x-api-key", "x-amz-security-token", "x-amz-user-agent"]
    allow_methods = ["*"]
    allow_origins = ["*"]
  }

  # Simplified route configuration - forward everything to Lambda
  routes = {
    "ANY /" = {
      integration = {
        uri                    = dependency.lambda.outputs.lambda_function_arn
        payload_format_version = "2.0"
        integration_type       = "AWS_PROXY"
      }
    }
  }

  # Basic stage configuration
  stage_access_log_settings = {
    create_log_group            = true
    log_group_retention_in_days = 7
    format = jsonencode({
      requestId    = "$context.requestId"
      requestTime  = "$context.requestTime"
      status      = "$context.status"
      errorMessage = "$context.error.message"
      integration = {
        error     = "$context.integration.error"
        status    = "$context.integration.integrationStatus"
      }
    })
  }

  # Default stage settings
  stage_default_route_settings = {
    detailed_metrics_enabled = true
    throttling_burst_limit   = 100
    throttling_rate_limit    = 100
  }

  tags = {
    Environment = local.env
    Project     = "zebra-logs"
  }
}
