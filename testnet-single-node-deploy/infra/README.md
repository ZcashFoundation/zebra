# Background
Those 2 folders contain the Terraform modules used to deploy the infrastructure, and the terragrunt files that uses them.
Each of the folders has it own README.md file with more information.
An AWS profile called 'qed-it' is required to be configured in the AWS CLI for running the terragrunt commands.

# To delete EFS file system:
aws efs describe-file-systems --query 'FileSystems[*].[FileSystemId,Name]'
aws efs delete-file-system --file-system-id fs-xxx

After that you'll have to run terragrunt refresh & terraform apply to re-create a new EFS file system drive.
