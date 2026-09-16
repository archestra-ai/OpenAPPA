"""The huggingface repository annotators: one consult in, one answer out.

Two annotators share this script, told apart by the consult's name. Both
read every repository the call names — inside `repo_ids` or inside an
`hf://` URI — and ask the Hub who reads it:

  huggingface.repo-visibility   what a read returns: content pushed by
                                whoever owns the repository (`suspicious`),
                                read by everyone for a public repository,
                                by the viewer alone (`self`) for a gated
                                one's files, a private one, or a listing
                                the token sees private names in
  huggingface.repo-readers      what a write needs: trusted data that
                                everyone may see for a public target,
                                that the viewer may see for a private
                                repository of the viewer's own

A private organization repository is read by its members minus those
with the `no_access` role, a set a read token cannot tell apart, so its
content narrows to `self` — unless the Hub lists the repository in a
resource group the token can see and the policy admits that group's
collection `@huggingface:org/<org>/resource-group/<group>/members`,
which the `huggingface` audience source resolves. A write into a
private organization repository needs data everyone may see, since the
organization's admins read every repository and no collection lists
them. A repository the token cannot see, an `hf://buckets` URI, a
malformed id, or any Hub error exits nonzero: the runtime treats that
as no answer and refuses the operation, so nothing is guessed public.

The Hub copies `create_repo`'s `source_uri` server-side, a flow the
trajectory never carries, so this script admits the copy only when the
source's readers are inside the target's: a public source, or the
viewer's own private repository into another of the viewer's own.

Credentials come from APPA_PROVIDER_HUGGINGFACE_TOKEN, else the Hugging
Face CLI's stored login (see hf_token.py); the Hub root is HF_ENDPOINT
when set.
"""

import concurrent.futures
import json
import re
import sys
import threading
import urllib.parse
from dataclasses import dataclass
from pathlib import Path

# The sibling module is found beside this file however the file is loaded:
# run by the runtime from its own directory, or imported by path from another.
sys.path.insert(0, str(Path(__file__).resolve().parent))
from hf_token import Forbidden, NotFound, hub_api, resolve_token  # noqa: E402, F401


CONTENT = "huggingface.repo-visibility"
READERS = "huggingface.repo-readers"
PROVIDER = "huggingface"
# hf_fs_write carries the file it puts inside the consult.
MAX_INPUT_BYTES = 4 * 1024 * 1024

PUBLIC = "public"
SELF = "self"

# Repository kinds by the plural the Hub API and hf:// URIs spell them with.
KINDS = {"models": "model", "datasets": "dataset", "spaces": "space"}
PLURALS = {kind: plural for plural, kind in KINDS.items()}
# The maximum entries each tool's schema admits.
MAX_REPO_IDS = 10
MAX_OPERATIONS = 30
# hf_fs commands that return file content; the others return listings and metadata.
CONTENT_COMMANDS = ("cat", "attach", "search")
SEGMENT = re.compile(r"^[A-Za-z0-9][A-Za-z0-9._-]*$")
# Repository lookups one consult runs at once: enough to keep a batch inside
# the runtime's consult budget, few enough for the Hub's rate limit.
LOOKUPS = 8


@dataclass(frozen=True)
class Repo:
    """One repository reference as a call spells it, the revision dropped:
    a revision changes content, never readers. `kind` is None when the
    call leaves the type to the server's auto-detection."""

    kind: str | None
    owner: str
    name: str

    def path(self, kind):
        return f"/api/{PLURALS[kind]}/{quote(self.owner)}/{quote(self.name)}"

    def id(self):
        return f"{self.owner}/{self.name}"


@dataclass(frozen=True)
class Facts:
    """What the Hub reports about one repository."""

    kind: str
    owner: str
    name: str
    private: bool
    gated: bool


def quote(segment):
    return urllib.parse.quote(segment, safe="")


def segment(value, label):
    if not isinstance(value, str) or not SEGMENT.match(value):
        raise ValueError(f"{label} {value!r} is not one repository segment")
    return value


