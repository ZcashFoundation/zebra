#!/usr/bin/env python3

import argparse
import io
import json
import re
import subprocess
import sys
import tarfile
import tempfile
import time
import urllib.error
import urllib.parse
import urllib.request
from pathlib import Path


TEMPORARY_FAILURE = 75
CRATES_IO_API = "https://crates.io/api/v1/crates"
USER_AGENT = "ZcashFoundation/zebra release controller"


class ContradictionError(RuntimeError):
    pass


class TransientError(RuntimeError):
    pass


def read_json(path):
    with Path(path).open(encoding="utf-8") as file:
        return json.load(file)


def validate_plan(plan):
    if plan.get("schema_version") != 1:
        raise ContradictionError("release plan schema_version must be 1")
    for field in ("base_sha", "target_sha"):
        if not re.fullmatch(r"[0-9a-f]{40}", str(plan.get(field, ""))):
            raise ContradictionError(f"release plan {field} must be a full Git SHA")
    packages = plan.get("packages")
    if not isinstance(packages, list) or not packages:
        raise ContradictionError("release plan must contain at least one package")
    names = []
    for package in packages:
        name = package.get("name", "")
        version = package.get("version", "")
        manifest_path_value = package.get("manifest_path", "")
        if not re.fullmatch(r"[A-Za-z0-9_-]+", name):
            raise ContradictionError(f"release plan has invalid package name: {name!r}")
        if not re.fullmatch(r"[0-9A-Za-z.+-]+", version):
            raise ContradictionError(
                f"release plan has invalid version for {name}: {version!r}"
            )
        if not isinstance(manifest_path_value, str) or not manifest_path_value:
            raise ContradictionError(
                f"release plan manifest path for {name} must be repository-relative"
            )
        manifest_path = Path(manifest_path_value)
        if (
            manifest_path.is_absolute()
            or ".." in manifest_path.parts
            or manifest_path.name != "Cargo.toml"
        ):
            raise ContradictionError(
                f"release plan manifest path for {name} must be repository-relative"
            )
        expected_tag = f"v{version}" if name == "zebrad" else f"{name}-v{version}"
        if package.get("tag") != expected_tag:
            raise ContradictionError(
                f"release plan tag for {name} must be {expected_tag}"
            )
        names.append(name)
    if names != sorted(names) or len(names) != len(set(names)):
        raise ContradictionError(
            "release plan packages must be unique and sorted by package name"
        )
    zebrad = plan.get("zebrad")
    zebrad_packages = [package for package in packages if package["name"] == "zebrad"]
    if zebrad is None:
        if zebrad_packages:
            raise ContradictionError("release plan is missing zebrad release metadata")
        return
    if len(zebrad_packages) != 1:
        raise ContradictionError("zebrad release metadata requires one zebrad package")
    package = zebrad_packages[0]
    if (
        zebrad.get("version") != package["version"]
        or zebrad.get("tag") != package["tag"]
    ):
        raise ContradictionError(
            "zebrad release metadata differs from its package plan"
        )
    if zebrad.get("prerelease") != ("-" in package["version"]):
        raise ContradictionError("zebrad prerelease channel differs from its version")
    if not isinstance(zebrad.get("notes"), str) or not zebrad["notes"].strip():
        raise ContradictionError("zebrad release notes must be non-empty")


