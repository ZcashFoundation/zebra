# Terraform-AWS-Modules repo

This repo, along with the 'terragrunt-aws-projects', creates a solana-node in AWS ECS, which
you can use with [Terragrunt](https://github.com/gruntwork-io/terragrunt) to keep your
[Terraform](https://www.terraform.io) code DRY. For background information, check out the [Keep your Terraform code
DRY](https://github.com/gruntwork-io/terragrunt#keep-your-terraform-code-dry) section of the Terragrunt documentation.

# Deployment

The deployment process is similar to Terraform, you can use 'terragrunt apply' in any folder containing a 'terragrunt.hcl' file (in the '-environments' repo), where this repository defines the Terraform modules used. To destroy the module you can use 'terragrunt destroy'.
There are also more advanced options to 'apply'/'destroy' many modules at the same time.

# Optional variables

The ECS module can receive an 'enable_logging' (boolean) variable to enable logging of the node itself. This can be quite extensive and repetative so set to 'false' by default.
The ECS module can also receive an 'enable_domain' (boolean) variable, and if true it expects to receive a domain name ('qedit-solana.net') that will be linked to the Load-Balancer automatically. Those variable can be passed (also) like this when creating the ECS module.
terragrunt apply --auto-approve -var="enable_domain=true" -var="domain=qedit-solana.net"