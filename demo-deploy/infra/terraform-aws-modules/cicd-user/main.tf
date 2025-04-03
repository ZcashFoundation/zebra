resource "aws_iam_user" "cicd_user" {
  name = "${var.env}-${var.user_name}"
}

# Create a custom policy for ECR push and EKS access
resource "aws_iam_policy" "ecr_ecs_policy" {
  name = "${var.env}-${var.user_name}-ecr-ecs-policy"
  policy = jsonencode({
    Version = "2012-10-17"
    Statement = [
      {
        Sid    = "AllowECRActions"
        Effect = "Allow"
        Action = [
          "ecr:GetAuthorizationToken",
          "ecr:BatchCheckLayerAvailability",
          "ecr:PutImage",
          "ecr:InitiateLayerUpload",
          "ecr:UploadLayerPart",
          "ecr:BatchGetImage",
          "ecr:GetDownloadUrlForLayer",
          "ecr:CompleteLayerUpload"
        ]
        Resource = "*"
      },
      {
        Sid    = "AllowECSActions"
        Effect = "Allow"
        Action = [
          "ecs:UpdateService",
          "ecs:DescribeServices",
          "ecs:ListServices",
          "ecs:ListTasks",
          "ecs:DescribeTasks",
          "ecs:RunTask",
          "ecs:StopTask",
          "ecs:StartTask"
        ]
        Resource = "*"
      },
      {
        Sid    = "AllowLambdaActions"
        Effect = "Allow"
        Action = [
          "lambda:*"
        ]
        Resource = "arn:aws:lambda:${var.aws_region}:${var.aws_account_id}:function:watch-zebra-logs"
      }
    ]
  })
}

# Attach the ECR push policy to the user
resource "aws_iam_user_policy_attachment" "ecr_push_attachment" {
  user       = aws_iam_user.cicd_user.name
  policy_arn = aws_iam_policy.ecr_ecs_policy.arn
}

# Create IAM access keys for the user
resource "aws_iam_access_key" "cicd_user_key" {
  user = aws_iam_user.cicd_user.name
}

# Store IAM access keys in Secrets Manager
resource "aws_secretsmanager_secret" "credentials" {
  name = "/${var.env}/${var.user_name}_iam_user_creds"
}

resource "aws_secretsmanager_secret_version" "credentials_version" {
  secret_id     = aws_secretsmanager_secret.credentials.id
  secret_string = jsonencode({
    AWS_ACCESS_KEY_ID     = aws_iam_access_key.cicd_user_key.id,
    AWS_SECRET_ACCESS_KEY = aws_iam_access_key.cicd_user_key.secret
  })
}
