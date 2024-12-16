variable "environment" {
  default = "dev"
}

variable "region" {
  default = "eu-central-1"
}

variable "account_id" {
}

variable "name" {
}

variable "vpc_id" {
}

variable "private_subnets" {
  type = list(string)
}

variable "public_subnets" {
  type = list(string)
}

variable "image" {
}

variable "task_cpu" {
  default = 4096
  type = number
}

variable "task_memory" {
  default = 16384
  type = number
}

variable "enable_logging" {
  default = false
  type = bool
}

variable "enable_domain" {
  description = "Flag to enable domain specific configurations"
  default = false
  type = bool
}

variable "domain" {
  default = "false.com"
}

variable "zone_name" {
  default = ""
  type = string
}

variable "enable_persistent" {
  description = "A flag to enable or disable the creation of a persistent volume"
  default = true
  type = bool
}

variable "enable_backup" {
  description = "A flag to enable or disable the creation of a backup policy"
  default = false
  type = bool
}

variable "port_mappings" {
  type = list(object({
    containerPort = number
    hostPort      = number
    protocol      = string
  }))
  description = "List of port mappings"
}

variable "persistent_volume_size" {
  description = "The size of the persistent volume"
  default = 40
  type = number
}
