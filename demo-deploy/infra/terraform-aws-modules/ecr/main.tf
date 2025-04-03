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
          countNumber = 15  # Keep the last 15 images
        }
        action = {
          type = "expire"
        }
      }
    ]
  })
}
