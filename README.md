# Bushido Studio

A video editor for Linux, made for professional work: cutting, colour,
sound and short-form delivery, with an AI assistant that edits the project
on screen. By Royalty Studio.

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
  Whisper on the GPU (CUDA).
- **Colour**: GPU scopes (waveform, RGB parade, vectorscope with the
  skin-tone line, histogram), an HSL qualifier for secondaries, a colour
  mixer for hue-by-hue work, and a broadcast-safe limiter.
- **The cutting room**: markers, J-K-L, in and out points, three-point
  insert and overwrite, lift and extract, roll, slip and slide.
- **Generated media**: clips and pictures from Higgsfield (Seedance 2.5),
  BytePlus ModelArk and Gemini, with your own keys kept in the desktop's
  keyring.
- **An assistant** (Claude Code) in a panel, editing and grading with a
  professional editor's and colourist's craft, measuring frames as scopes
  do, and kept away from what you protect.

## Requirements

Linux, x86-64. An NVIDIA GPU is used when there is one - decoding on NVDEC,
encoding on NVENC, Whisper on CUDA - and the editor runs without one.
The assistant panel needs [Claude Code](https://claude.com/claude-code)
installed; generated media needs an account and a key with the service.

## Building

As Concat builds (see [docs/CONCAT-README.md](docs/CONCAT-README.md) and
the `docs` folder), from `src/`:

```sh
cargo build --profile quick -p concat               # the editor, bin "bushido-studio"
cargo build --profile quick -p concat --features cuda  # with Whisper on CUDA
```

## Licence

Free software under the GNU Affero General Public License 3.0 or later, as
Concat is: see [LICENSE](LICENSE). Concat's copyright notices are kept in
every file they were in; the files and changes Bushido Studio adds are
© 2026 Royalty Studio under the same licence. The name Concat and its logo
are Concat's ([TRADEMARK.md](TRADEMARK.md)); Bushido Studio uses neither
as its own.
