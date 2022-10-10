#! /bin/bash
# Increase the Google Cloud instance sshd connection limit
#
# This script appends 'MaxStartups 500' to /etc/ssh/sshd_config allowing up to 500
# unauthenticated connections to Google Cloud instances.
ps auxwww | grep sshd
echo
sudo grep MaxStartups /etc/ssh/sshd_config
echo 'Original config:'
sudo cat /etc/ssh/sshd_config
echo
echo 'Modifying config:'
echo 'MaxStartups 500' | sudo tee --append /etc/ssh/sshd_config \
|| \
(echo "updating instance sshd config failed: failing test"; exit 1)
sudo grep MaxStartups /etc/ssh/sshd_config
echo 'Modified config:'
sudo cat /etc/ssh/sshd_config
echo
sudo systemctl reload sshd.service
echo
ps auxwww | grep sshd
