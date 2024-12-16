# Background
This folder contains the dockerfile and the Terraform code that will be deployed to the ECS cluster.
There is a github action that will build the docker image and push it to the ECR repository, updating the ECS service to use the latest image.
