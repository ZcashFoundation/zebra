output "ecr-url" {
  value = aws_ecr_repository.repository.repository_url
}

output "ecr-name" {
  value = aws_ecr_repository.repository.name
}
