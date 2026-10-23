"""maiTerm Files helper for one ssh host (docs/file-tree.md section 15).

maiTerm's constant loader sends this file over stdin at every start and runs it from memory:
nothing is installed on the host and nothing is left behind (the remote trash under
~/.maiterm/trash is the human's own deleted files, kept so Undo works).

Frames, both ways: one JSON header line, then `len` raw bytes when the header has a `len`.
A reply carries its request's `id` and `ok`; an event carries `ev` instead. Listings, file
contents and git output travel as raw bytes, never base64.

Every path a request names is root-relative and checked here as maiTerm checks it locally:
no `..`, `.`, empty or absolute part, and a symlinked folder on the way must resolve inside
the root. The root itself is never acted on. Subprocesses take argv lists, never a shell.

Python 3.6 is the floor (maiTerm's tests parse this file and refuse anything newer). Paths are
bytes inside, so a host whose locale isn't UTF-8 still reaches every name.

The helper exits when its stdin ends, when its parent goes away, or after 60 s without a ping.
ssh without a terminal sends the far side no hangup, so it must notice by itself.
"""

import ctypes
import errno
import json
import os
import platform
import re
import select
import shutil
import socket
import stat
import struct
import subprocess
import sys
import threading
import time
from concurrent.futures import ThreadPoolExecutor
from concurrent.futures import wait as futures_wait

PROTOCOL = 1
MAX_HEADER = 1 << 20
# Rust shows 50,000 entries of a folder; past this many only the count is exact.
MAX_ENTRIES = 200000
MAX_FILES = 1000000
WALK_DEPTH = 20
# Exit when nothing has come from maiTerm for this long (its pings come every 30 s). A test
# shortens it; nothing else sets the variable.
PING_DEADLINE = float(os.environ.get("MAITERM_FT_PING_DEADLINE") or 60)
COALESCE = 0.15
MAX_EVENT_PATHS = 4096
BUILTIN_EXCLUDED = (b".git", b".DS_Store", b"Thumbs.db")
GIT_SUBCOMMANDS = ("status", "ls-files", "diff", "cat-file", "merge-base", "diff-tree", "rev-parse")
GIT_OUTPUT_CAP = 64 << 20
TRASH_KEEP_DAYS = 30

IN = sys.stdin.buffer
OUT = sys.stdout.buffer
OUT_LOCK = threading.Lock()
PARENT = os.getppid()
# When bytes last came in from maiTerm: a ping, any frame, or part of a long payload.
LAST_HEARD = [time.monotonic()]


def B(s):
    return s.encode("utf-8", "surrogateescape")


def S(b):
    return b.decode("utf-8", "replace")


def die():
    os._exit(0)


def send(header, payload=b""):
    if payload:
        header["len"] = len(payload)
    data = json.dumps(header, separators=(",", ":")).encode("ascii") + b"\n"
    with OUT_LOCK:
        try:
            OUT.write(data)
            if payload:
                OUT.write(payload)
            OUT.flush()
        except (OSError, ValueError):
            die()


class Fail(Exception):
    """A request that can't be done: `code` is one of gone, denied, exists, outside,
    too_large, changed, missing, timeout, no_trash, not_in_trash, io."""

    def __init__(self, code, msg):
        Exception.__init__(self, msg)
        self.code = code
        self.msg = msg


def os_fail(e, what):
    c = e.errno
    if c in (errno.ENOENT, errno.ENOTDIR):
        code = "gone"
    elif c in (errno.EACCES, errno.EPERM):
        code = "denied"
    elif c == errno.EEXIST:
        code = "exists"
    else:
        code = "io"
    return Fail(code, "%s: %s" % (what, e.strerror or str(e)))


# -- Paths ---------------------------------------------------------------------------------


def parts_of(rel):
    """The components of a root-relative path, refused if any could step outside."""
    if rel == "":
        return []
    if not isinstance(rel, str) or rel.startswith("/") or rel.endswith("/") or "\0" in rel:
        raise Fail("outside", "not a path inside the root: %r" % (rel,))
    parts = rel.split("/")
    for p in parts:
        if p in ("", ".", ".."):
            raise Fail("outside", "not a path inside the root: %r" % (rel,))
    return parts


def under(path, root):
    """`path` is `root` or inside it, component-wise (both bytes, real)."""
    if path == root:
        return True
    base = root.rstrip(b"/") + b"/"
    return path.startswith(base)


def contained(root, rel, item):
    """The path of `rel` under the real root `root`, refused when it could reach outside. With
    `item` the last component is the thing acted on, so a link there is never followed and
    the root itself is refused; otherwise it is a folder things go into and is followed."""
    parts = parts_of(rel)
    if not parts and item:
        raise Fail("outside", "Files won't act on the root folder itself")
    p = root
    for i, part in enumerate(parts):
        p = os.path.join(p, B(part))
        if i + 1 == len(parts) and item:
            break
        try:
            st = os.lstat(p)
        except OSError:
            break
        if stat.S_ISLNK(st.st_mode):
            target = os.path.realpath(p)
            if not os.path.exists(target):
                raise Fail("outside", "%s: a link on the way doesn't resolve" % rel)
            if not under(target, root):
                raise Fail("outside", "%s: a link on the way leads outside the root" % rel)
    return p


def join_rel(parent, name):
    return name if parent == "" else parent + "/" + name


def split_parent(rel):
    i = rel.rfind("/")
    if i < 0:
        return "", rel
    return rel[:i], rel[i + 1:]


def lexists(p):
    try:
        os.lstat(p)
        return True
    except OSError:
        return False


def expand(path):
    """An absolute or `~`-based remote path, as bytes, `~` being this host's home."""
    if not isinstance(path, str) or not (path.startswith("/") or path == "~" or path.startswith("~/")):
        raise Fail("outside", "not an absolute path: %r" % (path,))
    return os.path.expanduser(B(path))


# -- Repository and ignore rules ------------------------------------------------------------


def repo_info(real):
    """The repository `real` is in, found as git finds it: the nearest `.git` at or above it,
    a folder, or a `gitdir:` file (a linked worktree or submodule) with its `commondir`."""
    d = real
    while True:
        dot = os.path.join(d, b".git")
        git_dir = None
        try:
            st = os.lstat(dot)
            if stat.S_ISDIR(st.st_mode) or (stat.S_ISLNK(st.st_mode) and os.path.isdir(dot)):
                git_dir = os.path.realpath(dot)
            elif stat.S_ISREG(st.st_mode):
                with open(dot, "rb") as f:
                    for line in f.read(4096).splitlines():
                        if line.startswith(b"gitdir:"):
                            t = line[len(b"gitdir:"):].strip()
                            t = t if t.startswith(b"/") else os.path.join(d, t)
                            if os.path.isdir(t):
                                git_dir = os.path.realpath(t)
                            break
        except OSError:
            pass
        if git_dir is not None:
            common = git_dir
            try:
                with open(os.path.join(git_dir, b"commondir"), "rb") as f:
                    c = f.read(4096).strip()
                if c:
                    common = os.path.realpath(c if c.startswith(b"/") else os.path.join(git_dir, c))
            except OSError:
                pass
            top = os.path.realpath(d)
            if top == real:
                up = ""
            else:
                depth = len(real[len(top):].strip(b"/").split(b"/"))
                up = "/".join([".."] * depth)
            return {"toplevel_rel": up, "toplevel": S(top), "git_dir": S(git_dir), "common_dir": S(common)}
        parent = os.path.dirname(d)
        if parent == d:
            return None
        d = parent