def parse_repo_id(text, kind=None):
    """`OWNER/NAME[@REVISION]`, as `repo_ids` and Space ids spell it."""
    if not isinstance(text, str):
        raise ValueError("a repository id must be a string")
    match text.split("/"):
        case [owner, name]:
            name, _, _revision = name.partition("@")
            return Repo(kind, segment(owner, "owner"), segment(name, "name"))
        case _:
            raise ValueError(f"{text!r} is not OWNER/NAME")


# What one hf:// URI names: readers known without a lookup, one repository,
# or one collection.
@dataclass(frozen=True)
class Fixed:
    readers: str


@dataclass(frozen=True)
class Collection:
    owner: str
    slug: str


def parse_uri(uri, repo_only=False):
    """The class of one `hf://` URI: a public tree, a listing the token may
    see private names in (`self`), one repository, or one collection.
    Anything else, buckets included, is refused: no visibility API is
    pinned for it."""
    if not isinstance(uri, str) or not uri.startswith("hf://"):
        raise ValueError(f"{uri!r} is not an hf:// URI")
    body = uri[len("hf://") :]
    segments = body.split("/") if body else []
    if any(not part for part in segments):
        raise ValueError(f"{uri!r} has an empty path segment")
    match segments:
        case [] | [("papers" | "docs"), *_] if not repo_only:
            return Fixed(PUBLIC)
        case [("models" | "datasets" | "spaces" | "collections")] if not repo_only:
            return Fixed(SELF)
        case [("models" | "datasets" | "spaces" | "collections"), _owner] if not repo_only:
            return Fixed(SELF)
        case ["collections", owner, slug, *_] if not repo_only:
            return Collection(segment(owner, "owner"), segment(slug, "slug"))
        case [plural, owner, name, *_] if plural in KINDS:
            name, _, _revision = name.partition("@")
            return Repo(KINDS[plural], segment(owner, "owner"), segment(name, "name"))
        case _:
            raise ValueError(f"{uri!r} names no tree this policy knows the readers of")


def parse_repo_uri(uri, label):
    """`hf://models|datasets|spaces/OWNER/NAME`, exactly: a create_repo target or source."""
    match parse_uri(uri, repo_only=True):
        case Repo() as repo if uri.count("/") == 4 and "@" not in uri:
            return repo
        case _:
            raise ValueError(f"{label} must be hf://models|datasets|spaces/OWNER/NAME, not {uri!r}")