def validate_release_pr(pr_number, repository, release_pr):
    if not re.fullmatch(r"[1-9][0-9]{0,9}", str(pr_number)):
        raise ContradictionError(
            "release recovery requires a Release PR number, not a commit SHA"
        )
    labels = {label.get("name") for label in release_pr.get("labels", [])}
    merge_sha = (release_pr.get("mergeCommit") or {}).get("oid", "")
    base_sha = release_pr.get("baseRefOid", "")
    head_repository = (release_pr.get("headRepository") or {}).get("nameWithOwner", "")
    valid = (
        release_pr.get("state") == "MERGED"
        and release_pr.get("reviewDecision") == "APPROVED"
        and release_pr.get("baseRefName") == "main"
        and release_pr.get("headRefName", "").startswith("release-plz-")
        and release_pr.get("isCrossRepository") is False
        and head_repository == repository
        and "A-release" in labels
        and re.fullmatch(r"[0-9a-f]{40}", merge_sha)
        and re.fullmatch(r"[0-9a-f]{40}", base_sha)
    )
    if not valid:
        raise ContradictionError(
            f"PR #{pr_number} is not an approved internal A-release Release PR "
            "merged into main"
        )
    return {
        "pr_number": str(pr_number),
        "base_sha": base_sha,
        "target_sha": merge_sha,
    }


def classify(plan, observations):
    all_observations = [
        *observations.get("packages", []),
        *observations.get("tags", []),
    ]
    github_release = observations.get("github_release")
    if github_release:
        all_observations.append(github_release)

    contradictions = [
        observation
        for observation in all_observations
        if observation.get("state") == "contradictory"
    ]
    if contradictions:
        for observation in contradictions:
            print_observation_error("contradictory", observation)
        raise SystemExit(1)

    transient = [
        observation
        for observation in all_observations
        if observation.get("state") == "transient"
    ]
    if transient:
        for observation in transient:
            print_observation_error("transient", observation)
        raise SystemExit(TEMPORARY_FAILURE)

    missing_names = {
        observation["subject"].rsplit("@", 1)[0]
        for observation in observations.get("packages", [])
        if observation.get("state") == "missing"
    }
    missing_packages = [
        package for package in plan["packages"] if package["name"] in missing_names
    ]

    if (
        missing_packages
        and github_release
        and github_release.get("state") == "correct"
        and github_release.get("public", True)
    ):
        print(
            f"contradictory {github_release['subject']}: public GitHub Release exists "
            "while expected packages are missing",
            file=sys.stderr,
        )
        raise SystemExit(1)

    return missing_packages


def observation(state, subject, detail=""):
    return {"state": state, "subject": subject, "detail": detail}


def package_subject(package):
    return f"{package['name']}@{package['version']}"


def raise_for_observations(observations):
    contradictions = [item for item in observations if item["state"] == "contradictory"]
    if contradictions:
        first = contradictions[0]
        raise ContradictionError(
            f"{first['subject']}: {first.get('detail') or 'contradictory external state'}"
        )
    transient = [item for item in observations if item["state"] == "transient"]
    if transient:
        first = transient[0]
        raise TransientError(
            f"{first['subject']}: {first.get('detail') or 'temporary observation failure'}"
        )


def missing_packages(plan, observations):
    missing_names = {
        item["subject"].rsplit("@", 1)[0]
        for item in observations
        if item["state"] == "missing"
    }
    return [package for package in plan["packages"] if package["name"] in missing_names]


def run_command(command, *, cwd=None, input_text=None, capture_output=False):
    return subprocess.run(
        command,
        cwd=cwd,
        input=input_text,
        text=True,
        capture_output=capture_output,
        check=True,
    )


def request_bytes(url):
    request = urllib.request.Request(url, headers={"User-Agent": USER_AGENT})
    try:
        with urllib.request.urlopen(request, timeout=30) as response:
            return response.read()
    except urllib.error.HTTPError as error:
        if error.code == 404:
            raise FileNotFoundError(url) from error
        raise TransientError(f"HTTP {error.code} from {url}") from error
    except (TimeoutError, urllib.error.URLError) as error:
        raise TransientError(f"could not query {url}: {error}") from error


