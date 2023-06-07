---
name: User bug report
about: Create a report to help us improve
title: '[Bug]: '
labels: C-bug, S-needs-triage
assignees: ''
body:
  - type: markdown
    attributes:
      value: |
        Thanks for taking the time to fill out this bug report!
  - type: textarea
    id: what-happened
    attributes:
      label: What happened?
      description: Also tell us, what did you expect to happen?
      placeholder: Tell us what you see!
      value: "I expected to see this happen:

      Instead, this happened:
    "
    validations:
      required: true
  - type: textarea
    id: reproduce
    attributes:
      label: Steps to reproduce
      description: Copy and paste the exact commands or code here
      placeholder: Tell us how we can reproduce it!
      value: "Behavior or code sample that causes the bug"
    validations:
      required: true
  - type: textarea
    id: logs
    attributes:
      label: Zebra logs
      description: Copy and paste the last 100 Zebra log lines.
      placeholder: Show us the logs!
      value: "copy and paste the logs here"
    validations:
      required: true
  - type: input
    id: version
    attributes:
      label: Version
      description: "For bugs in `zebrad`, run `zebrad version`.

For bugs in the `zebra` libraries, list the `zebra` crate versions.
You can get this information using cargo-tree:
cargo install cargo-tree
cargo tree | grep zebra"
      placeholder: 
    validations:
      required: false
  - type: checkboxes
    id: os
    attributes:
      label: Which operating systems have you used?
      description: You may select more than one.
      options:
        - label: macOS
        - label: Windows
        - label: Linux
  - type: input
    id: os-details
    attributes:
      label: OS details
      description: "Linux, macOS, BSD: the output of `uname -a`
Windows: Windows version and 32-bit or 64-bit"
      placeholder:
    validations:
      required: false
