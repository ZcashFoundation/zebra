## How do you deploy the infrastructure in this repo?

### Pre-requisites

1. Install [Terraform](https://www.terraform.io/) version `0.13.0` or newer and
   [Terragrunt](https://github.com/gruntwork-io/terragrunt) version `v0.25.1` or newer.
1. Fill in your AWS account variables in `dev/account.hcl`. (named 'qed-it')
1. Fill in your AWS region in `dev/<region>/region.hcl`. ('eu-central-1')

### Running first time in a region
On each region when running terragrunt for the first export your aws profile (this profile must exist in ~/.aws/credentials) so that the backend S3 bucket for the terraform state will be created, afterwards unset the aws profile, for example:
1. `cd dev/eu-central-1/rnd-1/vpc`
1. `export AWS_PROFILE=NAME`
1. `terragrunt init`
1. `unset AWS_PROFILE`

Afterwards terragrunt will use the profile that is in `dev/account.hcl`.

### Deploying a single module

1. `cd` into the module's folder (e.g. `cd dev/us-east-1/rnd-1/vpc`).
1. Run `terragrunt plan` to see the changes you're about to apply.
1. If the plan looks good, run `terragrunt apply`.

Deploy/destroy one resource of environment:

`cd <aws_account>/<region>/<env>/<resource>`

`terragrunt plan`

`terragrunt apply`

`terragrunt plan -destroy`

`terragrunt destroy`

### Deploying all modules in an environment

1. `cd` into the environment folder (e.g. `cd dev/us-east-1/rnd-1`).
1. Run `terragrunt plan-all` to see all the changes you're about to apply.
1. If the plan looks good, run `terragrunt apply-all`.

Deploy/destroy all resources of environment:

`cd <aws_account>/<region>/<env>`

`terragrunt plan-all`

`terragrunt apply-all`

`terragrunt plan-all -destroy`

`terragrunt destroy-all`

## How is the code in this repo organized?

The code in this repo uses the following folder hierarchy:

```
account
 └ _global
 └ region
    └ _global
    └ environment
       └ resource
```

## Creating and using root (project) level variables

In the situation where you have multiple AWS accounts or regions, you often have to pass common variables down to each
of your modules. Rather than copy/pasting the same variables into each `terragrunt.hcl` file, in every region, and in
every environment, you can inherit them from the `inputs` defined in the root `terragrunt.hcl` file.
