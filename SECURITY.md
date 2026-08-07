This page is copyright Zcash Foundation, 2021. It is posted in order to conform to this standard: <https://github.com/RD-Crypto-Spec/Responsible-Disclosure/tree/d47a5a3dafa5942c8849a93441745fdd186731e6>

# Security Disclosures

## Disclosure Principles

The Zcash Foundation's security disclosure process aims to achieve the following goals:

- protecting Zcash users and the wider Zcash ecosystem
- respecting the work of security researchers
- improving the ongoing health of the Zcash ecosystem

Specifically, we will:

- assume good faith from researchers and ecosystem partners
- operate a no fault process, focusing on the technical issues
- work with security researchers, regardless of how they choose to disclose issues

## Receiving Disclosures

The Zcash Foundation is committed to working with researchers who submit
security vulnerability notifications to us to resolve those issues on an
appropriate timeline and perform a coordinated release, giving credit to the
reporter if they would like. We align our reporting channels with the broader
Zcash ecosystem disclosure process.

### Before You Report

Before submitting a report, confirm that the issue affects the latest Zebra
release (<https://github.com/ZcashFoundation/zebra/releases/latest>) or the
current `main` branch. Any proof of concept must be tested against one of
those two versions — reports reproduced only on older releases, forks, or
modified builds may not represent a vulnerability in current code and take
substantially longer to triage.

In your report, state the exact release version or `main` commit hash you
tested against.

For critical vulnerabilities, notify us on Signal. Create a new Signal group
(do not reuse a previous group for a separate issue) that includes all of the
following handles:

- `pilizcash.01`
- `conrado.42`
- `dc_zf.77`

Treat a vulnerability as critical if it could cause consensus divergence or a
chain split, loss or counterfeiting of funds, a persistent node halt, state
corruption, or remote compromise of a node.

For all other vulnerabilities, use the GitHub "Report a Vulnerability" feature
for Zebra at <https://github.com/ZcashFoundation/zebra/security/advisories>.

If you cannot reach us by Signal or GitHub, fall back to email at
<security@zfnd.org> using the following PGP key. We no longer treat email as a
primary or fully reliable reporting channel, so use it only when the channels
above are unavailable. The key may also be used to encrypt follow-up material
once contact is established.

The key is `Zcash Foundation Security Team <security@zfnd.org>`, fingerprint
`7550 C36C 3DF6 16A6 9F1E FE00 6046 DDEF 94CF 99B5`, valid until 2028-03-03.
Verify the fingerprint before encrypting to this key.

```
-----BEGIN PGP PUBLIC KEY BLOCK-----

mDMEaagchhYJKwYBBAHaRw8BAQdA1dH8bFObJHo09FBETWW3nOxhnu1bO7NYdJqA
uL5LmPO0MlpjYXNoIEZvdW5kYXRpb24gU2VjdXJpdHkgVGVhbSA8c2VjdXJpdHlA
emZuZC5vcmc+iJYEExYKAD4WIQR1UMNsPfYWpp8e/gBgRt3vlM+ZtQUCaagchgIb
AwUJA8JnAAULCQgHAwUVCgkICwUWAgMBAAIeAQIXgAAKCRBgRt3vlM+ZtfDKAQDr
pYIU2e9e27h8ASgWbqXVzHWD2XDUsZRQtfkTrd4mAAD9EX0D8P1GC0JbJ3OSRKL9
qRMVaXVyR+MGiY4fq9umUAm4OARpqByGEgorBgEEAZdVAQUBAQdAV7XUsnWCFefO
rZMjcAHn750DNKSWqZJVG1Q/X/QJkBwDAQgHiH4EGBYKACYWIQR1UMNsPfYWpp8e
/gBgRt3vlM+ZtQUCaagchgIbDAUJA8JnAAAKCRBgRt3vlM+ZteKxAP9Mf9lzcCne
G7gptbThqUzGi0EaFoaLOi/9MFfSxMRChgEAtVpsm/bpf9kDYv//LCjQ32mG594e
jlOOQUwFoIKfgAk=
=K4Oq
-----END PGP PUBLIC KEY BLOCK-----
```

## Sending Disclosures

In the case where we become aware of security issues affecting other projects that have never affected Zebra or Zcash, our intention is to inform those projects of security issues on a best effort basis.

In the case where we fix a security issue in Zebra or Zcash that also affects the following neighboring projects, our intention is to engage in responsible disclosures with them as described in <https://github.com/RD-Crypto-Spec/Responsible-Disclosure>, subject to the deviations described in the section at the bottom of this document.

## Bilateral Responsible Disclosure Agreements

We have set up agreements with the following neighboring projects to share vulnerability information, subject to the deviations described in the next section.

Specifically, we have agreed to engage in responsible disclosures for security issues affecting Zebra or Zcash technology with the following teams:

- Zcash Open Development Lab (ZODL), which maintains the `zcash/zcash` core node, `librustzcash`, `zallet`, and related software, via its security disclosure process at <https://github.com/zcash/.github/blob/main/SECURITY.md>
- Shielded Labs, which maintains the Crosslink proof-of-stake and Network Sustainability Mechanism work and its associated Zebra and `librustzcash` forks.

## Deviations from the Standard

### Monetary Base Protection

Zcash is a technology that provides strong privacy. Notes are encrypted to their destination, and then the monetary base is kept via zero-knowledge proofs intended to only be creatable by the real holder of Zcash. If this fails, and a counterfeiting bug results, that counterfeiting bug might be exploited without any way for blockchain analyzers to identify the perpetrator or which data in the blockchain has been used to exploit the bug. Rollbacks before that point, such as have been executed in some other projects in such cases, are therefore impossible.

The standard describes reporters of vulnerabilities including full details of an issue, in order to reproduce it. This is necessary for instance in the case of an external researcher both demonstrating and proving that there really is a security issue, and that security issue really has the impact that they say it has - allowing the development team to accurately prioritize and resolve the issue.

In the case of a counterfeiting bug, we might decide not to include those details with our reports to partners ahead of coordinated release, so long as we are sure that they are vulnerable.
