output "cluster-name" {
  value = aws_ecs_cluster.cluster.name
}

output "lb-dns" {
  value = aws_lb.lb.dns_name
}

output "domain_name" {
  value = var.enable_domain ? "https://${aws_route53_record.lb_dns[0].name}" : ""
  description = "The domain name"
}

output "certificate_arn" {
  value = var.enable_domain ? [aws_acm_certificate.cert[0].arn] : []
  description = "The certificate used"
}
