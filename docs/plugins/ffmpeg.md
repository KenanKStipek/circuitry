# ffmpeg

Runs local ffmpeg commands for audio/video/image processing. Subprocess-based — no shell layer, no shell injection risk.

## When to use

Transcoding, format conversion, image compositing, adding text overlays, combining media files, extracting frames, applying video/audio filters, creating slideshows or strip layouts.

## Required params

- `params.input` (string) — input file path
- `params.output` (string) — output file path

## Optional params

- `params.flags` (string) — raw ffmpeg flags (e.g. `-c:v libx264 -crf 23`)
- `params.extra_inputs` (array of strings) — additional `-i` input files
- `params.filter_complex` (string) — filter_complex expression for multi-input operations (e.g. overlay, concat)
- `params.vf_drawtext` (object) — drawtext filter config for text overlays:
  - `text` (string, required) — the text to render
  - `fontfile` (string) — path to .ttf font file
  - `x`, `y` (string/int) — position (supports ffmpeg expressions like `(w-text_w)/2`)
  - `fontsize` (int) — font size in pixels
  - `fontcolor` (string) — e.g. `white`, `#FF0000`
  - `box` (int) — 1 to enable background box
  - `boxcolor` (string) — e.g. `black@0.6`
  - `boxborderw` (int) — box padding in pixels
- `params.map` (string) — output stream mapping (e.g. `[out]`)

## Safety

Shell metacharacters (`&&`, `||`, `;`, `>`, `<`, `` ` ``, `$(`) are rejected in all params. `-y` (overwrite) is always injected automatically.

## Examples

Simple transcode:
```yaml
- type: tool
  name: convert_video
  provider: ffmpeg
  params:
    input: "{{prime.source_path.value}}"
    output: "{{prime.source_path.value}}.mp4"
    flags: "-c:v libx264 -crf 23"
```

Text overlay using drawtext:
```yaml
- type: tool
  name: add_title
  provider: ffmpeg
  params:
    input: "{{prime.image_path.value}}"
    output: "./output/titled.png"
    vf_drawtext:
      text: "{{prime.generate_title.value}}"
      fontsize: 48
      fontcolor: white
      x: "(w-text_w)/2"
      y: 50
      box: 1
      boxcolor: "black@0.6"
      boxborderw: 10
```

Multi-input composite with filter_complex:
```yaml
- type: tool
  name: combine_panels
  provider: ffmpeg
  params:
    input: "./panel_1.png"
    extra_inputs:
      - "./panel_2.png"
      - "./panel_3.png"
    output: "./output/strip.png"
    filter_complex: "[0][1][2]hstack=inputs=3"
    map: "[out]"
```
