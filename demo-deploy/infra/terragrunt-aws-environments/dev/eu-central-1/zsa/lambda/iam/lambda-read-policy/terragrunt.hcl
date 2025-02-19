locals {
  # Automatically load environment-level variables
  environment_vars = read_terragrunt_config(find_in_parent_folders("env.hcl"))
  region_vars      = read_terragrunt_config(find_in_parent_folders("region.hcl"))
  account_vars     = read_terragrunt_config(find_in_parent_folders("account.hcl"))

  # Extract out common variables for reuse
  env = local.environment_vars.locals.environment
}

terraform {
  source = "github.com/terraform-aws-modules/terraform-aws-iam//modules/iam-policy?ref=v5.52.2"
}

include {
  path = find_in_parent_folders()
}

inputs = {
  create_policy = true

  name   = "lambda-zebra-read-logs"
  policy = jsonencode({
    Version = "2012-10-17"
    Statement = [
      {
        Effect   = "Allow"
        Action   = [
          "logs:DescribeLogStreams",
          "logs:GetLogEvents",
          "logs:PutLogEvents",
          "logs:CreateLogStream",
          "logs:CreateLogGroup"
        ]
        Resource = "arn:aws:logs:eu-central-1:496038263219:log-group:/dev/ecs/zebra-task:*"
      }
    ]
  })

  tags = {
    Environment = local.env
    Project     = "zebra-logs"
  }
}