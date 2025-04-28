variable "env" {
  description = "The name of the environment"
  type        = string
}

variable "user_name" {
  description = "The name of the IAM user"
  type        = string
}

variable "aws_region" {
  description = "The AWS region to deploy the resources to"
  type        = string
}

variable "aws_account_id" {
  description = "The AWS account ID to deploy the resources to"
  type        = string
}
