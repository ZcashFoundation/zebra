resource "aws_ecr_repository" "repository" {
  name                 = "${var.environment}-${var.name}"
  force_delete = true
  image_tag_mutability = "MUTABLE"
}

resource "aws_ecr_lifecycle_policy" "policy" {
  repository = aws_ecr_repository.repository.name

  policy = jsonencode({
    rules = [
      {
        rulePriority = 1
        description  = "Expire old images"
        selection    = {
          tagStatus = "any"
          countType = "imageCountMoreThan"
          countNumber = 50  # Keep the last 50 images
        }
        action = {
          type = "expire"
        }
      }
    ]
  })
}
