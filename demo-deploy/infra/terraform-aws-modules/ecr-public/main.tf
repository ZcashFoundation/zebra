provider "aws" {
  alias  = "us_east_1"
  region = "us-east-1"
}

resource "aws_ecrpublic_repository" "public_repository" {
  provider = aws.us_east_1

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


# Create IAM policy to allow pushing images
resource "aws_iam_policy" "ecr_public_push_policy" {
  name        = "ECRPublicPushPolicy"
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

# Attach the policy to the github CICD user
resource "aws_iam_user_policy_attachment" "attach_ecr_public_push_user" {
  user       = "${var.environment}-zebra-github-actions-user"
  policy_arn = aws_iam_policy.ecr_public_push_policy.arn
}
