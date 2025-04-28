output "cicd_user_name" {
  value = aws_iam_user.cicd_user.name
}

output "cicd_user_arn" {
  value = aws_iam_user.cicd_user.arn
}

output "access_key_id" {
  value     = aws_iam_access_key.cicd_user_key.id
  sensitive = true
}

output "secret_access_key" {
  value     = aws_iam_access_key.cicd_user_key.secret
  sensitive = true
}