class Hub:
    """The Hub as one consult sees it: each repository read once, the viewer once."""

    def __init__(self, call):
        self.call = call
        self._lock = threading.Lock()
        self._viewer = None
        self._facts = {}
        self._collections = {}
        self._groups = {}

    def viewer(self):
        with self._lock:
            known = self._viewer
        if known is None:
            known = self.call("/api/whoami-v2").get("name")
            if not isinstance(known, str) or not known:
                raise RuntimeError("whoami-v2 reports no account name")
            with self._lock:
                self._viewer = known
        return known

    def facts(self, repo, kind):
        """The repository as one type, read from the Hub once per consult;
        a repository the Hub does not know under that type is remembered
        as such, so the fold after a prefetch asks nothing twice."""
        key = (kind, repo.owner, repo.name)
        with self._lock:
            known = self._facts.get(key)
        if known is NotFound:
            raise NotFound(repo.path(kind))
        if known is None:
            try:
                payload = self.call(repo.path(kind))
            except NotFound:
                with self._lock:
                    self._facts[key] = NotFound
                raise
            private = payload.get("private") if isinstance(payload, dict) else None
            if not isinstance(private, bool):
                raise RuntimeError(f"the Hub reports no visibility for {kind} {repo.id()}")
            gated = payload.get("gated", False)
            if gated not in (False, "auto", "manual"):
                raise RuntimeError(f"the Hub reports the unknown gating {gated!r} for {kind} {repo.id()}")
            author = payload.get("author")
            owner = author if isinstance(author, str) and author else repo.owner
            known = Facts(kind, owner, repo.name, private, gated is not False)
            with self._lock:
                self._facts[key] = known
        return known

    def kinds_of(self, repo):
        return [repo.kind] if repo.kind else list(PLURALS)

    def probe(self, repo, kind):
        """One typed lookup whose miss is an answer when the call left the type open."""
        try:
            self.facts(repo, kind)
        except NotFound:
            if repo.kind:
                raise

    def facts_of(self, repo):
        """The repository as every type the call admits; auto-detection
        answers each type the name exists as."""
        found = []
        for kind in self.kinds_of(repo):
            try:
                found.append(self.facts(repo, kind))
            except NotFound:
                if repo.kind:
                    raise
        if not found:
            raise NotFound(repo.id())
        return found

    def prefetch(self, targets, declared):
        """Everything a batch will ask the Hub, asked at once: each
        repository under every type the call admits and each collection,
        then, when the policy admits a group collection, the resource
        groups of every private organization repository found. The first
        failure refuses the batch."""
        repos = {target for target in targets if isinstance(target, Repo)}
        collections = {target for target in targets if isinstance(target, Collection)}
        with concurrent.futures.ThreadPoolExecutor(max_workers=LOOKUPS) as pool:
            lookups = [pool.submit(self.probe, repo, kind) for repo in repos for kind in self.kinds_of(repo)]
            lookups += [pool.submit(self.collection_private, collection) for collection in collections]
            for lookup in lookups:
                lookup.result()
            if not admits_groups(declared):
                return
            private = {facts.owner for repo in repos for facts in self.facts_of(repo) if facts.private}
            owners = private - {self.viewer()} if private else private
            for _listing in pool.map(self.resource_groups, owners):
                pass

    def collection_private(self, collection):
        with self._lock:
            known = self._collections.get(collection)
        if known is None:
            payload = self.call(f"/api/collections/{quote(collection.owner)}/{quote(collection.slug)}")
            known = payload.get("private") if isinstance(payload, dict) else None
            if not isinstance(known, bool):
                raise RuntimeError(f"the Hub reports no visibility for collection {collection.owner}/{collection.slug}")
            with self._lock:
                self._collections[collection] = known
        return known

    def resource_groups(self, owner):
        """The resource groups the Hub lists for one organization among
        those the token can see; an unreadable or malformed listing is an
        empty one, since the repository lookup already attested the viewer
        reads what is asked about."""
        with self._lock:
            known = self._groups.get(owner)
        if known is None:
            try:
                listing = self.call(f"/api/organizations/{quote(owner)}/resource-groups")
            except (NotFound, Forbidden, RuntimeError):
                listing = []
            known = listing if isinstance(listing, list) else []
            with self._lock:
                self._groups[owner] = known
        return known

    def resource_group(self, facts):
        """The resource group the Hub lists the repository in, among the
        groups the token can see: its id and name, or None. The listing
        is scoped to the token, so an absent repository proves nothing
        and the caller falls back to the viewer."""
        for group in self.resource_groups(facts.owner):
            if not isinstance(group, dict) or not isinstance(group.get("resources"), list):
                continue
            for resource in group["resources"]:
                if not isinstance(resource, dict) or resource.get("type") != facts.kind:
                    continue
                if resource.get("name") in (f"{facts.owner}/{facts.name}", facts.name):
                    return group.get("id"), group.get("name")
        return None


def admits_groups(declared):
    """Whether the policy names any resource-group collection; without
    one every private repository narrows to the viewer and the Hub is
    asked about no group."""
    return any(isinstance(entry, str) and entry.startswith(f"@{PROVIDER}:org/") for entry in declared)


def group_collection(owner, group, declared):
    """The declared collection naming the group, by id first, else by
    name when the name is one path segment; None when the policy admits
    neither."""
    group_id, group_name = group
    for key in (group_id, group_name):
        if isinstance(key, str) and SEGMENT.match(key):
            spelling = f"@{PROVIDER}:org/{owner}/resource-group/{key}/members"
            if spelling in declared:
                return spelling
    return None


