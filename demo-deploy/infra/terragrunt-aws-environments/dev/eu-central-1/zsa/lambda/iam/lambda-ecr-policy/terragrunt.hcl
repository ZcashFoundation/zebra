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

  name   = "zebra-lambda-ecr-access"
  policy = jsonencode({
    Version = "2012-10-17"
    Statement = [
      {
        Effect = "Allow"
        Action = [
          "ecr:GetDownloadUrlForLayer",
          "ecr:BatchGetImage"
        ]
        Resource = "arn:aws:ecr:${local.region_vars.locals.aws_region}:${local.account_vars.locals.aws_account_id}:repository/*"
      }
    ]
  })

  tags = {
    Environment = local.env
    Project     = "zebra-logs"
  }
}