def observe_package(package, target_sha):
    subject = package_subject(package)
    encoded_name = urllib.parse.quote(package["name"], safe="")
    encoded_version = urllib.parse.quote(package["version"], safe="")
    metadata_url = f"{CRATES_IO_API}/{encoded_name}/{encoded_version}"
    try:
        metadata = json.loads(request_bytes(metadata_url))
    except FileNotFoundError:
        return observation("missing", subject)
    except (TransientError, json.JSONDecodeError) as error:
        return observation("transient", subject, str(error))

    registry_version = metadata.get("version", {})
    if (
        registry_version.get("crate") != package["name"]
        or registry_version.get("num") != package["version"]
    ):
        return observation(
            "contradictory",
            subject,
            "crates.io returned different package identity",
        )

    download_url = f"{metadata_url}/download"
    try:
        archive_bytes = request_bytes(download_url)
        archive = tarfile.open(fileobj=io.BytesIO(archive_bytes), mode="r:gz")
        root = f"{package['name']}-{package['version']}"
        vcs_member = archive.getmember(f"{root}/.cargo_vcs_info.json")
        vcs_file = archive.extractfile(vcs_member)
        if vcs_file is None:
            raise KeyError(vcs_member.name)
        vcs_info = json.load(vcs_file)
    except FileNotFoundError as error:
        return observation(
            "transient", subject, f"published archive is not visible yet: {error}"
        )
    except (TransientError, tarfile.TarError) as error:
        return observation("transient", subject, str(error))
    except (KeyError, json.JSONDecodeError) as error:
        return observation(
            "contradictory",
            subject,
            f"published archive has invalid provenance: {error}",
        )

    published_sha = vcs_info.get("git", {}).get("sha1")
    if published_sha != target_sha:
        return observation(
            "contradictory",
            subject,
            f"published archive has VCS SHA {published_sha or 'none'} instead of {target_sha}",
        )
    return observation("correct", subject)


def observe_packages(plan):
    return [
        observe_package(package, plan["target_sha"]) for package in plan["packages"]
    ]


def publish_missing(plan, source_directory, attempts=3):
    last_missing = []
    last_error = None
    for attempt in range(1, attempts + 1):
        observed = observe_packages(plan)
        try:
            raise_for_observations(observed)
        except TransientError as error:
            last_error = error
            if attempt == attempts:
                break
            time.sleep(15 * (2 ** (attempt - 1)))
            continue

        last_missing = missing_packages(plan, observed)
        if not last_missing:
            print(
                "All expected packages are visible on crates.io with correct provenance."
            )
            return

        command = ["cargo", "+1.91.0", "publish", "--locked"]
        for package in last_missing:
            command.extend(["--package", package["name"]])
        print(
            "Publishing missing packages: "
            + ", ".join(package_subject(package) for package in last_missing)
        )
        try:
            run_command(command, cwd=source_directory)
            last_error = None
        except subprocess.CalledProcessError as error:
            last_error = error

        if attempt < attempts:
            time.sleep(15 * (2 ** (attempt - 1)))

    observed = observe_packages(plan)
    try:
        raise_for_observations(observed)
    except TransientError as error:
        last_error = error
    else:
        last_missing = missing_packages(plan, observed)
        if not last_missing:
            print(
                "All expected packages are visible on crates.io with correct provenance."
            )
            return

    remaining = ", ".join(package_subject(package) for package in last_missing)
    detail = f"; last error: {last_error}" if last_error else ""
    raise RuntimeError(
        f"release publication did not converge after {attempts} attempts; "
        f"packages still missing: {remaining or 'unknown'}{detail}"
    )


def observe_tag(package, target_sha, source_directory, remote="origin"):
    tag = package["tag"]
    resolved = resolve_remote_tag(tag, source_directory, remote)
    if resolved["state"] != "correct":
        return resolved
    actual_target = resolved["target_sha"]
    if actual_target != target_sha:
        return observation(
            "contradictory",
            tag,
            f"tag peels to {actual_target} instead of {target_sha}",
        )
    return observation("correct", tag)


