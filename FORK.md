# Bushido Studio: a modified version of Concat

As the GNU AGPL 3.0 (section 5a) asks of a modified work: **Bushido Studio
is a modified version of Concat**, by Royalty Studio, changed from Concat
0.2.8 (commit `79116bd`, 18 October 2026) from 19 October 2026 on.

Concat is © 2026 Jareer and the Concat contributors
(https://github.com/jub0t/Concat), free software under the AGPL 3.0 or
later. Bushido Studio is released under the same licence. Code the fork
changed is marked in its comments "(Royalty Studio fork)"; the files it
added carry the line `SPDX-FileCopyrightText: 2026 Royalty Studio`.

## What changed

- The name, the icon, the app identifier (`studio.bushido.editor`, whose
  first run takes over Concat's settings and models, leaving Concat's own
  folders as they were) and the binary (`bushido-studio`). Its own version
  number, from 0.1.0; it tells a client made for Concat that it is Concat
  0.2.8. The updater reads Bushido Studio's own releases
  (`concat_host::dirs::REPOSITORY`, github.com/dcryptoRS/BushidoStudio),
  never Concat's.
- Linux alone: the packages (.deb, .rpm, Arch, AppImage) under the new
  name, built with Whisper on Vulkan; the macOS, Windows and phone builds
  are gone from CI.
- Decoding on NVDEC and encoding on NVENC; previews on the GPU; Whisper on
  CUDA or Vulkan. A zoomed picture decoded at the size it is drawn, not at
  its fit.
- Live mode for the API and the MCP server; the Claude panel, its guard,
  its craft guide and its settings; media generation (Higgsfield,
  BytePlus ModelArk, Gemini) and the desktop keyring for the keys.
- Video and audio lanes with linked clips; Resolve-style saving and
  recovery; the split timeline.
- Cut pauses and word-by-word captions.
- GPU scopes, the HSL Qualifier, the Color Mixer, Broadcast Safe, and
  `frame_levels` for the assistant.
- Markers, J-K-L, in and out, three-point editing, lift and extract,
  roll, slip and slide; J and L cuts.
- Hue Curves (`concat.hue-curves`), and curves that rest flat and run
  round the colour wheel.
- Ducking, loudness (EBU R128, the true peak held under −1 dBTP), delivery presets,
  and a vertical copy of a timeline that follows each shot's subject
  (`ReframeTimeline`).
- The look: Inter, pill buttons, round switches, the blue accent; text
  drawn with sub-pixel positioning on Linux.

Effect ids (`concat.*`), the project format and the MCP tool names are
Concat's still, so projects and scripts move between the two.

Concat's contributor licence agreement and its Windows code-signing policy
are kept, as Concat's, in [docs/CONCAT-CLA.md](docs/CONCAT-CLA.md) and
[docs/CONCAT-CODE_SIGNING.md](docs/CONCAT-CODE_SIGNING.md): Bushido Studio
takes contributions under the AGPL alone and ships for Linux.
