### Locals

locals {
  route53_zone_id = var.enable_domain ? tolist(data.aws_route53_zone.selected.*.zone_id)[0] : ""

  port_mappings = var.port_mappings

  used_domain = "${var.environment}.${var.domain}"
}

### ECS Cluster

resource "aws_ecs_cluster" "cluster" {
  name = "${var.environment}-${var.name}-cluster"

  setting {
    name  = "containerInsights"
    value = "enabled"
  }

dynamic "configuration" {
    for_each = var.enable_logging ? [1] : []
    content {
      execute_command_configuration {
        logging = "OVERRIDE"

        dynamic "log_configuration" {
          for_each = var.enable_logging ? [1] : []
          content {
            cloud_watch_log_group_name = aws_cloudwatch_log_group.cluster_log_group.name
          }
        }
      }
    }
  }
}

### ECS Service

resource "aws_ecs_service" "service" {
  name            = "${var.environment}-${var.name}"
  cluster         = aws_ecs_cluster.cluster.id
  task_definition = aws_ecs_task_definition.task.arn
  desired_count   = 1

  enable_execute_command = true

  launch_type = "FARGATE"

  network_configuration {
    subnets          = var.private_subnets
    security_groups  = [aws_security_group.sg.id]
    assign_public_ip = false
  }

  load_balancer {
    target_group_arn = aws_lb_target_group.app-tg-18232.arn
    container_name   = "${var.name}-container"
    container_port   = 18232
  }

  depends_on = [
    aws_lb_target_group.app-tg-18232
  ]

  lifecycle {
    create_before_destroy = true
  }

}

### Task Definition

resource "aws_ecs_task_definition" "task" {
  family                   = "${var.environment}-${var.name}-task"
  network_mode             = "awsvpc"
  requires_compatibilities = ["FARGATE"]
  cpu                      = var.task_cpu
  memory                   = var.task_memory
  execution_role_arn       = aws_iam_role.ecs_execution_role.arn
  task_role_arn            = aws_iam_role.ecs_execution_role.arn

  ephemeral_storage {
    size_in_gib = 70  # Adjust this size according to your needs
  }

  dynamic "volume" {
    for_each = var.enable_persistent ? [1] : []
    content {
      name = "persistent-volume"

      efs_volume_configuration {
        file_system_id     = aws_efs_file_system.persistent_efs[0].id
        root_directory     = "/"
        transit_encryption = "ENABLED"
      }
    }
  }

  container_definitions = jsonencode([
    {
      name      =  "${var.name}-container"
      image     = var.image
      cpu       = var.task_cpu
      memory    = var.task_memory
      essential = true
      ulimits = [{
        name      = "nofile"
        softLimit = 1000000
        hardLimit = 1000000
      }]
      portMappings = local.port_mappings
      logConfiguration = var.enable_logging ? {
        logDriver = "awslogs"
        options = {
          awslogs-group         = aws_cloudwatch_log_group.task_log_group.name
          awslogs-region        = var.region
          awslogs-stream-prefix = "ecs"
        }
      } : null
      healthCheck = {
        command = [
          "CMD-SHELL",
          "curl -X POST -H 'Content-Type: application/json' -d '{\"jsonrpc\":\"2.0\",\"method\":\"getinfo\",\"params\":[],\"id\":1}' http://localhost:18232/ || exit 1"
        ]
        interval     = 15     # Time between health checks in seconds
        timeout      = 10     # Time before a health check is considered failed
        retries      = 3      # Number of consecutive failures before marking as unhealthy
        startPeriod  = 120    # Grace period for the service to start
      }
      
    mountPoints = var.enable_persistent ? [{
        sourceVolume  = "persistent-volume"
        containerPath = "/persistent"  # Mount point in the container
        readOnly      = false
      }] : []

    }
    ]
  )

  depends_on = [ aws_iam_role.ecs_execution_role ]
}

### Load Balancer

resource "aws_lb" "lb" {
  name               = "${var.environment}-${var.name}-lb"
  internal           = false
  load_balancer_type = "network"
  subnets            = var.public_subnets

  idle_timeout       = 6000

  enable_deletion_protection = false
}

resource "aws_security_group" "lb_sg" {
  name        = "${var.environment}-${var.name}-lb-security-group"
  description = "Security group for load balancer"
  vpc_id      = var.vpc_id

  ingress {
    from_port   = 0
    to_port     = 0
    protocol    = "-1"
    cidr_blocks = ["0.0.0.0/0"]
    ipv6_cidr_blocks = ["::/0"]
  }

  egress {
    from_port   = 0
    to_port     = 0
    protocol    = "-1"
    cidr_blocks = ["0.0.0.0/0"]
    ipv6_cidr_blocks = ["::/0"]
  }
}

// Target groups

resource "aws_lb_target_group" "app-tg-18232" {
  name     = "${var.environment}-${var.name}-tg-18232"
  port     = 18232
  protocol = "TCP"
  vpc_id   = var.vpc_id
  target_type = "ip"

  stickiness {                                                                                                                 
    type            = "source_ip"
    enabled         = true
  }

  target_health_state {
    enable_unhealthy_connection_termination = false
  }
}

# TLS listener
# If we want to create a Certificate
data "aws_route53_zone" "zone" {
  name         = var.zone_name
  private_zone = false
}