def resolve_remote_tag(tag, source_directory, remote="origin"):
    command = [
        "git",
        "-C",
        str(source_directory),
        "ls-remote",
        "--tags",
        remote,
        f"refs/tags/{tag}",
        f"refs/tags/{tag}^{{}}",
    ]
    try:
        result = run_command(command, capture_output=True)
    except subprocess.CalledProcessError as error:
        detail = error.stderr.strip() if error.stderr else str(error)
        return observation("transient", tag, f"could not read remote tag: {detail}")

    refs = {}
    for line in result.stdout.splitlines():
        sha, ref = line.split("\t", 1)
        refs[ref] = sha
    direct = refs.get(f"refs/tags/{tag}")
    peeled = refs.get(f"refs/tags/{tag}^{{}}")
    if direct is None:
        return observation("missing", tag)
    actual_target = peeled or direct
    return {**observation("correct", tag), "target_sha": actual_target}


def gh_api(repository, endpoint, *, method="GET", payload=None):
    command = ["gh", "api", f"repos/{repository}/{endpoint}"]
    if method != "GET":
        command.extend(["--method", method])
    input_text = None
    if payload is not None:
        command.extend(["--input", "-"])
        input_text = json.dumps(payload)
    return run_command(command, input_text=input_text, capture_output=True)


def create_tag(package, target_sha, repository):
    tag_object = gh_api(
        repository,
        "git/tags",
        method="POST",
        payload={
            "tag": package["tag"],
            "message": f"chore: release {package_subject(package)}",
            "object": target_sha,
            "type": "commit",
        },
    )
    tag_sha = json.loads(tag_object.stdout)["sha"]
    gh_api(
        repository,
        "git/refs",
        method="POST",
        payload={"ref": f"refs/tags/{package['tag']}", "sha": tag_sha},
    )


def reconcile_tags(plan, source_directory, repository):
    for package in plan["packages"]:
        observed = observe_tag(package, plan["target_sha"], source_directory)
        if observed["state"] == "correct":
            continue
        if observed["state"] == "contradictory":
            raise ContradictionError(f"{observed['subject']}: {observed['detail']}")
        if observed["state"] == "transient":
            raise TransientError(f"{observed['subject']}: {observed['detail']}")

        try:
            create_tag(package, plan["target_sha"], repository)
        except subprocess.CalledProcessError:
            # A concurrent recovery may have created the same tag. Re-observe it
            # before deciding whether the API failure is safe.
            pass
        reconciled = observe_tag(package, plan["target_sha"], source_directory)
        if reconciled["state"] != "correct":
            error_type = (
                ContradictionError
                if reconciled["state"] == "contradictory"
                else TransientError
            )
            raise error_type(
                f"{reconciled['subject']}: {reconciled.get('detail') or 'tag was not created'}"
            )
        print(f"Reconciled tag {package['tag']} at {plan['target_sha']}.")


def observe_github_release(plan, repository):
    zebrad = plan.get("zebrad")
    if zebrad is None:
        return None
    command = [
        "gh",
        "release",
        "view",
        zebrad["tag"],
        "--repo",
        repository,
        "--json",
        "tagName,name,isDraft,isPrerelease,body",
    ]
    try:
        result = run_command(command, capture_output=True)
    except subprocess.CalledProcessError as error:
        detail = (error.stderr or "").lower()
        if "release not found" in detail or "not found" in detail:
            return observation("missing", zebrad["tag"])
        return observation(
            "transient",
            zebrad["tag"],
            error.stderr.strip() if error.stderr else str(error),
        )
    try:
        release = json.loads(result.stdout)
    except json.JSONDecodeError as error:
        return observation(
            "transient", zebrad["tag"], f"invalid GitHub Release response: {error}"
        )
    if release.get("tagName") != zebrad["tag"]:
        return observation(
            "contradictory", zebrad["tag"], "GitHub Release uses a different tag"
        )
    if release.get("isPrerelease") != zebrad["prerelease"]:
        return observation(
            "contradictory",
            zebrad["tag"],
            "GitHub Release prerelease channel differs from the release plan",
        )
    return {
        **observation("correct", zebrad["tag"]),
        "public": release.get("isDraft") is False,
        "release": release,
    }