def translate(pat):
    """A gitignore glob (no leading `/`, no trailing `/`) as a regex body, wildmatch rules:
    `*` and `?` never cross `/`, `**` as a whole component spans folders."""
    out = []
    i, n = 0, len(pat)
    while i < n:
        c = pat[i]
        if c == "*":
            j = i
            while j < n and pat[j] == "*":
                j += 1
            at_start = i == 0 or pat[i - 1] == "/"
            at_end = j == n or pat[j] == "/"
            if j - i >= 2 and at_start and at_end:
                if j == n:
                    out.append(".*")
                    i = j
                else:
                    out.append("(?:.*/)?")
                    i = j + 1
                continue
            out.append("[^/]*")
            i = j
        elif c == "?":
            out.append("[^/]")
            i += 1
        elif c == "[":
            j = i + 1
            if j < n and pat[j] in "!^":
                j += 1
            if j < n and pat[j] == "]":
                j += 1
            while j < n and pat[j] != "]":
                j += 1
            if j >= n:
                out.append("\\[")
                i += 1
                continue
            inner = pat[i + 1:j]
            neg = inner[:1] in ("!", "^")
            if neg:
                inner = inner[1:]
            inner = inner.replace("\\", "\\\\").replace("/", "")
            if not inner:
                return None
            out.append("[" + ("^/" if neg else "") + inner + "]")
            i = j + 1
        elif c == "\\":
            if i + 1 >= n:
                return None
            out.append(re.escape(pat[i + 1]))
            i += 2
        else:
            out.append(re.escape(c))
            i += 1
    return "".join(out)


def compile_pattern(line):
    """One gitignore line: (regex, negated, folders only), or None for a blank, a comment or
    a pattern git would not use."""
    line = line.rstrip("\r")
    if not line or line.startswith("#"):
        return None
    # Trailing spaces go unless escaped.
    end = len(line)
    while end > 0 and line[end - 1] == " " and not (end >= 2 and line[end - 2] == "\\"):
        end -= 1
    line = line[:end]
    if line.endswith("\\ "):
        line = line[:-2] + " "
    if not line:
        return None
    neg = False
    if line.startswith("!"):
        neg = True
        line = line[1:]
    elif line.startswith("\\!") or line.startswith("\\#"):
        line = line[1:]
    dir_only = line.endswith("/")
    line = line.rstrip("/")
    if not line:
        return None
    anchored = "/" in line
    line = line.lstrip("/")
    body = translate(line)
    if body is None:
        return None
    try:
        rx = re.compile(("^" if anchored else "^(?:.*/)?") + body + "$", re.S)
    except re.error:
        return None
    return (rx, neg, dir_only)


class Gitignore(object):
    def __init__(self, lines):
        self.pats = [p for p in (compile_pattern(l) for l in lines) if p is not None]

    def matched(self, path, is_dir):
        """True ignored, False re-included (`!`), None undecided. The last match wins."""
        for rx, neg, dir_only in reversed(self.pats):
            if dir_only and not is_dir:
                continue
            if rx.match(path):
                return not neg
        return None


def load_ignore(path):
    try:
        with open(path, "rb") as f:
            data = f.read(1 << 20)
    except OSError:
        return None
    g = Gitignore(S(data).split("\n"))
    return g if g.pats else None


GLOBAL_EXCLUDES = []


def global_excludes():
    """`core.excludesFile` (read once), else git's XDG default."""
    if GLOBAL_EXCLUDES:
        return GLOBAL_EXCLUDES[0]
    path = None
    try:
        r = subprocess.run(["git", "config", "--global", "--get", "core.excludesfile"], stdin=subprocess.DEVNULL,
                           stdout=subprocess.PIPE, stderr=subprocess.DEVNULL, timeout=5)
        v = r.stdout.strip()
        if r.returncode == 0 and v:
            path = os.path.expanduser(v)
    except (OSError, subprocess.SubprocessError):
        pass
    if path is None:
        xdg = os.environ.get("XDG_CONFIG_HOME")
        base = B(xdg) if xdg else os.path.expanduser(b"~/.config")
        path = os.path.join(base, b"git", b"ignore")
    GLOBAL_EXCLUDES.append(load_ignore(path))
    return GLOBAL_EXCLUDES[0]


def rel_from(base, rel):
    if base == "":
        return rel
    if rel.startswith(base + "/"):
        return rel[len(base) + 1:]
    return rel


class Ignore(object):
    """Which entries git ignores, as maiTerm's ignore_cache.rs decides it: one matcher per
    folder's .gitignore, stacked root to folder, then the repository's info/exclude, then
    core.excludesFile, and only inside a repository; the user's excludes always."""

    def __init__(self, real, git_dir, excludes):
        self.real = real
        self.root_in_repo = git_dir is not None
        self.git_dir = git_dir
        self.root_exclude = load_ignore(os.path.join(git_dir, b"info", b"exclude")) if git_dir else None
        self.glob = global_excludes()
        self.user = Gitignore(excludes) if excludes else None
        if self.user is not None and not self.user.pats:
            self.user = None
        self.folders = {}
        self.dir_ignored = {}

    def folder(self, rel):
        f = self.folders.get(rel)
        if f is None:
            p = self.real if rel == "" else os.path.join(self.real, B(rel))
            dot = os.path.join(p, b".git")
            repo_root = rel != "" and lexists(dot)
            info = load_ignore(os.path.join(dot, b"info", b"exclude")) if repo_root and os.path.isdir(dot) else None
            f = (load_ignore(os.path.join(p, b".gitignore")), repo_root, info)
            self.folders[rel] = f
        return f

    def matcher(self, d, ignored_dir):
        chain = []
        cur = d
        repo_base = None
        while True:
            gi, repo_root, _ = self.folder(cur)
            chain.append(cur)
            if repo_root:
                repo_base = cur
                break
            if cur == "":
                break
            cur = split_parent(cur)[0]
        in_repo = repo_base is not None or self.root_in_repo
        stack = []
        if in_repo:
            for f in chain:
                gi = self.folders[f][0]
                if gi is not None:
                    stack.append((f, gi))
        base = repo_base or ""
        if not in_repo:
            excl = None
        elif repo_base is not None:
            excl = self.folders[repo_base][2]
        else:
            excl = self.root_exclude
        if excl is not None:
            stack.append((base, excl))
        if in_repo and self.glob is not None:
            stack.append((base, self.glob))
        return (d, ignored_dir, stack)

    def is_ignored_with(self, m, name, is_dir):
        d, ignored_dir, stack = m
        if ignored_dir:
            return True
        if self.user is None and not stack:
            return False
        path = join_rel(d, name)
        if self.user is not None and self.user.matched(path, is_dir) is True:
            return True
        for base, gi in stack:
            v = gi.matched(rel_from(base, path), is_dir)
            if v is not None:
                return v
        return False

    def for_dir(self, d):
        return self.matcher(d, self.dir_is_ignored(d))

    def is_ignored(self, d, name, is_dir):
        return self.is_ignored_with(self.for_dir(d), name, is_dir)

    def dir_is_ignored(self, rel):
        if rel == "":
            return False
        v = self.dir_ignored.get(rel)
        if v is not None:
            return v
        parent, name = split_parent(rel)
        v = self.dir_is_ignored(parent) or self.is_ignored_with(self.matcher(parent, False), name, True)
        self.dir_ignored[rel] = v
        return v

    def invalidate_folder(self, rel):
        self.folders.pop(rel, None)
        self.dir_ignored = {}

    def invalidate_all(self):
        self.folders = {}
        self.dir_ignored = {}
        self.root_exclude = load_ignore(os.path.join(self.git_dir, b"info", b"exclude")) if self.git_dir else None


class Root(object):
    """One root a hub shows: its real path, repository and ignore rules."""

    def __init__(self, real, excludes):
        self.real = real
        self.lock = threading.Lock()
        self.repo = repo_info(real)
        git_dir = B(self.repo["git_dir"]) if self.repo else None
        self.excludes = list(excludes)
        self.ignore = Ignore(real, git_dir, self.excludes)


ROOTS = {}
ROOTS_LOCK = threading.Lock()


def get_root(name, excludes=None):
    real = os.path.realpath(expand(name))
    if not os.path.isdir(real):
        raise Fail("gone", "This folder no longer exists")
    with ROOTS_LOCK:
        r = ROOTS.get(real)
        if r is None or (excludes is not None and excludes != r.excludes):
            r = Root(real, excludes if excludes is not None else (r.excludes if r else []))
            ROOTS[real] = r
        return r


# -- Listing and stat ----------------------------------------------------------------------