def read_by(hub, facts, content, declared):
    """The narrowest readers this source can name for what the repository holds."""
    if not facts.private:
        return SELF if facts.gated and content else PUBLIC
    if not admits_groups(declared) or facts.owner == hub.viewer():
        return SELF
    group = hub.resource_group(facts)
    if group is None:
        return SELF
    return group_collection(facts.owner, group, declared) or SELF


def must_reach(hub, private, owner):
    """Everyone a write into the repository reaches: the viewer for a
    private repository of the viewer's own, everyone otherwise — an
    organization's admins read every private repository, a set no
    collection lists."""
    if not private:
        return PUBLIC
    return SELF if owner == hub.viewer() else PUBLIC


def fold(readers):
    """One audience for a call returning several repositories: everyone
    when all are public, one declared collection when every non-public
    entry is that collection, else the viewer, who reads each entry."""
    restricted = {entry for entry in readers if entry != PUBLIC}
    match sorted(restricted):
        case []:
            return PUBLIC
        case [one] if one != SELF:
            return one
        case _:
            return SELF


def written(readers):
    """An audience in the written grammar: the bare `public` token, else a one-entry list."""
    return PUBLIC if readers == PUBLIC else [readers]


def call_of(consult):
    """The annotator asked, the tool called, and its arguments."""
    if not isinstance(consult, dict):
        raise ValueError("the consult must be an object")
    if consult.get("version") != 1:
        raise ValueError("unsupported request version")
    if consult.get("kind") != "annotation":
        raise ValueError("unexpected consult kind")
    name = consult.get("name")
    if name not in (CONTENT, READERS):
        raise ValueError(f"unexpected annotator name {name!r}")
    artifact = consult.get("artifact")
    args = artifact.get("args") if isinstance(artifact, dict) else None
    tool = args.get("name") if isinstance(args, dict) else None
    arguments = args.get("arguments") if isinstance(args, dict) else None
    if not isinstance(tool, str) or not isinstance(arguments, dict):
        raise ValueError("the call's name or arguments are missing")
    return name, tool.rsplit("/", 1)[-1], arguments


def check_declaration(consult):
    """The mandate the policy declared for this call must admit `self`, the
    audience every non-public answer falls back to. A mandate without it
    is a policy and a script of different versions, refused before any
    credential is read; the exit status 2 tells it apart from a provider
    failure. Collections are read from the same list and never assumed."""
    declared = consult.get("declaration", {}).get("audiences")
    if not isinstance(declared, list) or SELF not in declared:
        print(f"{consult.get('name')}: the policy admits {declared!r}, this script answers with {SELF!r}", file=sys.stderr)
        raise SystemExit(2)
    return declared


def bounded_list(value, label, maximum):
    if not isinstance(value, list) or not value:
        raise ValueError(f"{label} must be a non-empty list")
    if len(value) > maximum:
        raise ValueError(f"{label} holds {len(value)} entries, more than the {maximum} the tool admits")
    return value


def reads_of(tool, arguments):
    """Everything a read call returns: fixed readers, repositories with a
    content flag, and collections."""
    targets = []
    match tool:
        case "hub_repo_details":
            kind = arguments.get("repo_type")
            if kind is not None and kind not in PLURALS:
                raise ValueError(f"unknown repo_type {kind!r}")
            operations = arguments.get("operations", ["overview"])
            if not isinstance(operations, list):
                raise ValueError("operations must be a list")
            content = "dataset_preview" in operations
            for repo_id in bounded_list(arguments.get("repo_ids"), "repo_ids", MAX_REPO_IDS):
                targets.append((parse_repo_id(repo_id, kind), content))
        case "hf_fs":
            for operation in bounded_list(arguments.get("operations"), "operations", MAX_OPERATIONS):
                if not isinstance(operation, dict):
                    raise ValueError("each operation must be an object")
                command = operation.get("cmd")
                args = operation.get("args")
                if not isinstance(args, list) or not args:
                    raise ValueError(f"{command!r} names no hf:// URI")
                targets.append((parse_uri(args[0]), command in CONTENT_COMMANDS))
        case _:
            raise ValueError(f"{tool!r} is not a read this annotator labels")
    return targets


