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

Introduce the exact sequence of command line instructions executed so the team can reproduce the problem.

**Logs**

Paste your entire output or logs. If they are too big please upload them somewhere like [Gist](https://gist.github.com/) and add a link to it here.