def classify_link(path, dir_real):
    try:
        st = os.stat(path)
    except OSError:
        return "l", 0
    if stat.S_ISDIR(st.st_mode):
        if dir_real[0] is None:
            dir_real[0] = os.path.realpath(os.path.dirname(path))
        target = os.path.realpath(path)
        return "L", (4 if under(dir_real[0], target) else 0)
    return "l", 0


def read_dir(d):
    """(name bytes, kind, flags) for every entry but the built-in excluded, unsorted."""
    out = []
    dir_real = [None]
    try:
        it = os.scandir(d)
    except OSError as e:
        raise os_fail(e, S(d))
    with it:
        for e in it:
            name = e.name
            if name in BUILTIN_EXCLUDED:
                continue
            f = 2 if name.startswith(b".") else 0
            try:
                if e.is_symlink():
                    k, lf = classify_link(e.path, dir_real)
                    f |= lf
                elif e.is_dir(follow_symlinks=False):
                    k = "d"
                elif e.is_file(follow_symlinks=False):
                    k = "f"
                else:
                    k = "o"
            except OSError as err:
                k = "o"
                if err.errno in (errno.EACCES, errno.EPERM):
                    f |= 8
            out.append((name, k, f))
            if len(out) >= MAX_ENTRIES:
                break
    return out


def op_list(req):
    root = get_root(req["root"])
    rel = req.get("rel", "")
    d = contained(root.real, rel, False)
    entries = read_dir(d)
    total = len(entries)
    with root.lock:
        m = root.ignore.for_dir(rel)
        payload = bytearray()
        for name, k, f in entries:
            if root.ignore.is_ignored_with(m, S(name), k in ("d", "L")):
                f |= 1
            payload += k.encode("ascii") + bytes([48 + f]) + name + b"\0"
    return {"total": total}, bytes(payload)


def op_stat(req):
    root = get_root(req["root"])
    p = contained(root.real, req.get("rel", ""), True)
    try:
        lst = os.lstat(p)
    except OSError as e:
        raise os_fail(e, req.get("rel", ""))
    link = None
    st = lst
    if stat.S_ISLNK(lst.st_mode):
        try:
            link = S(os.readlink(p))
        except OSError:
            link = None
        try:
            st = os.stat(p)
        except OSError:
            st = lst
    is_file = stat.S_ISREG(st.st_mode)
    return {
        "size": st.st_size,
        "mtime_ms": st.st_mtime_ns // 1000000,
        "exec": bool(is_file and st.st_mode & 0o111),
        "file": is_file,
        "dir": stat.S_ISDIR(st.st_mode),
        "link": link,
    }, b""


def op_probe(req):
    """For each candidate folder: its real path, whether it is a folder, and the top level of
    the repository it is in. What the resolver asks before picking an ssh tab's root."""
    items = []
    for path in req.get("paths", []):
        try:
            real = os.path.realpath(expand(path))
        except Fail:
            items.append({"real": None, "dir": False, "top": None})
            continue
        is_dir = os.path.isdir(real)
        info = repo_info(real) if is_dir else None
        items.append({"real": S(real), "dir": is_dir, "top": info["toplevel"] if info else None})
    return {"items": items}, b""


def op_root(req):
    """Open (or re-open after a reconnect) a root: its real path and repository, and the
    user's excludes for its listings."""
    root = get_root(req["root"], list(req.get("excludes", [])))
    return {"real": S(root.real), "repo": root.repo}, b""


def op_read(req):
    root = get_root(req["root"])
    p = contained(root.real, req["rel"], True)
    cap = int(req.get("cap", 2 << 20))
    try:
        with open(p, "rb") as f:
            data = f.read(cap + 1)
    except OSError as e:
        raise os_fail(e, req["rel"])
    if len(data) > cap:
        raise Fail("too_large", "%s is larger than %d bytes" % (req["rel"], cap))
    return {}, data