def latest_release_tag(repository):
    try:
        result = gh_api(repository, "releases/latest")
    except subprocess.CalledProcessError as error:
        if "404" in (error.stderr or ""):
            return None
        raise TransientError(
            error.stderr.strip()
            if error.stderr
            else "could not read latest GitHub Release"
        ) from error
    return json.loads(result.stdout)["tag_name"]


def stable_release_tags(repository):
    try:
        result = run_command(
            [
                "gh",
                "release",
                "list",
                "--repo",
                repository,
                "--exclude-drafts",
                "--exclude-pre-releases",
                "--limit",
                "1000",
                "--json",
                "tagName",
            ],
            capture_output=True,
        )
        releases = json.loads(result.stdout)
    except subprocess.CalledProcessError as error:
        raise TransientError(
            error.stderr.strip()
            if error.stderr
            else "could not list stable GitHub Releases"
        ) from error
    except json.JSONDecodeError as error:
        raise TransientError(
            f"invalid GitHub Release list response: {error}"
        ) from error
    return [
        release["tagName"]
        for release in releases
        if re.fullmatch(r"v[0-9]+\.[0-9]+\.[0-9]+", release.get("tagName", ""))
    ]


def resolve_remote_tags(tags, source_directory, remote="origin"):
    tags = list(dict.fromkeys(tags))
    if not tags:
        return {}
    patterns = []
    for tag in tags:
        patterns.extend([f"refs/tags/{tag}", f"refs/tags/{tag}^{{}}"])
    try:
        result = run_command(
            [
                "git",
                "-C",
                str(source_directory),
                "ls-remote",
                "--tags",
                remote,
                *patterns,
            ],
            capture_output=True,
        )
    except subprocess.CalledProcessError as error:
        detail = error.stderr.strip() if error.stderr else str(error)
        raise TransientError(f"could not read remote release tags: {detail}") from error

    refs = {}
    for line in result.stdout.splitlines():
        sha, ref = line.split("\t", 1)
        refs[ref] = sha
    return {
        tag: refs.get(f"refs/tags/{tag}^{{}}") or refs.get(f"refs/tags/{tag}")
        for tag in tags
    }


def is_ancestor(older, newer, source_directory):
    result = subprocess.run(
        [
            "git",
            "-C",
            str(source_directory),
            "merge-base",
            "--is-ancestor",
            older,
            newer,
        ],
        check=False,
    )
    if result.returncode not in (0, 1):
        raise TransientError(f"could not compare release commits {older} and {newer}")
    return result.returncode == 0


def should_make_latest(plan, source_directory, repository):
    zebrad = plan["zebrad"]
    if zebrad["prerelease"]:
        return False
    release_tags = stable_release_tags(repository)
    release_targets = resolve_remote_tags(release_tags, source_directory)
    target_sha = plan["target_sha"]
    for release_tag in release_tags:
        if release_tag == zebrad["tag"]:
            continue
        release_sha = release_targets[release_tag]
        if release_sha is None:
            raise ContradictionError(
                f"stable GitHub Release tag {release_tag} is missing"
            )
        if release_sha == target_sha:
            continue
        if is_ancestor(target_sha, release_sha, source_directory):
            return False
    return True


