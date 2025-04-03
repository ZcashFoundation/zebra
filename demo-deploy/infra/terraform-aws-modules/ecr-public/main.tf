provider "aws" {
  alias  = "us_east_1"
  region = "us-east-1"
}

resource "aws_ecrpublic_repository" "tx-tool" {
  provider = aws.us_east_1

  repository_name = "tx-tool"

  catalog_data {
    about_text        = "Qedit tx-tool"
    architectures     = ["ARM"]
    description       = "Qedit tx-tool is a tool for testing the Zebra node and showcasing its capabilities"
    operating_systems = ["Linux"]
    usage_text        = "Run the docker image with ZCASH_NODE_ADDRESS, ZCASH_NODE_PORT, ZCASH_NODE_PROTOCOL arguments to connect to the Zebra node"
  }

  tags = {
    env = "production"
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
        Resource = "arn:aws:ecr-public::496038263219:repository/tx-tool"
      }
    ]
  })
}

# Attach the policy to the github CICD user
resource "aws_iam_user_policy_attachment" "attach_ecr_public_push_user" {
  user       = "dev-zebra-github-actions-user"
  policy_arn = aws_iam_policy.ecr_public_push_policy.arn
}
