# Decision Log

We capture important decisions with [architectural decision records](https://adr.github.io/).

These records provide context, trade-offs, and reasoning taken at our community & technical cross-roads. Our goal is to preserve the understanding of the project growth, and capture enough insight to effectively revisit previous decisions.

To get started, create a new decision record using the template:

```sh
cp template.md NNNN-title-with-dashes.md
```

For more rationale for this approach, see [Michael Nygard's article](http://thinkrelevance.com/blog/2011/11/15/documenting-architecture-decisions).

We've inherited MADR [ADR template](https://adr.github.io/madr/), which is a bit more verbose than Nygard's original template. We may simplify it in the future.

## Evolving Decisions

Many decisions build on each other, a driver of iterative change and messiness
in software. By laying out the "story arc" of a particular system within the
application, we hope future maintainers will be able to identify how to rewind
decisions when refactoring the application becomes necessary.