def read_answer(hub, tool, arguments, declared):
    targets = reads_of(tool, arguments)
    hub.prefetch([target for target, _content in targets], declared)
    readers = []
    for target, content in targets:
        match target:
            case Fixed(fixed):
                readers.append(fixed)
            case Collection() as collection:
                readers.append(SELF if hub.collection_private(collection) else PUBLIC)
            case Repo() as repo:
                readers.append(fold([read_by(hub, facts, content, declared) for facts in hub.facts_of(repo)]))
    return {
        "delta": {"trust": "suspicious", "audience": written(fold(readers))},
        "requires": {"history": [], "attention": []},
        "emits": [],
    }


def write_requires(target_readers):
    return {"trust": "trusted", "audience": {"contains": written(target_readers)}, "history": [], "attention": []}


def write_answer(hub, tool, arguments, declared):
    match tool:
        case "hf_fs_write":
            args = arguments.get("args")
            if not isinstance(args, list) or not args:
                raise ValueError("hf_fs_write names no hf:// URI")
            repo = parse_uri(args[0], repo_only=True)
            facts = hub.facts(repo, repo.kind)
            return {"delta": {}, "requires": write_requires(must_reach(hub, facts.private, facts.owner)), "emits": []}
        case "create_repo":
            return create_repo_answer(hub, arguments, declared)
        case _:
            raise ValueError(f"{tool!r} is not a write this annotator labels")


def create_repo_answer(hub, arguments, declared):
    target = parse_repo_uri(arguments.get("uri"), "uri")
    private = arguments.get("private")
    if private is not None and not isinstance(private, bool):
        raise ValueError("private must be a boolean")
    source_uri = arguments.get("source_uri")
    if source_uri is None:
        # The Hub's default for a new repository is public.
        readers = must_reach(hub, bool(private), target.owner)
        return {"delta": {}, "requires": write_requires(readers), "emits": []}
    source = parse_repo_uri(source_uri, "source_uri")
    if source.kind != target.kind:
        raise ValueError("source_uri and uri must name the same repository type")
    facts = hub.facts(source, source.kind)
    source_readers = read_by(hub, facts, True, declared)
    target_private = facts.private if private is None else private
    target_readers = must_reach(hub, target_private, target.owner)
    if not (source_readers == PUBLIC or (source_readers == SELF and target_readers == SELF)):
        raise RuntimeError(
            f"copying {source.id()} (read by {source_readers}) into {target.id()} (read by {target_readers}) "
            "would widen its readers server-side"
        )
    return {"delta": {}, "requires": write_requires(target_readers), "emits": []}


def annotation(hub, name, tool, arguments, declared):
    match name:
        case "huggingface.repo-visibility":
            return read_answer(hub, tool, arguments, declared)
        case "huggingface.repo-readers":
            return write_answer(hub, tool, arguments, declared)
        case _:
            raise ValueError(f"unexpected annotator name {name!r}")


def main():
    raw = sys.stdin.buffer.read(MAX_INPUT_BYTES + 1)
    if len(raw) > MAX_INPUT_BYTES:
        raise ValueError("the consult is too large")
    consult = json.loads(raw)
    name, tool, arguments = call_of(consult)
    declared = check_declaration(consult)

    hub = Hub(hub_api(resolve_token()))
    json.dump({"version": 1, "answer": annotation(hub, name, tool, arguments, declared)}, sys.stdout)
    sys.stdout.write("\n")


if __name__ == "__main__":
    try:
        main()
    except Exception as error:
        print(f"huggingface repository annotator: {error}", file=sys.stderr)
        raise SystemExit(1)
