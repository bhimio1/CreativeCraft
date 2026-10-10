# AI image and video generation

The **Generate** page makes images and short videos from a description. You choose the service:
one running on your own computer, a cloud service with your own API key, or any compatible
endpoint. Results are saved in your projects folder under `Generated/<date>/`, so they sync with
the rest of your work and the apps can open them. Agents can generate too, through the hub's tools.

CreativeCraft has no account and sells no credits. A cloud service bills your own account
directly, and a local one costs nothing.

## Services

Set them up in **Settings → AI generation**: **Add a service**, fill in its key or address, then
**Test**. Test checks the connection and the key without generating, so it costs nothing.

| Service | Images | Video | You need |
|---|---|---|---|
| **ComfyUI** (local) | Any Stable Diffusion checkpoint with the built-in workflow; FLUX, SD3 and anything else with your own workflow | With your own workflow (Wan, LTX, Hunyuan, …) | ComfyUI running (`http://127.0.0.1:8188` by default) |
| **Stable Diffusion WebUI** (local) | AUTOMATIC1111, Forge, SD.Next | No | Started with `--api` (`http://127.0.0.1:7860`) |
| **OpenAI** | GPT Image | Sora | An API key from platform.openai.com |
| **Google Gemini** | Gemini image models | Veo | An API key from Google AI Studio |
| **xAI** | Grok Imagine | Grok Imagine video | An API key from console.x.ai |
| **Replicate** | Hundreds of models: FLUX, Ideogram, Recraft, Seedream, Imagen… | Veo, Kling, MiniMax, Wan, Seedance… | An API token; the model as `owner/name` |
| **fal** | Hundreds of models: FLUX, Recraft, Seedream… | Veo, Kling, Luma, MiniMax, Wan… | An API key; the model's endpoint id (`fal-ai/…`) |
| **Together AI** | FLUX | No | An API key |
| **Custom (OpenAI-compatible)** | Any server with an OpenAI-style `/images/generations`: LocalAI, gateways such as LiteLLM or OpenRouter-style proxies, your own | No | Its address; a key if it asks for one |

You can add a service more than once (two ComfyUI machines, two Replicate models), and give each
one its own name.

**Models.** Every model name is an editable field, because services rename and retire models
often. The presets in [`catalog/generation.toml`](../catalog/generation.toml) are only starting
points. On the Generate page, **Model** overrides the service's model for one request.

**Extra options** is a JSON object added to every request, for model-specific settings such as
`{"steps": 30}`, `{"guidance_scale": 4}` or `{"quality": "high"}`. It isn't offered for ComfyUI,
where your workflow holds the settings, or for Google.

## Local: ComfyUI

With no workflow set, CreativeCraft sends a basic text-to-image workflow: a checkpoint loader, two
prompts, a sampler, a decoder and a save. The **Image model** is a checkpoint file name (empty:
the first one ComfyUI has).

For anything else, build the workflow in ComfyUI and save it with **Workflow → Export (API)**.
Then put these placeholders where the values go and set the file as **Your workflow**:

| Placeholder | Becomes |
|---|---|
| `{{prompt}}`, `{{negative}}` | your description and what to avoid (also inside longer text) |
| `{{seed}}`, `{{width}}`, `{{height}}`, `{{count}}` | numbers (the whole string must be the placeholder) |
| `{{model}}` | the model field |

Every file the workflow saves (SaveImage, video combine nodes, …) is brought back. Previews and
temporary images aren't. Set a workflow to make video with ComfyUI.

## Keys

API keys are kept in `secrets.json` in CreativeCraft's config folder, readable only by you, and
never in `settings.json`. Removing a service removes its key. Keys are never shown to agents. If a
service quotes your key back in an error message, it is replaced with `[your key]` before anyone
sees it. From the command line, `generate key <id>` reads the key from standard input so it stays
out of your shell history.

## Agents

The hub has three tools: `generation_providers` lists your services (never their keys), and
`generate_image` / `generate_video` make files and return where they were saved. They use your
default service unless the agent names one. To stop agents generating, turn off **Agents can
generate** in Settings. Their tool descriptions tell agents that cloud services bill you, and that
they should only generate what you asked for.

## Saved files

```text
<projects>/Generated/2026-10-09/a-lighthouse-on-a-cliff-at-dusk-103012.png
<projects>/Generated/2026-10-09/a-lighthouse-on-a-cliff-at-dusk-103012.png.json
```

The `.json` beside each file records the prompt, what to avoid, the service, the model, the
shape, the seed and when it was made (UTC). Right-click a result on the Generate page and choose
**Use this prompt** to start again from it.

## Command line

```sh
creativecraft-cli generate presets                       # services you can add
creativecraft-cli generate add comfyui                   # or openai, google, replicate, fal, custom…
creativecraft-cli generate add custom --url http://my-server:8080/v1 --image-model my-model
creativecraft-cli generate key openai                    # paste the key
creativecraft-cli generate test openai
creativecraft-cli generate default video fal
creativecraft-cli generate image "a lighthouse at dusk, oil painting" --aspect landscape --count 2
creativecraft-cli generate video "slow drone shot over a misty forest" --seconds 8 --provider fal
```

## Troubleshooting

| What you see | What to do |
|---|---|
| "… isn't answering at http://127.0.0.1:8188: is it running?" | Start ComfyUI (or the WebUI), or fix the address. |
| "the key wasn't accepted" (401/403) | Copy the key again. Some models also need access turned on in the service's console. |
| 402 / "billing" | Add credit or a payment method to your account with that service. |
| 404 for a model | The model was renamed or retired. Look up its current name and change the model field. |
| 429 | Rate limit or quota reached. Wait a moment, or check your plan. |
| "start it with --api" | Restart Stable Diffusion WebUI with the `--api` flag. |
| "… has no checkpoints yet" | Put a model in ComfyUI's `models/checkpoints` folder. |
| "gave up waiting" | The service is slow; the job may still finish there. Check the service's own dashboard. |
| A refusal message | The service declined the prompt. Its own explanation is shown. |
