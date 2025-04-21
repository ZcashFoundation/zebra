data "aws_security_group" "default" {
  name   = "default"
  vpc_id = module.vpc.vpc_id
}

resource "aws_security_group_rule" "allow_outbound" {
  security_group_id = data.aws_security_group.default.id
  type              = "egress"
  to_port           = 0
  protocol          = "-1"
  from_port         = 0
  cidr_blocks       = ["0.0.0.0/0"]

  // fixes error: A duplicate Security Group rule was found
  depends_on = [
    module.vpc
  ]
}

resource "aws_security_group_rule" "allow_all_self" {
  count = var.default_sg_allow_all_self ? 1 : 0
  security_group_id = data.aws_security_group.default.id
  type              = "ingress"
  to_port           = 0
  protocol          = "-1"
  from_port         = 0
  self = true

// fixes error: A duplicate Security Group rule was found
  depends_on = [
    module.vpc
  ]
}

module "vpc" {
  source = "terraform-aws-modules/vpc/aws"

  name = "${var.environment}-tf-vpc"

  cidr = var.cidr

  azs                 = var.availability_zones
  private_subnets     = var.internal_subnets
  public_subnets      = var.external_subnets

  create_database_subnet_group = false

  enable_dns_hostnames = true
  enable_dns_support   = true
  enable_nat_gateway = true

  # Default security group - ingress/egress rules cleared to deny all
  manage_default_security_group  = true
  default_security_group_ingress = []
  default_security_group_egress  = []

  tags = {
    Environment = var.environment
    Name        = "${var.environment}-tf-vpc"
  }

}

