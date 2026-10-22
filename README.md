# Bushido Studio

A video editor for Linux, made for professional work: cutting, colour,
sound and short-form delivery, with an AI assistant that edits the project
on screen. By Royalty Studio. Version 0.1.0, built on Concat 0.2.8.

**Bushido Studio is a modified version of [Concat](https://github.com/jub0t/Concat)**,
the free video editor by Jareer and the Concat contributors. It is not
Concat and the Concat project is not responsible for it: report problems
with Bushido Studio here, not there. Concat's own README is kept in
[docs/CONCAT-README.md](docs/CONCAT-README.md), and what was changed is
listed in [FORK.md](FORK.md).

## What it adds to Concat

- **Video and audio lanes**, with a clip's picture and sound linked and
  unlinked as in Premiere, and Resolve-style saving with a recovery copy.
- **Reels tools**: pauses cut out by the voice, word-by-word captions,
  Whisper on the GPU, and a vertical version of a horizontal edit in one
  step, each shot following its subject.
- **Colour**: GPU scopes (waveform, RGB parade, vectorscope with the
  skin-tone line, histogram), an HSL qualifier for secondaries, hue curves
  (hue vs hue, saturation and luminance) and a colour mixer, and a
  broadcast-safe limiter.
- **The cutting room**: markers, J-K-L, in and out points, three-point
  insert and overwrite, lift and extract, roll, slip and slide, and J and
  L cuts in one click.
- **Sound and delivery**: music ducked under the voice, delivery presets
  for Reels, TikTok, Shorts, YouTube, podcasts and broadcast, and exports
  normalised to a loudness (EBU R128), the true peak held under −1 dBTP.
- **Generated media**: clips and pictures from Higgsfield (Seedance 2.5),
  BytePlus ModelArk and Gemini, with your own keys kept in the desktop's
  keyring.
- **An assistant** (Claude Code) in a panel, editing and grading with a
  professional editor's and colourist's craft, measuring frames as scopes
  do, and kept away from what you protect.

## Requirements

Linux, x86-64 or arm64. The graphics card is used for the preview and for
Whisper through Vulkan - NVIDIA, AMD or Intel, with its own driver and
nothing else - and on NVIDIA for decoding (NVDEC) and encoding (NVENC);
the editor runs without one. The assistant panel needs
[Claude Code](https://claude.com/claude-code) installed; generated media
needs an account and a key with the service.

## Installing

Each release carries a package for your distribution and an AppImage for
any: `BushidoStudio-<version>-linux-x86_64.deb` (Debian, Ubuntu, Mint),
`.rpm` (Fedora, openSUSE), `.pkg.tar.zst` (Arch) and
`BushidoStudio-<version>-x86_64.AppImage`. The packages install under
`/opt/bushido-studio` with `bushido-studio` on the path. Models (cutout,
voices, Whisper) are downloaded from Concat's mirror the first time they
are needed. A Concat already installed is left alone; Bushido Studio's
first run copies its settings and models over.

## Building

As Concat builds (see [docs/CONCAT-README.md](docs/CONCAT-README.md) and
the `docs` folder), from `src/`:

```sh
cargo build --profile quick -p concat                    # the editor, bin "bushido-studio"
cargo build --profile quick -p concat --features vulkan  # Whisper on any GPU, as the packages
cargo build --profile quick -p concat --features cuda    # Whisper on CUDA, for an NVIDIA card
```

The Vulkan build needs the Vulkan headers and `glslc`
(`sudo apt install libvulkan-dev glslc` on Debian and Ubuntu); the CUDA
build needs the CUDA toolkit.

## Publishing a release

`.github/workflows/release.yml` builds, checks and drafts a release when a
tag `v<version>` is pushed, the version being `src/Cargo.toml`'s. Before
the first one: put the repository's `owner/name` in
`concat_host::dirs::REPOSITORY` (which turns the updater on and points the
links at it), the maintainer's address in `assets/linux/nfpm.yaml` and
`assets/arch/PKGBUILD`, and the homepage there and in `flake.nix`.

## Licence

Free software under the GNU Affero General Public License 3.0 or later, as
Concat is: see [LICENSE](LICENSE). Concat's copyright notices are kept in
every file they were in; the files and changes Bushido Studio adds are
© 2026 Royalty Studio under the same licence. The name Concat and its logo
are Concat's ([TRADEMARK.md](TRADEMARK.md)); Bushido Studio uses neither
as its own.
