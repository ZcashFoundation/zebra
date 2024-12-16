# Set common variables for the environment. This is automatically pulled in in the root terragrunt.hcl configuration to
# feed forward to the child modules.
locals {
  environment        = "dev"
  cidr = "10.12.0.0/16"
  internal_subnets = [
    "10.12.0.0/19",
    "10.12.64.0/19"]
  external_subnets = [
    "10.12.32.0/19",
    "10.12.96.0/19"]
  availability_zones = [
    "eu-central-1a",
    "eu-central-1b"]
}
