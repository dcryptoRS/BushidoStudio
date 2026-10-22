#!/usr/bin/env python3
# SPDX-License-Identifier: AGPL-3.0-or-later
# SPDX-FileCopyrightText: 2026 Jareer and Concat contributors
# SPDX-FileCopyrightText: 2026 Royalty Studio
"""The Claude pane's guard (Royalty Studio fork): a PreToolUse hook for Claude
Code, which runs in the pane with every permission. It refuses the few things
that cannot be taken back - and is the one limit the agent there has.

Reads the hook's JSON on stdin; exit 2 with a reason on stderr refuses the
call, exit 0 lets it through. Kept short and literal on purpose: it stops
accidents and planted instructions, not a determined adversary."""
import glob
import json
import os
import re
import shlex
import sys

HOME = os.path.expanduser("~")
USER = os.path.basename(HOME)
MEDIA = f"/run/media/{USER}"
# What can go at once without asking: more is a whole job (a shoot's folder).
MAX_FILES, MAX_BYTES = 300, 3 * 1024**3


def fences():
    """What the person protects in Settings (claude-guard.json, written beside
    this file on each start): folders, and names matched anywhere."""
    try:
        with open(os.path.splitext(os.path.abspath(__file__))[0] + ".json") as file:
            data = json.load(file)
    except Exception:
        data = {}
    folders, names = [], []
    for entry in data.get("protected") or []:
        entry = str(entry).strip()
        if entry.startswith(("/", "~")):
            folders.append(os.path.normpath(os.path.expanduser(entry)))
        elif entry:
            names.append(entry.lower())
    keep = [os.path.normpath(str(path)) for path in data.get("keep") or []]
    return folders, names, keep


FOLDERS, NAMES, KEEP = fences()


def refuse(why):
    print(f"Blocked by the editor's guard: {why}. If it really is needed, ask the person "
          "to do it from a terminal.", file=sys.stderr)
    sys.exit(2)


def fenced(path):
    """Inside a folder the person protects, or naming one of their names."""
    low = path.lower()
    return any(name in low for name in NAMES) or any(
        path == folder or path.startswith(folder + os.sep) for folder in FOLDERS)


def protected(path):
    """A folder whose loss is a disaster: the roots and the first level under them."""
    path = os.path.normpath(os.path.expanduser(path.strip("'\"")))
    if fenced(path):
        return True
    roots = ["/", "/home", HOME, MEDIA, f"{HOME}/dev", *KEEP]
    for root in roots:
        if path == root:
            return True
        parent = os.path.dirname(path)
        if root in (HOME, MEDIA) and parent == root:
            return True  # ~/X, /run/media/<user>/<drive>
        if parent.startswith(MEDIA + "/") and os.path.dirname(parent) == MEDIA:
            return True  # /run/media/<user>/<drive>/<folder>
    return path.startswith(("/etc", "/usr", "/bin", "/boot", "/var", "/opt", "/lib"))


def too_much(targets, cwd):
    """Whether `rm` would take too much: its targets, wildcards expanded, counted on disk."""
    files, size = 0, 0
    for target in targets:
        pattern = os.path.expanduser(target)
        if not os.path.isabs(pattern):
            pattern = os.path.join(cwd, pattern)
        for path in glob.glob(pattern) or [pattern]:
            if os.path.isfile(path):
                files, size = files + 1, size + os.path.getsize(path)
            elif os.path.isdir(path):
                for root, _, names in os.walk(path):
                    for name in names:
                        files += 1
                        try:
                            size += os.path.getsize(os.path.join(root, name))
                        except OSError:
                            pass
                    if files > MAX_FILES or size > MAX_BYTES:
                        return True
            if files > MAX_FILES or size > MAX_BYTES:
                return True
    return False


