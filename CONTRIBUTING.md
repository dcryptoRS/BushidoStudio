# Contributing to Bushido Studio

Thanks for being here. Bushido Studio is a video editor for Linux, built on
[Concat](https://github.com/jub0t/Concat), and it gets better mostly through
people using it hard and reporting what broke.

## The most valuable thing you can do

Grab a build from the [Releases](https://github.com/dcryptoRS/BushidoStudio/releases)
page and edit a real video with it. Real footage finds what no reading of
the source does. A good [bug report](https://github.com/dcryptoRS/BushidoStudio/issues/new/choose)
(what you did, what happened, your distribution and graphics card, the
media you used) is worth more than most patches.

A problem that is in Concat itself - its engine, its timeline, its
exporter - is better reported to Concat too, so every fork gets the fix.

## Before you write code

Open an issue first for anything beyond a small fix. Large
areas are already in progress or intentionally deferred, and it is
no fun to review a big PR that has to be turned down for reasons that were
invisible from outside.

## Setting up

You will need Rust (the version `src/rust-toolchain.toml` pins), the FFmpeg
7+ development libraries, ONNX Runtime, cmake, a C++ compiler and libclang;
[`.github/workflows/build-app.yml`](.github/workflows/build-app.yml) is the
reference build. Then:

```sh
cd src && cargo run -p concat
```

That is the editor window. Everything - the engine, the host layer and the
Slint UI - is one Cargo workspace under `src/`.

## Layout

| Path | What lives there |
|---|---|
| `src/crates/` | The engine (core, media, render, effects, export, project), the host layer (`concat-host`, `concat-speech`), the API (`concat-api`, `concat-server`), the CLI, and `concat`, the Slint editor window |

[`src/README.md`](src/README.md) explains how the crates fit together and
where the sharp edges are. Read it before touching the engine.

## Checks

Run these before opening a PR:

```sh
cd src && cargo fmt --check && cargo clippy --all-targets -- -D warnings && cargo test
```

If you added or changed text the interface shows, also run
`python3 scripts/locales.py` so the string inventory follows; CI checks it.
Translations live in one JSON file per language; see
[`TRANSLATING.md`](TRANSLATING.md).

If you added or changed a downloadable model, put it in
[`models/manifest.toml`](models/manifest.toml) as well as the engine table it
belongs to, and run `python3 scripts/models.py --check`; CI checks that too.
A maintainer then runs the *Mirror models* workflow, which fetches the model,
records its digest in both places and publishes it to the mirror the app
downloads from.

Write what happens through the `log` facade (`log::info!`, `log::warn!`,
`log::error!`) rather than to standard output or standard error. Every run
writes `~/.local/share/studio.bushido.editor/logs/concat-<when>.log`, and a packaged build has no
terminal for anything that goes anywhere else; `CONCAT_LOG=debug` turns the
level up. See `src/crates/concat-host/src/logs.rs`.

New source files need a licence header (see below). Match the style of the code
around you; the engine avoids cleverness on purpose.

## Licensing your contribution

Bushido Studio is **AGPL-3.0-or-later** ([`LICENSE`](LICENSE)), as Concat is.

Contributions are accepted under that same licence: what you send in is
licensed to everyone on the terms the project is. There is no contributor
licence agreement, and you keep the copyright in your work. (Concat's own
CLA, which applies to contributions to Concat, is kept in
[`docs/CONCAT-CLA.md`](docs/CONCAT-CLA.md).)

### File headers

Every source file starts with:

```rust
// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Jareer and Concat contributors
```

in a file that came from Concat, and

```rust
// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Royalty Studio
```

in one Bushido Studio added. Keep your own copyright line if you want one:
add it, don't replace what's there.

### Third-party code

If your patch brings in code you did not write, say so in the PR: what it is,
where it came from, and its licence. Anything incompatible with
AGPL-3.0-or-later cannot be merged, and licence problems are much cheaper to
catch before a merge than after a release.

## Trademarks

The code is free to fork. The Concat name and logo are Concat's and not
covered by the AGPL grant ([`TRADEMARK.md`](TRADEMARK.md)), which is why this
fork has a name of its own. Forks of Bushido Studio are welcome too; please
ship them under your own name and logo.

## Conduct

[`CODE_OF_CONDUCT.md`](CODE_OF_CONDUCT.md) applies to the repository, its
issues and its pull requests.
