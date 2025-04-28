# Infrastructure Deployment for Zebra Testnet

This repository includes a `Makefile` to streamline the deployment, management, and teardown of AWS infrastructure using Terragrunt.

After creating the infrastructure, which includes the ECR repository, you can use the push-deploy Github workflow to deploy the Zebra Server to ECR and the ECS cluster.
You can see the workflow in this repository's `.github/workflows/push-deploy.yaml` file.

## Prerequisites

Before using this script, ensure the following:

1. **AWS CLI**:
   - Install the AWS CLI.
   - Configure it with your credentials.
   - Ensure the `qed-it` AWS profile exists in `~/.aws/credentials`.

2. **Terragrunt**:
   - Install Terragrunt: [Install Instructions](https://terragrunt.gruntwork.io/docs/getting-started/install/).

3. **Make**:
   - Ensure `make` is installed on your system.

4. **Repository Structure**:
   - The script expects the `infra/terragrunt-aws-environments` directory to exist at the following location:
     ```
     ./zebra/testnet-single-node-deploy/infra/terragrunt-aws-environments
     ```
   - Update the `Makefile` if the directory structure changes.

## Makefile Targets

### 1. `check-prerequisites`
- Verifies that the required tools and configurations are available:
  - AWS CLI is installed.
  - Terragrunt is installed.
  - The `qed-it` AWS profile exists.

### 2. `plan-all`
- **Command**: `make plan-all`
- Plans changes for all modules in the environment specified in the `Makefile`.

### 3. `apply-all`
- **Command**: `make apply-all`
- Applies the planned changes for all modules in the environment.

### 4. `destroy-all`
- **Command**: `make destroy-all`
- Destroys all resources in the specified environment.

## Terragrunt Structure

The deployment uses Terragrunt to manage the infrastructure, following a hierarchical configuration structure:

### Terragrunt Configuration Hierarchy

```
terragrunt-aws-environments/
├── terragrunt.hcl            # Root configuration for all environments
├── dev/                      # Development environment
│   ├── account.hcl           # AWS account variables
│   └── eu-central-1/         # AWS region
│       ├── region.hcl        # Region-specific variables
│       └── zsa/              # ZSA environment
│           ├── env.hcl       # Environment-specific variables
│           ├── vpc/          # VPC infrastructure
│           ├── ecr/          # Container Registry
│           ├── ecs/          # ECS service for Zebra node
│           ├── github-actions-user/ # IAM user for GitHub Actions
│           └── lambda/       # Lambda functions for "Watch Zebra Logs" functionality
│               ├── api-gw/        # API Gateway configuration
│               ├── ecr-logs/      # ECR logs integration
│               ├── iam/           # IAM permissions
│               │   ├── lambda-ecr-policy/
│               │   ├── lambda-read-policy/
│               │   └── lambda-role/
│               └── lambda/        # Lambda function configuration
```

### Key Configuration Files

1. **Root Configuration (`terragrunt.hcl`)**
   - Configures Terragrunt remote state in S3
   - Sets up AWS provider with appropriate region and profile
   - Merges variables from account, region, and environment-level configurations

2. **Account Configuration (`account.hcl`)**
   - Defines AWS account ID and name
   - Sets AWS profile for authentication

3. **Region Configuration (`region.hcl`)**
   - Sets AWS region for deployments

4. **Environment Configuration (`env.hcl`)**
   - Defines environment name (dev)
   - Configures VPC CIDR ranges and subnets
   - Sets availability zones

5. **Module-Specific Configuration (e.g., `ecs/terragrunt.hcl`)**
   - Points to the corresponding Terraform module
   - Defines dependencies on other modules
   - Sets module-specific input variables

## Terraform Modules

The Terragrunt configurations use the following Terraform modules:

### 1. VPC Module

Creates and configures the VPC networking infrastructure:
- Public and private subnets across multiple availability zones
- NAT gateways for private subnet internet access
- Security group configurations
- DNS support and hostnames

### 2. ECR Module

Sets up the Elastic Container Registry for Docker images:
- Creates a named repository for the Zebra node image
- Configures lifecycle policy to maintain the last 15 images
- Enables image tag mutability

### 3. ECS Module

Deploys the Zebra node service using ECS Fargate:
- Creates ECS cluster and service
- Defines task definition with appropriate CPU and memory allocations
- Configures Network Load Balancer with TCP listeners
- Sets up health checks and logging to CloudWatch
- Supports optional domain name with TLS certificate
- Configures persistent storage with EFS (when enabled)
- Creates required IAM roles and policies

### 4. CICD User Module

Creates IAM resources for CI/CD processes:
- Creates IAM user for GitHub Actions
- Sets up IAM policies for ECR push and ECS service management
- Stores access credentials in AWS Secrets Manager

### 5. Lambda Functions

Deploys "Watch Zebra Logs" functionality:
- Source code available at: https://github.com/QED-it/zebra-logs-lambda
- API Gateway for HTTPS endpoint access
- IAM roles and policies for Lambda execution
- Integration with CloudWatch logs to monitor Zebra nodes

## Deployment Architecture

When deployed, the infrastructure creates a Zebra node with:
- VPC with public and private subnets
- ECS Fargate service running in private subnets
- ECR repository for the Zebra Docker image
- Network Load Balancer in public subnets
- Optional domain name with TLS certificate
- Persistent storage with EFS (when enabled)
- CloudWatch logging (when enabled)
- "Watch Zebra Logs" Lambda function with API Gateway for log monitoring

## Usage

1. Navigate to the directory containing the `Makefile`:
   ```bash
   cd ./zebra/testnet-single-node-deploy
   ```

2. Run the appropriate make command:
   ```bash
   # Plan infrastructure changes
   make plan-all
   
   # Apply infrastructure changes
   make apply-all
   
   # Destroy infrastructure
   make destroy-all
   ```

## Additional Management

To delete EFS file system manually:
```bash
aws efs describe-file-systems --query 'FileSystems[*].[FileSystemId,Name]'
aws efs delete-file-system --file-system-id fs-xxx
```

After that, you'll have to run terragrunt refresh & terraform apply to re-create a new EFS file system drive.