def check_bash(command, cwd):
    text = command.strip()
    low = text.lower()
    rules = [
        (r"(^|[;&|`(]\s*)(sudo|su|doas|pkexec)\b", "sudo and the like"),
        (r"\b(mkfs(\.\w+)?|wipefs|fdisk|sfdisk|parted|gdisk|shred)\b", "touching disks or partitions"),
        (r"\bdd\b[^|;&]*\bof=/dev/", "writing straight to a disk"),
        (r"\b(shutdown|reboot|poweroff|halt)\b|systemctl\s+(poweroff|reboot|halt|suspend)", "shutting down or restarting the computer"),
        (r"\bdocker\s+((system|image|builder|volume|container|network)\s+prune|rmi|(container\s+)?rm|volume\s+rm|kill)\b",
         "deleting Docker things"),
        (r"\bdocker\s+compose\b[^|;&]*\bdown\b", "taking a Docker stack down"),
        (r"\bgit\b[^|;&]*\bpush\b[^|;&]*(--force|\s-f\b|--force-with-lease)", "git push --force"),
        (r"\bgit\b[^|;&]*\b(reset\s+--hard|clean\s+-[a-z]*f|checkout\s+--\s*\.|restore\s+(\.|--staged\s+\.)|stash\s+(drop|clear)|branch\s+-D)\b",
         "discarding git work (there may be uncommitted changes)"),
        (r"(curl|wget)\b[^|;&]*\|\s*(sudo\s+)?(ba|z|fi)?sh\b", "running a script fetched from the internet"),
        (r"\b(chmod|chown)\s+-[a-z]*R", "changing permissions recursively"),
        (r":\(\)\s*\{\s*:\|:&\s*\};:", "a fork bomb"),
        (r"\bkill(all)?\b[^|;&]*(-9\s+-1|\s-1\b)", "killing every process"),
    ]
    for pattern, why in rules:
        if re.search(pattern, low):
            refuse(why)
    destructive = re.search(r"\brm\b|\bmv\b|\bdocker\b[^|;&]*\b(stop|down|restart)\b|\btruncate\b|\bshred\b|-delete\b|--delete\b|\bgit\b[^|;&]*\b(reset|clean)\b", low)
    if destructive and (any(name in low for name in NAMES) or any(folder.lower() in low for folder in FOLDERS)):
        refuse("deleting, moving or stopping something the person protects")
    # Recursive deletes and find -delete on protected folders.
    try:
        words = shlex.split(text, posix=True)
    except ValueError:
        words = text.split()
    for i, word in enumerate(words):
        if word == "rm":
            args = []
            for arg in words[i + 1:]:
                if arg in (";", "&&", "||", "|", "&"):
                    break
                args.append(arg)
            flags = "".join(a for a in args if a.startswith("-") and not a.startswith("--"))
            recursive = "r" in flags.lower() or "--recursive" in args
            targets = [a for a in args if not a.startswith("-")]
            if recursive and any(protected(t) or t in ("*", ".", "..", "~") for t in targets):
                refuse("deleting an important folder whole")
            if too_much(targets, cwd):
                refuse(f"deleting more than {MAX_FILES} files or {MAX_BYTES // 1024**3} GB at once")
        if word == "find" and ("-delete" in words or "rm" in words[i + 1:]):
            roots = [w for w in words[i + 1:] if not w.startswith("-")][:1]
            if any(protected(r) for r in roots):
                refuse("mass-deleting with find in an important folder")


def main():
    try:
        call = json.load(sys.stdin)
    except Exception:
        return
    tool = call.get("tool_name", "")
    data = call.get("tool_input") or {}
    if tool == "Bash":
        check_bash(str(data.get("command", "")), call.get("cwd") or os.getcwd())
    elif tool in ("Write", "Edit", "MultiEdit", "NotebookEdit"):
        path = str(data.get("file_path") or data.get("notebook_path") or "")
        if path.startswith(("/etc", "/usr", "/bin", "/boot", "/var", "/opt", "/lib")):
            refuse("writing to system files")
        if path and fenced(os.path.normpath(os.path.expanduser(path))):
            refuse("writing into something the person protects")


if __name__ == "__main__":
    main()
