#!/bin/bash
# Copyright 2020 Google LLC
#
# Licensed under the Apache License, Version 2.0 (the "License");
# you may not use this file except in compliance with the License.
# You may obtain a copy of the License at
#
#      http://www.apache.org/licenses/LICENSE-2.0
#
# Unless required by applicable law or agreed to in writing, software
# distributed under the License is distributed on an "AS IS" BASIS,
# WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
# See the License for the specific language governing permissions and
# limitations under the License.

#remove runner on stop signal
remove_runner() {
    ./config.sh remove --unattended --token "$(curl -sS --request POST --url "https://api.github.com/repos/${REPO_OWNER}/${REPO_NAME}/actions/runners/remove-token" --header "authorization: Bearer ${GITHUB_TOKEN}"  --header "content-type: application/json" | jq -r .token)"
    exit 0
}

#Trap termination signals
trap 'remove_runner' SIGINT SIGQUIT SIGTERM


# shellcheck disable=SC2034
#ACTIONS_RUNNER_INPUT_NAME is read by config.sh
#set name for this runner as the hostname
ACTIONS_RUNNER_INPUT_NAME=$HOSTNAME
#get regsistration token for this runnner
ACTIONS_RUNNER_INPUT_TOKEN="$(curl -sS --request POST --url "https://api.github.com/repos/${REPO_OWNER}/${REPO_NAME}/actions/runners/registration-token" --header "authorization: Bearer ${GITHUB_TOKEN}"  --header 'content-type: application/json' | jq -r .token)"
# Create the runner and start the configuration script in unattended mode
./config.sh --unattended --disableupdate --replace --work "/tmp" --url "$ACTIONS_RUNNER_INPUT_URL" --token "$ACTIONS_RUNNER_INPUT_TOKEN"
#start runner
./run.sh
