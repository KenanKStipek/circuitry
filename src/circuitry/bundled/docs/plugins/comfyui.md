# comfyui

Generates images via ComfyUI REST API. Supports txt2img and img2img workflows using Stable Diffusion / FLUX checkpoints.

## When to use

Image generation, illustration, visual content creation, style transfer, image-to-image transformation, character sheets, scene rendering.

## Required fields

- `prompt` (string, top-level) — image generation prompt. Supports Mustache rendering (e.g. `{{prime.expand_prompt.value}}`).
- `model` (string, top-level) — checkpoint filename (e.g. `flux1-dev-fp8.safetensors`)

## Optional params

- `params.width` (int, default 512) — image width
- `params.height` (int, default 512) — image height
- `params.steps` (int, default 20) — sampling steps
- `params.cfg` (float, default 7.0) — classifier-free guidance scale
- `params.sampler_name` (string, default `euler`) — sampler algorithm
- `params.scheduler` (string, default `normal`) — scheduler type
- `params.seed` (int) — random seed (auto-generated if absent)
- `params.negative_prompt` (string) — negative prompt for content to avoid
- `params.reference_image` (string) — path to reference image; triggers img2img workflow
- `params.denoise` (float, 0-1) — denoising strength for img2img (lower = closer to reference)
- `params.image_output` (string: `path`|`base64`|`url`, default `path`) — output format
- `params.image_dir` (string, default `./output/images`) — output directory for path mode

## Examples

Text-to-image:
```yaml
- type: tool
  name: generate_image
  provider: comfyui
  prompt: "{{prime.expand_prompt.value}}"
  model: flux1-dev-fp8.safetensors
  params:
    width: 768
    height: 768
    steps: 20
    cfg: 3.5
    sampler_name: euler
    scheduler: beta
    image_dir: "./output/images"
```

Image-to-image with reference:
```yaml
- type: tool
  name: redraw_scene
  provider: comfyui
  prompt: "{{prime.scene_prompt.value}}"
  model: flux1-dev-fp8.safetensors
  params:
    reference_image: "{{prime.ref_sheet.value}}"
    denoise: 0.82
    width: 768
    height: 768
    steps: 20
    cfg: 3.5
    image_dir: "./output/scenes"
```
