---
name: Bug report
about: Create a report to help us improve
title: ''
labels: C-bug, S-needs-triage
assignees: ''

---

**Version**

For bugs in `zebrad`, run

`zebrad version`

For bugs in the `zebra` libraries, list the versions of all `zebra` crates you
are using. The easiest way to get this information is using `cargo-tree`.

`cargo install cargo-tree`
(see install here: https://github.com/sfackler/cargo-tree)

Then:

`cargo tree | grep zebra`


**Platform**

The output of `uname -a` (UNIX), or version and 32 or 64-bit (Windows)

**Description**

Enter your issue details here.
One way to structure the description:

[short summary of the bug]

I tried this:

[behavior or code sample that causes the bug]

I expected to see this happen: [explanation]

Instead, this happened: [explanation]

**Commands**

Copy and paste the exact commands you used, so the team can try to reproduce the issue.

**Logs**

Copy and paste the last 100 Zebra log lines.

If you can, upload the full logs to [Gist](https://gist.github.com/), and add a link to them here.