resource "aws_acm_certificate" "cert" {
  count             = var.enable_domain && var.domain != "false.com" ? 1 : 0
  domain_name               = local.used_domain
  subject_alternative_names = concat(
    ["www.${local.used_domain}"]
  )
  validation_method         = "DNS"
}

resource "aws_route53_record" "validation_records" {
  for_each = {
    for dvo in aws_acm_certificate.cert[0].domain_validation_options : dvo.domain_name => {
      name    = dvo.resource_record_name
      record  = dvo.resource_record_value
      type    = dvo.resource_record_type
      zone_id = data.aws_route53_zone.zone.zone_id
    }
  }

  allow_overwrite = true
  name            = each.value.name
  records         = [each.value.record]
  ttl             = 60
  type            = each.value.type
  zone_id         = each.value.zone_id
}

resource "aws_acm_certificate_validation" "acm_validation" {
  certificate_arn         = aws_acm_certificate.cert[0].arn
  validation_record_fqdns = [for record in aws_route53_record.validation_records : record.fqdn]
}

# Conditional Load Balancer Listener
resource "aws_lb_listener" "tls" {
  count            = var.enable_domain && var.domain != "false.com" ? 1 : 0
  load_balancer_arn = aws_lb.lb.arn
  port              = 443
  protocol          = "TLS"
  ssl_policy        = "ELBSecurityPolicy-TLS13-1-2-2021-06"
  
  # Using the created certificate if available
  certificate_arn   = aws_acm_certificate.cert[0].arn

  default_action {
    type             = "forward"
    target_group_arn = aws_lb_target_group.app-tg-18232.arn
  }
}

resource "aws_lb_listener" "port-18232" {
  load_balancer_arn = aws_lb.lb.arn
  port              = "18232"
  protocol          = "TCP"

  default_action {
    type             = "forward"
    target_group_arn = aws_lb_target_group.app-tg-18232.arn
  }
}

### Security group

resource "aws_security_group" "sg" {
  name        = "${var.environment}-ecs-${var.name}"
  description = "manage rules for ${var.name} ecs service"
  vpc_id      = var.vpc_id

  ingress {
    from_port   = 0
    to_port     = 0
    protocol    = "-1"
    cidr_blocks = ["0.0.0.0/0"]
    ipv6_cidr_blocks = ["::/0"]
  }


  egress {
    from_port        = 0
    to_port          = 0
    protocol         = "-1"
    cidr_blocks      = ["0.0.0.0/0"]
    ipv6_cidr_blocks = ["::/0"]
  }
}

### Volume
resource "random_string" "random" {
  length  = 4 
  special = false
  upper   = false
  numeric  = true
}

resource "aws_efs_file_system" "persistent_efs" {
  count = var.enable_persistent ? 1 : 0
  creation_token = "${var.environment}-${var.region}-${var.name}-${random_string.random.result}"

  lifecycle_policy {
    transition_to_ia = "AFTER_60_DAYS"
  }

  tags = {
    Name = "${var.name}-FileStorage"
  }
}

### Volume backup

resource "aws_efs_backup_policy" "policy" {
  count = var.enable_backup ? 1 : 0
  file_system_id = aws_efs_file_system.persistent_efs[0].id

  backup_policy {
    status = "ENABLED"
  }
}

resource "aws_efs_mount_target" "efs_mt" {
  count = var.enable_persistent ? 1 : 0

  file_system_id  = aws_efs_file_system.persistent_efs[0].id
  subnet_id       = var.private_subnets[0]
  security_groups = [aws_security_group.efs_sg.id]
}

##   Volume Security Group

resource "aws_security_group" "efs_sg" {
  name        = "${var.environment}-efs-${var.name}"
  description = "Security group for EFS used by ${var.name}"
  vpc_id      = var.vpc_id

  ingress {
    from_port   = 2049
    to_port     = 2049
    protocol    = "tcp"
    cidr_blocks = ["0.0.0.0/0"]
  }
}

### Route53 records

data "aws_route53_zone" "selected" {
  count        = var.enable_domain ? 1 : 0
  name         = var.zone_name
  private_zone = false
}

resource "aws_route53_record" "lb_dns" {
  count   = var.enable_domain ? 1 : 0
  zone_id = local.route53_zone_id
  name    = local.used_domain
  type    = "A"

  alias {
    name                   = aws_lb.lb.dns_name
    zone_id                = aws_lb.lb.zone_id
    evaluate_target_health = true
  }
}

# Logs

resource "aws_cloudwatch_log_group" "cluster_log_group" {
  name = "/${var.environment}/ecs/${var.name}-cluster"
  retention_in_days = 90
}


resource "aws_cloudwatch_log_group" "task_log_group" {
  name = "/${var.environment}/ecs/${var.name}-task"
  retention_in_days = 90
}

### Role (Permissions)

resource "aws_iam_role" "ecs_execution_role" {
  name = "${var.environment}-${var.name}-ecs_execution_role"

  assume_role_policy = jsonencode({
    Version = "2012-10-17",
    Statement = [{
      Action = "sts:AssumeRole",
      Effect = "Allow",
      Principal = {
        Service = "ecs-tasks.amazonaws.com"
      },
    }]
  })
}


##   Policies

resource "aws_iam_role_policy_attachment" "ecs_execution_role_policy" {
  role       = aws_iam_role.ecs_execution_role.name
  policy_arn = "arn:aws:iam::aws:policy/service-role/AmazonECSTaskExecutionRolePolicy"
}