def reconcile_github_release(plan, source_directory, repository):
    zebrad = plan.get("zebrad")
    if zebrad is None:
        print("Library-only release plan; no zebrad GitHub Release is required.")
        return
    observed = observe_github_release(plan, repository)
    if observed["state"] == "contradictory":
        raise ContradictionError(f"{observed['subject']}: {observed['detail']}")
    if observed["state"] == "transient":
        raise TransientError(f"{observed['subject']}: {observed['detail']}")

    make_latest = should_make_latest(plan, source_directory, repository)
    notes_file_handle = tempfile.NamedTemporaryFile(
        mode="w", encoding="utf-8", prefix="zebra-release-notes-", suffix=".md"
    )
    notes_file_handle.write(zebrad["notes"])
    notes_file_handle.flush()
    notes_file = Path(notes_file_handle.name)
    channel_flags = (
        ["--latest=false", "--prerelease"]
        if zebrad["prerelease"]
        else (["--latest"] if make_latest else ["--latest=false", "--prerelease=false"])
    )
    title = f"Zebra {zebrad['version']}"

    if observed["state"] == "missing":
        command = [
            "gh",
            "release",
            "create",
            zebrad["tag"],
            "--repo",
            repository,
            "--title",
            title,
            "--notes-file",
            str(notes_file),
            "--target",
            plan["target_sha"],
            "--verify-tag",
            *channel_flags,
        ]
        run_command(command)
        return

    release = observed["release"]
    latest_tag = latest_release_tag(repository)
    latest_is_correct = (make_latest and latest_tag == zebrad["tag"]) or (
        not make_latest and latest_tag != zebrad["tag"]
    )
    attributes_match = (
        release.get("name") == title
        and release.get("body") == zebrad["notes"]
        and release.get("isDraft") is False
        and latest_is_correct
    )
    if attributes_match:
        print(f"GitHub Release {zebrad['tag']} already matches the release plan.")
        return
    run_command(
        [
            "gh",
            "release",
            "edit",
            zebrad["tag"],
            "--repo",
            repository,
            "--title",
            title,
            "--notes-file",
            str(notes_file),
            "--draft=false",
            *channel_flags,
        ]
    )


def print_observation_error(classification, observation):
    detail = observation.get("detail")
    suffix = f": {detail}" if detail else ""
    print(
        f"{classification} {observation.get('subject', 'release state')}{suffix}",
        file=sys.stderr,
    )


def classify_command(arguments):
    plan = read_json(arguments.plan)
    validate_plan(plan)
    observations = read_json(arguments.observations)
    missing_packages = classify(plan, observations)
    Path(arguments.missing_output).write_text(
        json.dumps(missing_packages, indent=2, sort_keys=True) + "\n",
        encoding="utf-8",
    )


def preflight_command(arguments):
    plan = read_json(arguments.plan)
    validate_plan(plan)
    package_observations = observe_packages(plan)
    tag_observations = [
        observe_tag(package, plan["target_sha"], arguments.source)
        for package in plan["packages"]
    ]
    release_observation = observe_github_release(plan, arguments.repository)
    observations = {
        "packages": package_observations,
        "tags": tag_observations,
        "github_release": release_observation,
    }
    if arguments.observations_output:
        Path(arguments.observations_output).write_text(
            json.dumps(observations, indent=2, sort_keys=True) + "\n",
            encoding="utf-8",
        )
    missing = classify(plan, observations)
    Path(arguments.missing_output).write_text(
        json.dumps(missing, indent=2, sort_keys=True) + "\n", encoding="utf-8"
    )
    print(f"Preflight found {len(missing)} missing package(s).")


def publish_command(arguments):
    plan = read_json(arguments.plan)
    validate_plan(plan)
    publish_missing(plan, arguments.source, attempts=arguments.attempts)


def verify_packages_command(arguments):
    plan = read_json(arguments.plan)
    validate_plan(plan)
    observed = observe_packages(plan)
    raise_for_observations(observed)
    missing = missing_packages(plan, observed)
    if missing:
        raise RuntimeError(
            "expected packages are still missing from crates.io: "
            + ", ".join(package_subject(package) for package in missing)
        )
    print("Verified every expected package and published VCS SHA.")


def reconcile_tags_command(arguments):
    plan = read_json(arguments.plan)
    validate_plan(plan)
    reconcile_tags(plan, arguments.source, arguments.repository)


def reconcile_release_command(arguments):
    plan = read_json(arguments.plan)
    validate_plan(plan)
    reconcile_github_release(plan, arguments.source, arguments.repository)


