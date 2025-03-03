output "ecr-public-url" {
  value = aws_ecrpublic_repository.public_repository.repository_uri
}

output "ecr-public-name" {
  value = aws_ecrpublic_repository.public_repository.repository_name
}
