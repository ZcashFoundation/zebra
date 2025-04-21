variable "cidr" {
  description = "The CIDR block for the VPC."
}

variable "external_subnets" {
  description = "List of external subnets"
  type        = list(string)
}

variable "internal_subnets" {
  description = "List of internal subnets"
  type        = list(string)
}

variable "environment" {
  description = "Environment tag, e.g prod"
}

variable "availability_zones" {
  description = "List of availability zones"
  type        = list(string)
}

variable "default_sg_allow_all_self" {
  description = "Security group allow all traffic to itself"
  type = bool
  default = false
}