def resolve_pr_command(arguments):
    if not re.fullmatch(r"[1-9][0-9]{0,9}", arguments.pr_number):
        raise ContradictionError(
            "release recovery requires a Release PR number, not a commit SHA"
        )
    result = run_command(
        [
            "gh",
            "pr",
            "view",
            arguments.pr_number,
            "--repo",
            arguments.repository,
            "--json",
            "baseRefName,baseRefOid,headRefName,headRepository,isCrossRepository,"
            "labels,mergeCommit,reviewDecision,state",
        ],
        capture_output=True,
    )
    resolved = validate_release_pr(
        arguments.pr_number, arguments.repository, json.loads(result.stdout)
    )
    with Path(arguments.github_output).open("a", encoding="utf-8") as output:
        for key, value in resolved.items():
            output.write(f"{key}={value}\n")


def build_parser():
    parser = argparse.ArgumentParser(
        description="Reconcile a Zebra release plan with public release state."
    )
    subparsers = parser.add_subparsers(dest="command", required=True)
    classify_parser = subparsers.add_parser(
        "classify", help="Classify previously observed release state."
    )
    classify_parser.add_argument("plan")
    classify_parser.add_argument("observations")
    classify_parser.add_argument("missing_output")
    classify_parser.set_defaults(function=classify_command)

    preflight_parser = subparsers.add_parser(
        "preflight",
        help="Observe packages, tags, and the GitHub Release without writes.",
    )
    add_plan_source_repository_arguments(preflight_parser)
    preflight_parser.add_argument("missing_output")
    preflight_parser.add_argument("--observations-output")
    preflight_parser.set_defaults(function=preflight_command)

    publish_parser = subparsers.add_parser(
        "publish",
        help="Publish the currently missing package set with bounded retries.",
    )
    publish_parser.add_argument("plan")
    publish_parser.add_argument("--source", type=Path, required=True)
    publish_parser.add_argument("--attempts", type=int, default=3)
    publish_parser.set_defaults(function=publish_command)

    verify_parser = subparsers.add_parser(
        "verify-packages", help="Verify all package identities and VCS provenance."
    )
    verify_parser.add_argument("plan")
    verify_parser.set_defaults(function=verify_packages_command)

    tags_parser = subparsers.add_parser(
        "reconcile-tags", help="Create missing tags after package verification."
    )
    add_plan_source_repository_arguments(tags_parser)
    tags_parser.set_defaults(function=reconcile_tags_command)

    release_parser = subparsers.add_parser(
        "reconcile-release", help="Create or update the public zebrad GitHub Release."
    )
    add_plan_source_repository_arguments(release_parser)
    release_parser.set_defaults(function=reconcile_release_command)

    resolve_parser = subparsers.add_parser(
        "resolve-pr", help="Resolve an approved merged Release PR into immutable SHAs."
    )
    resolve_parser.add_argument("pr_number")
    resolve_parser.add_argument("--repository", required=True)
    resolve_parser.add_argument("--github-output", required=True)
    resolve_parser.set_defaults(function=resolve_pr_command)
    return parser


def add_plan_source_repository_arguments(parser):
    parser.add_argument("plan")
    parser.add_argument("--source", type=Path, required=True)
    parser.add_argument("--repository", required=True)


def main():
    arguments = build_parser().parse_args()
    try:
        arguments.function(arguments)
    except ContradictionError as error:
        print(f"::error title=Contradictory release state::{error}", file=sys.stderr)
        raise SystemExit(1) from error
    except TransientError as error:
        print(f"::error title=Transient release state::{error}", file=sys.stderr)
        raise SystemExit(TEMPORARY_FAILURE) from error
    except (RuntimeError, subprocess.CalledProcessError) as error:
        print(f"::error title=Release reconciliation failed::{error}", file=sys.stderr)
        raise SystemExit(1) from error


if __name__ == "__main__":
    main()
