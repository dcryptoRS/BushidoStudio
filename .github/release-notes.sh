#!/usr/bin/env bash
# The notes for a release.
#
# Usage: release-notes.sh <since-ref> <until-ref> <version>
#
# Writes markdown to stdout: what changed, what to know before installing,
# then how to download each bundle and the checksum file.
#
# "Before you install" says what to know first: Bushido Studio is a
# modified Concat, and where its sources are.
#
# What changed is the subjects of the commits between the two refs,
# filtered as follows.
#
# What a release note is for is what somebody running the app can notice, so
# three kinds of subject are dropped on the way past. Housekeeping, by its
# shape: formatting, lock files, "Update foo.rs". Anything typed as work on
# the tree rather than on the product - a conventional-commit `test:`,
# `chore:`, `ci:`, `build:`, `refactor:`, `style:` or `docs:` says so
# itself, and so does a fix scoped to the pipeline, `fix(release):` or
# `fix(ci):` - a smoke script that can run is not something a person who
# downloads the app can notice. And a fix that only got the build green
# again: a lint appeased, an import removed, a file moved to please a
# compiler is not a fix anybody asked for.
#
# A subject that survives is printed as a sentence. A `feat(studio):` in
# front of one is scaffolding for the log and reads as noise in a release,
# so the type comes off and the first letter goes up. A subject that appears
# twice appears once.
set -euo pipefail

since="$1"
until="$2"
version="$3"
title="Bushido Studio $version"

# A conventional-commit type, with its optional (scope) and breaking `!`.
type='^[a-z0-9]+(\([^)]*\))?!?: '

# Each line opens with one mark for what it touches, from the commit's
# scope where it has one and its type where it has not - a fix reads as a
# fix at a glance, and the text rows stand apart from the timeline's. A
# subject with neither carries no mark rather than a wrong one.
marks='text=📝 speech=🗣️ enhance=🗣️ keyframes=🎞️ effects=✨ render=✨ colour=✨ color=✨ timeline=✂️ media=📁 export=📤 settings=⚙️ i18n=🌍 locales=🌍 api=🤖 server=🤖 android=📱 ios=📱 models=📦 inspector=🖥️ window=🖥️ workspace=🖥️ ui=🖥️ start=🖥️ feat=✨ fix=🐛 perf=⚡'
changes=$(git log "$since..$until" --no-merges --format='%s' 2>/dev/null \
  | grep -Ev '^((Create|Update) [^ ]+\.[a-z]+|Version [0-9]|Lock the flake|Format the workspace|Changelog for|Merge )' \
  | grep -Ev 'in the (export|pool) tests$' \
  | grep -Eiv '^(test|chore|ci|build|refactor|style|docs)(\([^)]*\))?!?: ' \
  | grep -Eiv '^[a-z0-9]+\((release|ci)\)!?: ' \
  | grep -Eiv "${type}.*(clippy|lint|rustfmt|(unused|duplicate|missing) [A-Za-z]* ?import|non-existent|does not compile|before test module|green again)" \
  | grep -Eiv '^(updates?|wip|fixes?|cleanup)\.?$' \
  | awk -v marks="$marks" '
      BEGIN {
        n = split(marks, pairs, " ")
        for (i = 1; i <= n; i++) { split(pairs[i], kv, "="); mark[kv[1]] = kv[2] }
      }
      {
        prefix = ""
        if (match($0, /^[a-z0-9]+(\([^)]*\))?!?: /)) {
          head = substr($0, 1, RLENGTH)
          $0 = substr($0, RLENGTH + 1)
          kind = head; sub(/[(!:].*/, "", kind)
          scope = ""
          if (match(head, /\([^)]*\)/)) scope = substr(head, RSTART + 1, RLENGTH - 2)
          if (scope in mark) prefix = mark[scope] " "
          else if (kind in mark) prefix = mark[kind] " "
        }
        print prefix toupper(substr($0, 1, 1)) substr($0, 2)
      }' \
  | awk '!seen[$0]++' \
  | sed 's/^/- /')

echo "## $title"
echo
if [ -n "$changes" ]; then
  echo "### What changed"
  echo
  echo "$changes"
  echo
fi
echo "### Before you install"
echo
echo "- Bushido Studio is a modified version of [Concat](https://github.com/jub0t/Concat), by Jareer and the Concat contributors, made by Royalty Studio. Concat is not responsible for it. Free software under the GNU AGPL 3.0 or later; this release's source is its tag."
echo "- It downloads its models (cutout, voices, Whisper) from Concat's mirror the first time they are needed."
echo
cat <<'MD'
### Download

Linux, as a package for your distribution or one AppImage that runs on any.

| | x86_64 | arm64 |
|---|---|---|
| Debian, Ubuntu, Mint | `linux-x86_64.deb` | `linux-aarch64.deb` |
| Fedora, openSUSE | `linux-x86_64.rpm` | `linux-aarch64.rpm` |
| Arch, Manjaro | `linux-x86_64.pkg.tar.zst` | `linux-aarch64.pkg.tar.zst` |
| Any distribution | `x86_64.AppImage` | `aarch64.AppImage` |

The preview and the export draw on the graphics card, and on x86_64
transcription runs on it too, through Vulkan: NVIDIA, AMD and Intel, with
the card's own driver and nothing else. On arm64 it runs on the CPU.

`SHA256SUMS` lists each file's checksum.
MD
