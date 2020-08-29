# Contributing

* [Running and Debugging](#running-and-debugging)
* [Bug Reports](#bug-reports)
* [Pull Requests](#pull-requests)
* [Zebra RFCs](#zebra-rfcs)

## Running and Debugging
[running-and-debugging]: #running-and-debugging

See the [user documentation](https://zebra.zfnd.org/user.html) for details on
how to build, run, and instrument Zebra.

## Bug Reports
[bug-reports]: #bug-reports

[File an issue](https://github.com/ZcashFoundation/zebra/issues/new/choose)
on the issue tracker using the bug report template.

## Pull Requests
[pull-requests]: #pull-requests

PRs are welcome for small and large changes, but please don't make large PRs
without coordinating with us via the issue tracker or Discord. This helps
increase development coordination and makes PRs easier to merge.

Check out the [help wanted][hw] or [good first issue][gfi] labels if you're
looking for a place to get started!

[hw]: https://github.com/ZcashFoundation/zebra/labels/E-help-wanted
[gfi]: https://github.com/ZcashFoundation/zebra/labels/good%20first%20issue

## Zebra RFCs
[zebra-rfcs]: #zebra-rfcs

Significant changes to the Zebra codebase are planned using Zebra RFCs. These
allow structured discussion about a proposed change and provide a record of
the planned design.

To make a Zebra RFC:

1. Choose a short feature name like `my-feature`.

2. Copy the `book/src/dev/rfcs/0000-template.md` file to
`book/src/dev/rfcs/XXXX-my-feature.md`.

3. Edit the template header to add the feature name and the date, but leave
the other fields blank for now.

4. Write the design! The template has a suggested list of sections that are a
useful guide.

5. Create an design PR using the RFC template.

6. Take the next available RFC number (not conflicting with any existing RFCs
or design PRs) and name the RFC file accordingly, e.g., `0027-my-feature.md`
for number 27. Make sure that `book/src/SUMMARY.md` links to the numbered RFC.

7. After creating an RFC PR, update the RFC header and the PR description
with the PR number.

8. After the RFC is accepted, create an issue for the implementation of the
design, and update the RFC header and PR description with the implementation
issue number.

