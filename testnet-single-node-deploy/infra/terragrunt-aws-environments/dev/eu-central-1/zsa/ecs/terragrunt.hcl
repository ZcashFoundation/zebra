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
  source = "../../../../../terraform-aws-modules/ecs"
}

# Include all settings from the root terragrunt.hcl file
include {
  path = find_in_parent_folders()
}

dependency "vpc" {
  config_path = "../vpc"
}

dependency "ecr" {
  config_path = "../ecr"
}

inputs = {
  name = "zebra"
  environment = local.env
  region = local.region_vars.locals.aws_region
  account_id = local.account_vars.locals.aws_account_id

  vpc_id = dependency.vpc.outputs.vpc_id
  private_subnets = dependency.vpc.outputs.private_subnets
  public_subnets = dependency.vpc.outputs.public_subnets
  
  image = "${dependency.ecr.outputs.ecr-url}:latest"

  task_memory=4096
  task_cpu=1024

  enable_logging = true
  enable_backup = false

  enable_domain = true
  domain = "zebra.zsa-test.net"
  zone_name = "zsa-test.net"

  persistent_volume_size = 40

  port_mappings = [
    {
      containerPort = 80
      hostPort      = 80
      protocol      = "tcp"
    },
    {
      containerPort = 443
      hostPort      = 443
      protocol      = "tcp"
    },
    // Zebra ports are:
    // TCP ports:
    { // RPC PubSub
      containerPort = 18232
      hostPort      = 18232
      protocol      = "tcp"
    }
  ]
}