def op_write(req, payload):
    """Replace a file's content: a temporary file in the same folder, synced, then renamed
    over it, keeping its mode. With `expect` ({mtime_ms, size}) the file must still be what
    the writer last read."""
    root = get_root(req["root"])
    rel = req["rel"]
    p = contained(root.real, rel, True)
    mode = None
    try:
        st = os.stat(p)
        if not stat.S_ISREG(st.st_mode):
            raise Fail("io", "%s isn't a file" % rel)
        mode = stat.S_IMODE(st.st_mode)
        exp = req.get("expect")
        if exp and (st.st_size != exp.get("size") or st.st_mtime_ns // 1000000 != exp.get("mtime_ms")):
            raise Fail("changed", "%s changed on disk since it was read" % rel)
    except OSError as e:
        if e.errno != errno.ENOENT:
            raise os_fail(e, rel)
        if req.get("expect"):
            raise Fail("gone", "%s no longer exists" % rel)
    d, name = os.path.split(p)
    tmp = os.path.join(d, b"." + name + b".maiterm-" + B(str(os.getpid())) + b"-" + B(str(threading.get_ident())) + b".tmp")
    try:
        fd = os.open(tmp, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600 if mode is None else mode)
        try:
            view = memoryview(payload)
            while view:
                n = os.write(fd, view)
                view = view[n:]
            os.fsync(fd)
        finally:
            os.close(fd)
        if mode is not None:
            os.chmod(tmp, mode)
        else:
            os.chmod(tmp, 0o666 & ~current_umask())
        os.rename(tmp, p)
    except OSError as e:
        try:
            os.unlink(tmp)
        except OSError:
            pass
        raise os_fail(e, rel)
    st = os.stat(p)
    return {"size": st.st_size, "mtime_ms": st.st_mtime_ns // 1000000}, b""


def current_umask():
    m = os.umask(0o22)
    os.umask(m)
    return m


# -- git ----------------------------------------------------------------------------------


def git_env():
    env = dict(os.environ)
    env["GIT_OPTIONAL_LOCKS"] = "0"
    env["GIT_TERMINAL_PROMPT"] = "0"
    env["LC_ALL"] = "C"
    env["GIT_SSH_COMMAND"] = "ssh -o BatchMode=yes -o ConnectTimeout=10"
    return env


def run_git(real, args, timeout):
    try:
        return subprocess.run([b"git", b"--no-optional-locks", b"-C", real] + [B(a) for a in args], stdin=subprocess.DEVNULL,
                              stdout=subprocess.PIPE, stderr=subprocess.PIPE, env=git_env(), timeout=timeout)
    except subprocess.TimeoutExpired:
        raise Fail("timeout", "git timed out")
    except OSError as e:
        if e.errno == errno.ENOENT:
            raise Fail("missing", "git isn't installed")
        raise os_fail(e, "git")


def op_git(req):
    """One git command in the root, from a fixed set of read-only subcommands."""
    root = get_root(req["root"])
    args = [str(a) for a in req.get("args", [])]
    if not args or args[0] not in GIT_SUBCOMMANDS:
        raise Fail("denied", "git %s isn't something Files runs" % (args[0] if args else ""))
    r = run_git(root.real, args, float(req.get("timeout", 10)))
    out = r.stdout
    if len(out) > GIT_OUTPUT_CAP:
        raise Fail("too_large", "git printed more than Files reads")
    return {"code": r.returncode, "stderr": S(r.stderr[-4096:])}, out


def op_files(req):
    """Every file Find should know: `git ls-files` in a repository (deleted-from-worktree paths
    left out, skip-worktree ones only when on disk), else a walk that skips `.git` and what
    the ignore rules hide."""
    root = get_root(req["root"])
    if root.repo is not None:
        try:
            r = run_git(root.real, ["ls-files", "-cdo", "-t", "--exclude-standard", "-z"], 30)
            if r.returncode == 0:
                return {"via": "git"}, b"\0".join(ls_files_tagged(root.real, r.stdout))
        except Fail:
            pass
    return {"via": "walk"}, b"\0".join(walk(root))


def ls_files_tagged(real, out):
    removed = set()
    listed = []
    for rec in out.split(b"\0"):
        if len(rec) < 3 or rec[1:2] != b" ":
            continue
        tag, path = rec[0:1], rec[2:]
        if tag == b"R":
            removed.add(path)
        elif tag == b"S":
            if lexists(os.path.join(real, path)) and not os.path.isdir(os.path.join(real, path)):
                listed.append(path)
        else:
            listed.append(path)
    seen = set()
    result = []
    for p in listed:
        if p in removed or p in seen:
            continue
        seen.add(p)
        result.append(p)
        if len(result) >= MAX_FILES:
            break
    return result


def walk(root):
    out = []
    stack = [("", root.real, 0)]
    while stack and len(out) < MAX_FILES:
        rel, d, depth = stack.pop()
        try:
            entries = read_dir(d)
        except Fail:
            continue
        with root.lock:
            m = root.ignore.for_dir(rel)
            keep = [(n, k) for n, k, _ in entries if not root.ignore.is_ignored_with(m, S(n), k == "d")]
        for name, k in keep:
            child = join_rel(rel, S(name)) if rel else S(name)
            if k == "d":
                if depth + 1 < WALK_DEPTH:
                    stack.append((child, os.path.join(d, name), depth + 1))
            elif k in ("f", "l", "L"):
                out.append(B(child))
    out.sort()
    return out


# -- File operations ----------------------------------------------------------------------


COPY_STYLES = {
    "mac": (" copy", " copy %d"),
    "windows": (" - Copy", " - Copy (%d)"),
    "linux": (" (copy)", " (copy %d)"),
}


def stem_ext(name, is_dir):
    if is_dir:
        return name, ""
    i = name.rfind(".")
    if i > 0:
        return name[:i], name[i:]
    return name, ""


def unique_name(name, is_dir, style, fresh, taken):
    if not fresh and not taken(name):
        return name
    stem, ext = stem_ext(name, is_dir)
    first, nth = COPY_STYLES.get(style, COPY_STYLES["linux"])
    for n in range(1, 1000):
        cand = stem + (first if n == 1 else nth % n) + ext
        if not taken(cand):
            return cand
    return None


def landing(d, name, is_dir, conflict, fresh, style):
    """("ok", name) | ("skip",) | ("exists",): the name an item lands under in folder `d`."""
    if not fresh and not lexists(os.path.join(d, B(name))):
        return ("ok", name)
    if fresh or conflict == "keep_both":
        n = unique_name(name, is_dir, style, fresh, lambda c: lexists(os.path.join(d, B(c))))
        return ("ok", n) if n is not None else ("exists",)
    if conflict == "skip":
        return ("skip",)
    return ("exists",)


LIBC = []


def libc():
    if not LIBC:
        try:
            LIBC.append(ctypes.CDLL(None, use_errno=True))
        except OSError:
            LIBC.append(None)
    return LIBC[0]


RENAME_NOREPLACE = 1
AT_FDCWD = -100
RENAMEAT2_SYSCALL = {"x86_64": 316, "aarch64": 276, "armv7l": 382, "armv6l": 382, "i686": 353, "i386": 353,
                     "ppc64le": 357, "s390x": 347, "riscv64": 276}


def rename_noreplace(src, dst):
    """A rename that never replaces what is at `dst`: renameat2(RENAME_NOREPLACE) where the
    kernel and filesystem have it, else check-then-rename (an old kernel, an NFS mount)."""
    lc = libc()
    if lc is not None and sys.platform.startswith("linux"):
        fn = getattr(lc, "renameat2", None)
        if fn is not None:
            rc = fn(AT_FDCWD, src, AT_FDCWD, dst, RENAME_NOREPLACE)
        else:
            nr = RENAMEAT2_SYSCALL.get(platform.machine())
            rc = lc.syscall(nr, AT_FDCWD, src, AT_FDCWD, dst, RENAME_NOREPLACE) if nr is not None else None
        if rc == 0:
            return
        if rc is not None:
            err = ctypes.get_errno()
            # Only "this kernel or filesystem can't" falls through to the check.
            if err not in (errno.EINVAL, errno.ENOSYS, getattr(errno, "ENOTSUP", errno.EINVAL)):
                raise OSError(err, os.strerror(err))
    if lexists(dst):
        raise OSError(errno.EEXIST, os.strerror(errno.EEXIST))
    os.rename(src, dst)


def real_dir(p):
    try:
        st = os.lstat(p)
    except OSError:
        return None
    return os.path.realpath(p) if stat.S_ISDIR(st.st_mode) else None


def inside_itself(src, dest):
    s = real_dir(src)
    return s is not None and under(os.path.realpath(dest), s)


def copy_tree(src, dst, made):
    """Copy without following links: a link is recreated as a link; nothing at `dst` is ever
    replaced (O_EXCL, mkdir). `made[0]` turns true once the top item exists."""
    st = os.lstat(src)
    if stat.S_ISLNK(st.st_mode):
        os.symlink(os.readlink(src), dst)
        made[0] = True
    elif stat.S_ISDIR(st.st_mode):
        os.mkdir(dst, stat.S_IMODE(st.st_mode) | 0o700)
        made[0] = True
        for e in os.scandir(src):
            copy_tree(e.path, os.path.join(dst, e.name), [False])
        os.chmod(dst, stat.S_IMODE(st.st_mode))
    elif stat.S_ISREG(st.st_mode):
        fd = os.open(dst, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
        made[0] = True
        with os.fdopen(fd, "wb") as out, open(src, "rb") as inp:
            shutil.copyfileobj(inp, out, 1 << 20)
        shutil.copystat(src, dst, follow_symlinks=False)
    else:
        raise OSError(errno.EINVAL, "only files, folders and links are copied")


def copy_fresh(src, dst):
    """copy_tree, removing only what it made when it fails: something another writer put at
    `dst` first (the O_EXCL or mkdir refused) is left alone."""
    made = [False]
    try:
        copy_tree(src, dst, made)
    except OSError:
        if made[0]:
            try:
                remove_any(dst)
            except OSError:
                pass
        raise


def remove_any(p):
    st = os.lstat(p)
    if stat.S_ISDIR(st.st_mode):
        shutil.rmtree(p)
    else:
        os.unlink(p)


def tally(p):
    """(files, folders, bytes) under `p`, links counted, never followed."""
    st = os.lstat(p)
    if not stat.S_ISDIR(st.st_mode):
        return 1, 0, st.st_size
    files, dirs, size = 0, 1, 0
    for e in os.scandir(p):
        f, d, s = tally(e.path)
        files += f
        dirs += d
        size += s
    return files, dirs, size


def move_across(src, dst):
    """A move to another filesystem: copy, check the copy, then remove the source."""
    copy_fresh(src, dst)
    if tally(src)[:2] != tally(dst)[:2]:
        remove_any(dst)
        raise OSError(errno.EIO, "the copy didn't match the original")
    remove_any(src)


def outermost(rels):
    out = []
    for i, r in enumerate(rels):
        inside = any(j != i and o != r and (r.startswith(o + "/")) for j, o in enumerate(rels))
        if not inside and r not in out:
            out.append(r)
    return out


def trash_home():
    return os.path.join(os.path.expanduser(b"~"), b".maiterm", b"trash")


def sweep_trash(home):
    cutoff = time.time() - TRASH_KEEP_DAYS * 86400
    try:
        for e in os.scandir(home):
            try:
                if e.is_dir(follow_symlinks=False) and e.stat(follow_symlinks=False).st_mtime < cutoff:
                    shutil.rmtree(e.path)
            except OSError:
                pass
    except OSError:
        pass


TRASH_SEQ = [0]
TRASH_LOCK = threading.Lock()


def trash_paths(paths):
    """Move each path into a fresh `~/.maiterm/trash/<ms>-<pid>-<n>/`, recording where it came
    from and when, so Undo can put it back. Per item: None, or why it couldn't go."""
    home = trash_home()
    with TRASH_LOCK:
        TRASH_SEQ[0] += 1
        seq = TRASH_SEQ[0]
    box = os.path.join(home, B("%d-%d-%d" % (int(time.time() * 1000), os.getpid(), seq)))
    try:
        os.makedirs(box, 0o700)
    except OSError as e:
        return ["the trash at ~/.maiterm/trash can't be made: %s" % e.strerror] * len(paths)
    sweep_trash(home)
    now = time.time()
    record = []
    results = []
    for i, p in enumerate(paths):
        dst = os.path.join(box, B(str(i)))
        try:
            os.rename(p, dst)
            record.append({"p": p.hex(), "t": now, "n": str(i)})
            results.append(None)
        except OSError as e:
            if e.errno == errno.EXDEV:
                results.append("it is on another disk than the host's trash (~/.maiterm/trash)")
            else:
                results.append(e.strerror or str(e))
    if record:
        with open(os.path.join(box, b"items.json"), "w") as f:
            json.dump(record, f)
    else:
        try:
            os.rmdir(box)
        except OSError:
            pass
    return results


def restore_path(p, since):
    """Put back the newest trashed item that was at `p`, trashed at or after `since`."""
    home = trash_home()
    want = p.hex()
    best = None
    try:
        boxes = list(os.scandir(home))
    except OSError:
        boxes = []
    for b in boxes:
        try:
            with open(os.path.join(b.path, b"items.json")) as f:
                items = json.load(f)
        except (OSError, ValueError):
            continue
        for it in items:
            if it.get("p") == want and it.get("t", 0) >= since and (best is None or it["t"] > best[1]["t"]):
                best = (b.path, it)
    if best is None:
        raise Fail("not_in_trash", "It is no longer in the trash")
    box, it = best
    src = os.path.join(box, B(it["n"]))
    try:
        rename_noreplace(src, p)
    except OSError as e:
        raise os_fail(e, S(p))
    try:
        with open(os.path.join(box, b"items.json")) as f:
            items = json.load(f)
        items = [x for x in items if x.get("n") != it["n"]]
        if items:
            with open(os.path.join(box, b"items.json"), "w") as f:
                json.dump(items, f)
        else:
            shutil.rmtree(box)
    except (OSError, ValueError):
        pass


def fail_item(rel, code, msg):
    return {"rel": rel, "code": code, "msg": msg}


def trash_undo(rels):
    return {"kind": "trash", "src": rels} if rels else None


class Op(object):
    """The whole FileOp, run as maiTerm runs it locally (ops.rs), with the same result shape.
    The tickets and the live-folder guard are maiTerm's, checked before this runs."""

    def __init__(self, req):
        self.root = get_root(req["root"]).real
        self.op = req["file"]
        self.style = req.get("style", "linux")
        c = self.op.get("conflict") or "fail"
        if c not in ("fail", "keep_both", "skip"):
            raise Fail("io", "Unknown conflict rule: %s" % c)
        self.conflict = c
        self.done, self.failed, self.skipped = [], [], []
        self.undo = None
        self.needs = None

    def result(self):
        return {"done": self.done, "failed": self.failed, "skipped": self.skipped, "undo": self.undo, "needs": self.needs}

    def dest_folder(self, rel):
        rel = rel or ""
        p = contained(self.root, rel, False)
        if not os.path.isdir(p):
            raise Fail("io", "%s: not a folder" % (rel or "the root"))
        return rel, p

    def items(self, src):
        if not src:
            raise Fail("io", "No items given")
        return [(r, contained(self.root, r, True)) for r in outermost(src)]

    def run(self):
        kind = self.op.get("kind")
        fn = {
            "create_file": lambda: self.create(False),
            "create_dir": lambda: self.create(True),
            "rename": self.rename,
            "move": self.move,
            "copy": self.copy,
            "duplicate": self.duplicate,
            "trash": self.trash,
            "delete": self.delete,
            "restore": self.restore,
        }.get(kind)
        if fn is None:
            raise Fail("io", "Unknown file operation: %s" % kind)
        fn()
        return self.result()

    def create(self, is_dir):
        parent_rel, parent = self.dest_folder(self.op.get("dest"))
        name = self.op.get("name") or ""
        if not name:
            raise Fail("io", "A name is needed")
        parts = parts_of(name)
        first_new = None
        for part in parts[:-1]:
            rel = join_rel(parent_rel, part)
            p = os.path.join(parent, B(part))
            try:
                os.mkdir(p)
                if first_new is None:
                    first_new = rel
            except OSError as e:
                if e.errno == errno.EEXIST:
                    contained(self.root, rel, False)
                    if not os.path.isdir(p):
                        self.failed.append(fail_item(rel, "exists", "%s exists and isn't a folder" % rel))
                        self.undo = trash_undo([first_new] if first_new else [])
                        return
                else:
                    self.failed.append(fail_item(rel, "io", e.strerror or str(e)))
                    self.undo = trash_undo([first_new] if first_new else [])
                    return
            parent_rel, parent = rel, p
        leaf = parts[-1]
        land = landing(parent, leaf, is_dir, self.conflict, False, self.style)
        if land[0] == "skip":
            self.skipped.append(join_rel(parent_rel, leaf))
        elif land[0] == "exists":
            self.failed.append(fail_item(join_rel(parent_rel, leaf), "exists", "Something with that name is already there"))
        else:
            rel = join_rel(parent_rel, land[1])
            p = os.path.join(parent, B(land[1]))
            try:
                if is_dir:
                    os.mkdir(p)
                else:
                    os.close(os.open(p, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o666))
                if first_new is None:
                    first_new = rel
                self.done.append({"from": rel, "to": None})
            except OSError as e:
                code = "exists" if e.errno == errno.EEXIST else "io"
                self.failed.append(fail_item(rel, code, "Something with that name is already there" if code == "exists" else e.strerror))
        self.undo = trash_undo([first_new] if first_new else [])

    def rename(self):
        src = self.op.get("src") or []
        if len(src) != 1:
            raise Fail("io", "Rename takes exactly one item")
        rel = src[0]
        frm = contained(self.root, rel, True)
        name = self.op.get("name") or ""
        if not name:
            raise Fail("io", "A name is needed")
        if len(parts_of(name)) != 1:
            raise Fail("io", "“%s”: a name can't contain /" % name)
        parent_rel, old = split_parent(rel)
        try:
            st = os.lstat(frm)
        except OSError:
            self.failed.append(fail_item(rel, "gone", "It is no longer there"))
            return
        if name == old:
            self.skipped.append(rel)
            return
        parent = os.path.dirname(frm)
        land = landing(parent, name, stat.S_ISDIR(st.st_mode), self.conflict, False, self.style)
        if land[0] == "skip":
            self.skipped.append(rel)
            return
        if land[0] == "exists":
            self.failed.append(fail_item(rel, "exists", "“%s” is already there" % name))
            return
        new_rel = join_rel(parent_rel, land[1])
        try:
            rename_noreplace(frm, os.path.join(parent, B(land[1])))
        except OSError as e:
            code = "exists" if e.errno == errno.EEXIST else "io"
            self.failed.append(fail_item(rel, code, "“%s” is already there" % land[1] if code == "exists" else e.strerror))
            return
        self.done.append({"from": rel, "to": new_rel})
        self.undo = {"kind": "rename", "src": [new_rel], "name": old}

    def move(self):
        plan = []
        to = self.op.get("to") or []
        src = self.op.get("src") or []
        if not to:
            dest_rel, dest = self.dest_folder(self.op.get("dest"))
            for rel, p in self.items(src):
                plan.append((rel, p, dest_rel, dest, split_parent(rel)[1]))
        else:
            if len(to) != len(src):
                raise Fail("io", "A move's `to` names one destination per item")
            for rel, t in zip(src, to):
                p = contained(self.root, rel, True)
                contained(self.root, t, True)
                d_rel, name = split_parent(t)
                d_rel, d = self.dest_folder(d_rel)
                plan.append((rel, p, d_rel, d, name))
        moved, back = [], []
        for rel, frm, dest_rel, dest, name in plan:
            try:
                st = os.lstat(frm)
            except OSError:
                self.failed.append(fail_item(rel, "gone", "It is no longer there"))
                continue
            if inside_itself(frm, dest):
                self.failed.append(fail_item(rel, "inside_itself", "A folder can't be moved into itself"))
                continue
            src_parent, src_name = split_parent(rel)
            if src_parent == dest_rel and src_name == name:
                self.skipped.append(rel)
                continue
            land = landing(dest, name, stat.S_ISDIR(st.st_mode), self.conflict, False, self.style)
            if land[0] == "skip":
                self.skipped.append(rel)
                continue
            if land[0] == "exists":
                self.failed.append(fail_item(rel, "exists", "“%s” is already there" % name))
                continue
            dst = os.path.join(dest, B(land[1]))
            try:
                try:
                    rename_noreplace(frm, dst)
                except OSError as e:
                    if e.errno != errno.EXDEV:
                        raise
                    move_across(frm, dst)
            except OSError as e:
                code = "exists" if e.errno == errno.EEXIST else "io"
                self.failed.append(fail_item(rel, code, "“%s” is already there" % land[1] if code == "exists" else e.strerror))
                continue
            new_rel = join_rel(dest_rel, land[1])
            moved.append(new_rel)
            back.append(rel)
            self.done.append({"from": rel, "to": new_rel})
        if moved:
            self.undo = {"kind": "move", "src": moved, "to": back}

    def copy_one(self, label, frm, dest_rel, dest, conflict, fresh):
        try:
            st = os.lstat(frm)
        except OSError:
            self.failed.append(fail_item(label, "gone", "It is no longer there"))
            return
        if inside_itself(frm, dest):
            self.failed.append(fail_item(label, "inside_itself", "A folder can't be copied into itself"))
            return
        name = S(os.path.basename(frm))
        land = landing(dest, name, stat.S_ISDIR(st.st_mode), conflict, fresh, self.style)
        if land[0] == "skip":
            self.skipped.append(label)
            return
        if land[0] == "exists":
            self.failed.append(fail_item(label, "exists", "“%s” is already there" % name))
            return
        try:
            copy_fresh(frm, os.path.join(dest, B(land[1])))
        except OSError as e:
            self.failed.append(fail_item(label, "exists" if e.errno == errno.EEXIST else "io", e.strerror or str(e)))
            return
        self.done.append({"from": label, "to": join_rel(dest_rel, land[1])})

    def copy(self):
        dest_rel, dest = self.dest_folder(self.op.get("dest"))
        for rel, p in self.items(self.op.get("src") or []):
            self.copy_one(rel, p, dest_rel, dest, self.conflict, False)
        self.undo = trash_undo([d["to"] for d in self.done])

    def duplicate(self):
        for rel, p in self.items(self.op.get("src") or []):
            parent_rel, parent = self.dest_folder(split_parent(rel)[0])
            self.copy_one(rel, p, parent_rel, parent, "keep_both", True)
        self.undo = trash_undo([d["to"] for d in self.done])

    def trash(self):
        items = self.items(self.op.get("src") or [])
        present = []
        for rel, p in items:
            if lexists(p):
                present.append((rel, p))
            else:
                self.failed.append(fail_item(rel, "gone", "It is no longer there"))
        since = int(time.time())
        no_trash = []
        for (rel, _), why in zip(present, trash_paths([p for _, p in present])):
            if why is None:
                self.done.append({"from": rel, "to": None})
            else:
                no_trash.append(rel)
                self.failed.append(fail_item(rel, "no_trash", why))
        if no_trash:
            what = "This item" if len(no_trash) == 1 else "These items"
            self.needs = {"code": "no_trash", "detail": "%s can't go to the trash here. Delete permanently?" % what, "tabs": []}
        trashed = [d["from"] for d in self.done]
        if trashed:
            self.undo = {"kind": "restore", "src": trashed, "trashed_at": since}

    def delete(self):
        for rel, p in self.items(self.op.get("src") or []):
            try:
                remove_any(p)
                self.done.append({"from": rel, "to": None})
            except OSError as e:
                if e.errno == errno.ENOENT:
                    self.failed.append(fail_item(rel, "gone", "It is no longer there"))
                else:
                    self.failed.append(fail_item(rel, "io", e.strerror or str(e)))

    def restore(self):
        since = self.op.get("trashed_at")
        if since is None:
            raise Fail("io", "A restore needs the time the items were trashed")
        for rel, p in self.items(self.op.get("src") or []):
            if lexists(p):
                self.failed.append(fail_item(rel, "exists", "Something else now has its name"))
                continue
            try:
                restore_path(p, since)
                self.done.append({"from": rel, "to": None})
            except Fail as f:
                self.failed.append(fail_item(rel, f.code, f.msg))
        self.undo = trash_undo([d["from"] for d in self.done])


def op_fileop(req):
    return Op(req).run(), b""


def op_tally(req):
    """What permanently deleting these items would remove (the ConfirmStrip's counts)."""
    root = get_root(req["root"])
    files, dirs = 0, 0
    for rel in outermost(req.get("rels", [])):
        p = contained(root.real, rel, True)
        try:
            f, d, _ = tally(p)
        except OSError as e:
            raise os_fail(e, rel)
        files += f
        dirs += d
    return {"files": files, "dirs": dirs}, b""


# Import from the client's disk arrives as primitives: maiTerm walks its own files and sends
# them one by one, each refusing to replace anything.


def op_land(req):
    root = get_root(req["root"])
    dest = contained(root.real, req.get("dest", ""), False)
    if not os.path.isdir(dest):
        raise Fail("io", "%s: not a folder" % (req.get("dest") or "the root"))
    land = landing(dest, req["name"], bool(req.get("is_dir")), req.get("conflict") or "fail", False, req.get("style", "linux"))
    if land[0] == "ok":
        return {"name": land[1]}, b""
    return {"name": None, "skip": land[0] == "skip"}, b""


def op_mkdir(req):
    root = get_root(req["root"])
    p = contained(root.real, req["rel"], True)
    try:
        os.mkdir(p)
    except OSError as e:
        raise os_fail(e, req["rel"])
    return {}, b""


def write_all(fd, payload):
    view = memoryview(payload)
    while view:
        n = os.write(fd, view)
        view = view[n:]


def op_put(req, payload):
    """A new file, never replacing anything. `more`: the rest follows in `append` frames,
    so the file is made writable by its owner until the last one sets `mode`.

    A put that fails has made nothing: maiTerm reads any refusal as "not made" (only `exists`
    means another writer has the name) and so never discards after one."""
    root = get_root(req["root"])
    p = contained(root.real, req["rel"], True)
    mode = int(req.get("mode", 0o644)) & 0o7777
    try:
        fd = os.open(p, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600 if req.get("more") else mode)
    except OSError as e:
        raise os_fail(e, req["rel"])
    try:
        try:
            write_all(fd, payload)
        finally:
            os.close(fd)
    except OSError as e:
        # O_EXCL made the name this put's own, so a write that fails after it (a full disk, a
        # quota) takes the half-written file back out rather than leave it under the real name.
        try:
            os.unlink(p)
        except OSError:
            pass
        raise os_fail(e, req["rel"])
    return {}, b""


def op_append(req, payload):
    """The next frame of a file `put` began: written only to a regular file exactly `at`
    bytes long, so nothing else that took the name meanwhile is ever written to."""
    root = get_root(req["root"])
    p = contained(root.real, req["rel"], True)
    try:
        fd = os.open(p, os.O_WRONLY | os.O_APPEND | getattr(os, "O_NOFOLLOW", 0))
        try:
            st = os.fstat(fd)
            if not stat.S_ISREG(st.st_mode) or st.st_size != int(req["at"]):
                raise Fail("changed", "%s changed while it was being sent" % req["rel"])
            write_all(fd, payload)
            if not req.get("more"):
                os.fchmod(fd, int(req.get("mode", 0o644)) & 0o7777 & ~current_umask())
        finally:
            os.close(fd)
    except OSError as e:
        raise os_fail(e, req["rel"])
    return {}, b""


def op_symlink(req):
    root = get_root(req["root"])
    p = contained(root.real, req["rel"], True)
    try:
        os.symlink(B(req["target"]), p)
    except OSError as e:
        raise os_fail(e, req["rel"])
    return {}, b""


def op_discard(req):
    """Remove what a failed import made: maiTerm names only an item it created in that op."""
    root = get_root(req["root"])
    p = contained(root.real, req["rel"], True)
    try:
        remove_any(p)
    except OSError as e:
        raise os_fail(e, req["rel"])
    return {}, b""


# -- Watching -----------------------------------------------------------------------------


IN_MODIFY = 0x2
IN_CLOSE_WRITE = 0x8
IN_MOVED_FROM = 0x40
IN_MOVED_TO = 0x80
IN_CREATE = 0x100
IN_DELETE = 0x200
IN_DELETE_SELF = 0x400
IN_MOVE_SELF = 0x800
IN_Q_OVERFLOW = 0x4000
IN_IGNORED = 0x8000
IN_ONLYDIR = 0x01000000
IN_ISDIR = 0x40000000
SHAPE = IN_CREATE | IN_DELETE | IN_MOVED_FROM | IN_MOVED_TO
SELF = IN_DELETE_SELF | IN_MOVE_SELF
MASK = SHAPE | SELF | IN_CLOSE_WRITE | IN_ONLYDIR
EVENT = struct.Struct("iIII")


def inotify_limits():
    try:
        with open("/proc/sys/fs/inotify/max_user_watches") as f:
            mx = int(f.read().strip())
    except (OSError, ValueError):
        mx = 0
    budget = min(mx // 8, 8192)
    # maiTerm's own tests set a small budget to see the fallback; nothing else sets this.
    forced = os.environ.get("MAITERM_FT_WATCH_BUDGET")
    if forced and forced.isdigit():
        budget = min(budget, int(forced))
    return mx, budget


class Watcher(object):
    """inotify through ctypes, on the folders maiTerm has open and the repository's own
    folders, within a budget of min(max_user_watches / 8, 8192). A folder that can't have a
    watch (out of budget, ENOSPC, no inotify on this host) is polled by mtime, and only when
    maiTerm asks (`poll`): it asks while a view can see the result."""

    def __init__(self):
        self.lock = threading.Lock()
        self.fd = None
        self.lc = None
        self.max_watches, self.budget = inotify_limits()
        self.wds = {}        # wd -> set of (tag, rel, kind)
        self.paths = {}      # path -> wd
        self.users = {}      # (tag, rel, kind) -> path
        self.polled = {}     # (tag, rel) -> (path, mtime_ns)
        self.tags = {}       # tag -> root real path
        self.pending = {}
        self.first = None
        self.available = False
        lc = libc()
        if lc is not None and sys.platform.startswith("linux") and hasattr(lc, "inotify_init1"):
            fd = lc.inotify_init1(os.O_NONBLOCK | os.O_CLOEXEC)
            if fd >= 0:
                self.fd = fd
                self.lc = lc
                self.lc.inotify_add_watch.argtypes = [ctypes.c_int, ctypes.c_char_p, ctypes.c_uint32]
                self.lc.inotify_add_watch.restype = ctypes.c_int
                self.lc.inotify_rm_watch.argtypes = [ctypes.c_int, ctypes.c_int]
                self.available = True
                t = threading.Thread(target=self.loop, name="watch")
                t.daemon = True
                t.start()

    def add(self, tag, real, rel, path, kind):
        """Watch `path` for (tag, rel). True when watched, False when it is to be polled."""
        key = (tag, rel, kind)
        with self.lock:
            self.tags[tag] = real
            if key in self.users:
                return (tag, rel) not in self.polled
            wd = self.paths.get(path)
            if wd is None:
                if self.fd is None or len(self.wds) >= self.budget:
                    wd = -1
                else:
                    wd = self.lc.inotify_add_watch(self.fd, path, MASK)
                    if wd < 0:
                        err = ctypes.get_errno()
                        if err != errno.ENOSPC:
                            # Gone or not a folder: its listing says so; nothing to watch.
                            return True
                if wd >= 0:
                    self.paths[path] = wd
            self.users[key] = path
            if wd >= 0:
                self.wds.setdefault(wd, set()).add(key)
                return True
            if kind == "dir":
                try:
                    m = os.stat(path).st_mtime_ns
                except OSError:
                    m = None
                self.polled[(tag, rel)] = (path, m)
            return False

    def remove(self, tag, rel, kind):
        key = (tag, rel, kind)
        with self.lock:
            path = self.users.pop(key, None)
            self.polled.pop((tag, rel), None)
            if path is None:
                return
            wd = self.paths.get(path)
            if wd is None:
                return
            users = self.wds.get(wd, set())
            users.discard(key)
            if not users:
                self.wds.pop(wd, None)
                for p in [p for p, w in self.paths.items() if w == wd]:
                    del self.paths[p]
                self.lc.inotify_rm_watch(self.fd, wd)

    def forget(self, tag):
        with self.lock:
            keys = [k for k in self.users if k[0] == tag]
        for k in keys:
            self.remove(k[0], k[1], k[2])
        with self.lock:
            self.tags.pop(tag, None)
            self.pending.pop(tag, None)

    def polled_rels(self, tag):
        with self.lock:
            return sorted(rel for (t, rel) in self.polled if t == tag)

    def watched_rels(self, tag):
        with self.lock:
            return set(rel for (t, rel, kind) in self.users if t == tag and kind == "dir")

    def poll(self, tag, rels):
        changed = []
        with self.lock:
            targets = [(rel, self.polled.get((tag, rel))) for rel in rels]
        for rel, entry in targets:
            if entry is None:
                continue
            path, before = entry
            try:
                now = os.stat(path).st_mtime_ns
            except OSError:
                now = None
            if now != before:
                changed.append(rel)
                with self.lock:
                    if (tag, rel) in self.polled:
                        self.polled[(tag, rel)] = (path, now)
        return changed

    def loop(self):
        while True:
            timeout = None
            with self.lock:
                if self.first is not None:
                    timeout = max(0.0, self.first + COALESCE - time.monotonic())
            try:
                ready, _, _ = select.select([self.fd], [], [], timeout)
            except (OSError, ValueError):
                return
            if ready:
                try:
                    data = os.read(self.fd, 1 << 16)
                except OSError as e:
                    if e.errno in (errno.EAGAIN, errno.EINTR):
                        data = b""
                    else:
                        return
                self.absorb(data)
            with self.lock:
                due = self.first is not None and time.monotonic() >= self.first + COALESCE
            if due:
                self.flush()

    def note(self, tag):
        if self.first is None:
            self.first = time.monotonic()
        p = self.pending.get(tag)
        if p is None:
            p = {"dirs": set(), "paths": {}, "many": False, "index": False, "worktree": False, "ignore": set()}
            self.pending[tag] = p
        return p

    def absorb(self, data):
        off = 0
        with self.lock:
            while off + EVENT.size <= len(data):
                wd, mask, _cookie, ln = EVENT.unpack_from(data, off)
                name = data[off + EVENT.size:off + EVENT.size + ln].rstrip(b"\0")
                off += EVENT.size + ln
                if mask & IN_Q_OVERFLOW:
                    for tag in self.tags:
                        self.note(tag)["many"] = True
                    continue
                users = self.wds.get(wd)
                if not users:
                    continue
                if mask & IN_IGNORED:
                    self.wds.pop(wd, None)
                    for p in [p for p, w in self.paths.items() if w == wd]:
                        del self.paths[p]
                    for k in list(users):
                        self.users.pop(k, None)
                    continue
                for tag, rel, kind in users:
                    p = self.note(tag)
                    if kind == "git":
                        # git writes `index.lock` and renames it onto `index`: the lock is noise.
                        if not name.endswith(b".lock"):
                            p["index"] = True
                            if rel == "git/info" and name == b"exclude":
                                p["ignore"].add(None)
                        continue
                    if mask & SELF:
                        p["dirs"].add(rel)
                        continue
                    if not name:
                        continue
                    sname = S(name)
                    if mask & SHAPE:
                        p["dirs"].add(rel)
                    if name == b".gitignore":
                        p["ignore"].add(rel)
                    child = join_rel(rel, sname)
                    if len(p["paths"]) < MAX_EVENT_PATHS:
                        p["paths"][child] = bool(mask & IN_ISDIR)
                    else:
                        p["many"] = True

    def flush(self):
        with self.lock:
            pending = self.pending
            self.pending = {}
            self.first = None
            tags = dict(self.tags)
        for tag, p in pending.items():
            real = tags.get(tag)
            root = ROOTS.get(real) if real is not None else None
            dirs = set(p["dirs"])
            if root is not None and p["ignore"]:
                with root.lock:
                    for rel in p["ignore"]:
                        if rel is None:
                            root.ignore.invalidate_all()
                        else:
                            root.ignore.invalidate_folder(rel)
                # Every open folder the changed rules reach is listed again, so its flags follow.
                watched = self.watched_rels(tag)
                for rel in p["ignore"]:
                    dirs.update(w for w in watched if rel is None or rel == "" or w == rel or w.startswith(rel + "/"))
            paths = []
            worktree = False
            if root is not None:
                with root.lock:
                    for rel, is_dir in p["paths"].items():
                        parent, name = split_parent(rel)
                        if name in (".git", ".DS_Store"):
                            continue
                        if root.ignore.dir_is_ignored(parent) or root.ignore.is_ignored(parent, name, is_dir):
                            continue
                        worktree = True
                        full = os.path.join(root.real, B(rel))
                        try:
                            st = os.lstat(full)
                            k = "d" if stat.S_ISDIR(st.st_mode) else "f"
                        except OSError:
                            k = "x"
                        paths.append([rel, k])
            send({"ev": "fs", "tag": tag, "dirs": sorted(dirs), "paths": paths, "many": p["many"], "index": p["index"],
                  "worktree": worktree or p["many"]})


WATCHER = []
WATCHER_LOCK = threading.Lock()


def watcher():
    with WATCHER_LOCK:
        if not WATCHER:
            WATCHER.append(Watcher())
        return WATCHER[0]


def op_watch(req):
    """Watch folders of a root for the view tagged `tag`; `git` names the repository's own
    folders (index, HEAD, refs), whose changes are git signals. Answers the folders that are
    polled instead."""
    root = get_root(req["root"])
    tag = int(req["tag"])
    w = watcher()
    for rel in req.get("rels", []):
        try:
            p = contained(root.real, rel, False)
        except Fail:
            continue
        w.add(tag, root.real, rel, p, "dir")
    for g in req.get("git", []):
        gp = os.path.realpath(expand(g))
        for sub, rel in ((gp, "git"), (os.path.join(gp, b"refs", b"heads"), "git/refs/heads"), (os.path.join(gp, b"info"), "git/info")):
            if os.path.isdir(sub):
                w.add(tag, root.real, rel, sub, "git")
    return {"polled": w.polled_rels(tag), "inotify": w.available}, b""


def op_unwatch(req):
    tag = int(req["tag"])
    w = watcher()
    for rel in req.get("rels", []):
        w.remove(tag, rel, "dir")
    return {}, b""


def op_forget(req):
    watcher().forget(int(req["tag"]))
    return {}, b""


def op_poll(req):
    tag = int(req["tag"])
    w = watcher()
    rels = req.get("rels")
    if rels is None:
        rels = w.polled_rels(tag)
    return {"changed": w.poll(tag, rels)}, b""


# -- Hello and the loop -------------------------------------------------------------------


def git_version():
    try:
        r = subprocess.run(["git", "--version"], stdin=subprocess.DEVNULL, stdout=subprocess.PIPE, stderr=subprocess.DEVNULL, timeout=5)
        return S(r.stdout).strip() or None
    except (OSError, subprocess.SubprocessError):
        return None


def op_hello(req):
    mx, budget = inotify_limits()
    w = watcher()
    return {
        "protocol": PROTOCOL,
        "hostname": socket.gethostname(),
        "uid": os.getuid(),
        "home": S(os.path.expanduser(b"~")),
        "python": platform.python_version(),
        "git": git_version(),
        "inotify": {"available": w.available, "max_user_watches": mx, "budget": budget},
    }, b""


OPS = {
    "hello": op_hello,
    "list": op_list,
    "stat": op_stat,
    "probe": op_probe,
    "root": op_root,
    "read": op_read,
    "git": op_git,
    "files": op_files,
    "fileop": op_fileop,
    "tally": op_tally,
    "land": op_land,
    "mkdir": op_mkdir,
    "symlink": op_symlink,
    "discard": op_discard,
    "watch": op_watch,
    "unwatch": op_unwatch,
    "forget": op_forget,
    "poll": op_poll,
}
PAYLOAD_OPS = {"write": op_write, "put": op_put, "append": op_append}


def handle(req, payload):
    rid = req.get("id")
    op = req.get("op")
    try:
        if op in PAYLOAD_OPS:
            result, out = PAYLOAD_OPS[op](req, payload)
        elif op in OPS:
            result, out = OPS[op](req)
        else:
            raise Fail("io", "unknown op %r" % (op,))
        header = {"id": rid, "ok": True}
        header.update(result)
        send(header, out)
    except Fail as f:
        send({"id": rid, "ok": False, "code": f.code, "msg": f.msg})
    except OSError as e:
        f = os_fail(e, op or "")
        send({"id": rid, "ok": False, "code": f.code, "msg": f.msg})
    except Exception as e:  # a bug here must answer, not leave maiTerm waiting
        send({"id": rid, "ok": False, "code": "io", "msg": "%s: %s" % (type(e).__name__, e)})


def read_frame():
    line = IN.readline(MAX_HEADER)
    if not line or not line.endswith(b"\n"):
        return None
    LAST_HEARD[0] = time.monotonic()
    req = json.loads(line.decode("utf-8"))
    n = int(req.get("len") or 0)
    payload = read_payload(n) if n else b""
    if len(payload) != n:
        return None
    return req, payload


def read_payload(n):
    """`n` bytes, or fewer at the end of the stream. Each piece that arrives counts as hearing
    from maiTerm: a payload on a slow link can take longer than the ping deadline, and no ping
    can be read until it is all in."""
    parts = []
    left = n
    while left:
        b = IN.read1(min(left, 1 << 16))
        if not b:
            break
        parts.append(b)
        left -= len(b)
        LAST_HEARD[0] = time.monotonic()
    return b"".join(parts)


def liveness():
    """Exit when the parent (the ssh session) is gone or maiTerm has gone quiet."""
    while True:
        time.sleep(min(5.0, PING_DEADLINE / 4))
        if os.getppid() != PARENT or time.monotonic() - LAST_HEARD[0] > PING_DEADLINE:
            die()


# Requests that make something an import may discard, still running, by (root, rel).
MAKING_OPS = ("put", "append", "mkdir", "symlink")
MAKING = {}
MAKING_LOCK = threading.Lock()


def under_rel(rel, top):
    return rel == top or (top != "" and rel.startswith(top + "/"))


def dispatch(pool, req, payload):
    """Hand a request to the workers, which run in any order. A `discard` waits for every
    request read before it that is still making something at or under its path: maiTerm sends
    one after a put whose answer it stopped waiting for, and run first it would remove nothing
    and let the put then make the half-written file it was sent to take away."""
    op = req.get("op")
    root, rel = req.get("root"), str(req.get("rel") or "")
    if op == "discard":
        with MAKING_LOCK:
            before = [f for (r, k), fs in MAKING.items() if r == root and under_rel(k, rel) for f in fs]

        def after():
            futures_wait(before)
            handle(req, payload)

        pool.submit(after)
        return
    fut = pool.submit(handle, req, payload)
    if op in MAKING_OPS:
        key = (root, rel)
        with MAKING_LOCK:
            MAKING.setdefault(key, set()).add(fut)

        def done(f):
            with MAKING_LOCK:
                fs = MAKING.get(key)
                if fs is not None:
                    fs.discard(f)
                    if not fs:
                        del MAKING[key]

        fut.add_done_callback(done)


def main():
    t = threading.Thread(target=liveness, name="liveness")
    t.daemon = True
    t.start()
    pool = ThreadPoolExecutor(max_workers=6)
    send({"ev": "ready", "protocol": PROTOCOL})
    while True:
        try:
            frame = read_frame()
        except (OSError, ValueError):
            frame = None
        if frame is None:
            die()
        req, payload = frame
        if req.get("op") == "ping":
            send({"id": req.get("id"), "ok": True})
            continue
        dispatch(pool, req, payload)


if __name__ == "__main__":
    main()
