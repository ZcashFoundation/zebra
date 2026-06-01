provider "aws" {
  # ECR Public is global, but its API is only available through us-east-1.
  alias               = "us_east_1"
  region              = "us-east-1"
  profile             = var.aws_profile
  allowed_account_ids = [var.aws_account_id]
}

resource "aws_ecrpublic_repository" "public_repository" {
  provider        = aws.us_east_1
  repository_name = var.name

  catalog_data {
    about_text        = var.about_text
    architectures     = [var.architecture]
    description       = var.description
    operating_systems = [var.operating_system]
    usage_text        = var.usage_text
  }

  tags = {
    env = var.environment
  }
}

resource "aws_ecrpublic_repository_policy" "public_pull_policy" {
  provider        = aws.us_east_1
  repository_name = aws_ecrpublic_repository.public_repository.repository_name

  policy = jsonencode({
    Version = "2008-10-17",
    Statement = [
      {
        Sid       = "AllowPublicPull"
        Effect    = "Allow"
        Principal = "*"
        Action = [
          "ecr-public:GetRepositoryCatalogData",
          "ecr-public:BatchCheckLayerAvailability",
          "ecr-public:GetDownloadUrlForLayer",
          "ecr-public:BatchGetImage"
        ]
      }
    ]
  })
}

resource "aws_iam_policy" "ecr_public_push_policy" {
  name        = "${var.environment}-${var.name}-ecr-public-push-policy"
  description = "Allows pushing images to the public ECR repository"

  policy = jsonencode({
    Version = "2012-10-17"
    Statement = [
      {
        Effect = "Allow"
        Action = [
          "ecr-public:GetAuthorizationToken",
          "sts:GetServiceBearerToken",
          "ecr-public:PutImage",
          "ecr-public:BatchCheckLayerAvailability",
          "ecr-public:InitiateLayerUpload",
          "ecr-public:UploadLayerPart",
          "ecr-public:CompleteLayerUpload"
        ]
        Resource = "*"
      },
      {
        Effect = "Allow"
        Action = [
          "ecr-public:PutImage",
          "ecr-public:BatchCheckLayerAvailability",
          "ecr-public:InitiateLayerUpload",
          "ecr-public:UploadLayerPart",
          "ecr-public:CompleteLayerUpload"
        ]
        Resource = "arn:aws:ecr-public::${var.aws_account_id}:repository/${var.name}"
      }
    ]
  })
}

resource "aws_iam_user_policy_attachment" "attach_ecr_public_push_user" {
  user       = "${var.environment}-zebra-github-actions-user"
  policy_arn = aws_iam_policy.ecr_public_push_policy.arn
}
