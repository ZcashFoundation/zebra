variable "environment" {
  default = "dev"
}

variable "name" {
  type     = string
  nullable = false
}

variable "description" {
  type     = string
  nullable = false
}

variable "usage_text" {
  type     = string
  nullable = false
}

variable "about_text" {
  type     = string
  nullable = false
}

variable "architecture" {
  type     = string
  nullable = false
}

variable "operating_system" {
  type     = string
  nullable = false
}

variable "aws_account_id" {
  type     = string
  nullable = false
}
