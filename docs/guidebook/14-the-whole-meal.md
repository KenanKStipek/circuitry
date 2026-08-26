# The whole meal

Every part of the language, in one document. Two files: the dinner party, and the sauce sub-recipe it calls.

```yaml
# make_sauce.yml — the child: a typed interface and one prompt
interface:
  inputs:
    base: {type: string, required: true, description: The dish the sauce accompanies.}
    style: {type: string, required: false}
  outputs:
    recipe: {type: string, path: prime.compose.value}

effects:
  - type: prompt
    name: compose
    template: "Compose a {{input.style}} sauce for {{input.base}}, in three sentences."
```

```yaml
# dinner_party.yml
# Inputs: occasion (required), diet, recipe_url. Output: prime.plate.value
interface:
  inputs:
    occasion: {type: string, required: true}
    diet: {type: string, required: false}
    recipe_url: {type: string, required: false}
  outputs:
    dinner: {type: string, path: prime.plate.value}

effects:
  # tool — fetch inspiration; no internet, still dinner
  - type: tool
    name: fetch_recipe
    provider: web_fetch
    params: {url: "{{input.recipe_url}}"}
    on_error: skip

  # if — branch on the guest list; both branches write `main_course`
  - type: if
    name: check_diet
    if: {mode: cel, expr: "state.input.diet == 'vegetarian'"}
    then:
      - type: prompt
        name: main_course
        template: "Suggest a vegetarian main course for {{input.occasion}}, in one sentence."
    else:
      - type: prompt
        name: main_course
        template: "Suggest a main course for {{input.occasion}}, in one sentence."

  # use — the sauce sub-recipe; its interface supplies the output mapping
  - type: use
    name: sauce
    path: ./make_sauce.yml
    inputs: {base: "{{prime.check_diet.main_course.value}}", style: pan}

  # dynamic — the menu, planned in a named scope; typed output feeds the loop below
  - type: dynamic
    name: menu
    flow: chain
    effects:
      - type: prompt
        name: plan_courses
        prompt_type: array
        schema: {type: array, items: {type: string}}
        template: |
          List three courses around {{prime.check_diet.main_course.value}}.
          Inspiration, if any: {{{prime.fetch_recipe.value}}}
          Return ONLY a JSON array of strings.
      - type: prompt
        name: wine
        template: "Pick one wine for this menu: {{prime.menu.plan_courses.value}}"

  # loop (each) — cook every course in parallel, collect the steps
  - type: loop
    name: courses
    flow: tree
    each: {in: prime.menu.plan_courses.value, as: course}
    collect: cook
    body:
      - type: prompt
        name: cook
        template: "Write the cooking steps for: {{course}}"

  # loop (while) — season, taste, adjust; the model is the palate
  - type: loop
    name: season
    while:
      mode: model
      template: "Taste this. Does the seasoning need adjusting?\n\n{{prime.adjust.value}}"
    min_iterations: 1        # always taste at least once, so `last` exists below
    max_iterations: 3
    body:
      - type: prompt
        name: adjust
        template: "Adjust the seasoning of {{prime.check_diet.main_course.value}} and describe the dish now."

  # reflector — plan the final hour of service live, from a stated goal
  - type: prompt
    name: goal
    template: |
      State the goal for the final hour of service at {{input.occasion}}, in one sentence,
      given these courses: {{prime.courses.collected.value}}
  - type: reflector
    name: replan_service
    max_effects: 4
    effects:
      - type: prompt
        name: propose_steps
        template: "Plan the final hour of service. Output must follow the OUTPUT CONTRACT exactly."

  # plate — one prompt reads across everything the run produced
  - type: prompt
    name: plate
    template: |
      Write tonight's menu card.
      Main: {{{prime.season.last.adjust.value}}}
      Sauce: {{{prime.sauce.value.recipe}}}
      Wine: {{{prime.menu.wine.value}}}
      Courses: {{prime.courses.collected.value}}
```

```bash
cof check dinner_party.yml
cof run dinner_party.yml -e occasion=anniversary -e diet=vegetarian --live-state dinner.live.json
```

## Reading the run

`dinner.live.json` is the state graph filling in as dinner cooks. When it is done, the shape is exactly what the names dictate:

```
input.occasion                                  "anniversary"
input.diet                                      "vegetarian"
prime.fetch_recipe.value                        null — skipped; meta.error says why
prime.check_diet.value.branch                   "then"
prime.check_diet.main_course.value              the main course
prime.sauce.value.recipe                        the sauce, via the child's interface
prime.menu.plan_courses.value                   ["…", "…", "…"]
prime.menu.wine.value
prime.courses.iter_0.cook.value … iter_2        one pass per course, cooked in parallel
prime.courses.collected.value                   the three sets of steps, in menu order
prime.courses.value.termination.reason          "collection_exhausted"
prime.season.iter_0.adjust.value …              each tasting pass
prime.season.last.adjust.value                  the final one
prime.goal.value                                what the planner was told
prime.replan_service.generated.iter_0.…         whatever the planner decided to run
prime.plate.value                               the menu card
runtime.effective_settings.sources              where every setting came from
```

Every effect's `meta` sits beside its value — which model, why, the rendered prompt, tokens, timing, any fallback that answered.

## Turning on the complexity layer

Nothing in the document changes. In config:

```json
{
  "runtime": {
    "complexity": {
      "scoring": {"enabled": true},
      "routing": {
        "enabled": true,
        "bands": [
          {"name": "light", "max": 20, "model": "phi3:mini"},
          {"name": "mid",   "max": 40, "model": "qwen2.5:7b-instruct"},
          {"name": "heavy",            "model": "gpt-oss:20b"}
        ]
      },
      "decomposition": {"enabled": true, "threshold": 45, "max_chunks": 5, "max_depth": 1}
    }
  }
}
```

```bash
cof score dinner_party.yml
cof run dinner_party.yml -e occasion=anniversary --explain-routing --decompose-out ./plans
```

The cheap prompts — `wine`, `cook` — route to the small model; `plate`, reading across the whole run, scores higher and routes up; and if `plan_courses` ever grows past the threshold, it decomposes into courses of its own and merges back at `prime.menu.plan_courses.value`, where the loop reads it unchanged.

## What the meal demonstrates

| Chapter | In the document |
| --- | --- |
| [Prompt](01-prompt.md) | typed output (`plan_courses` → array), triple-stache for prose |
| [Dynamic](02-dynamic.md) | `menu` chain; `courses` fans out with `flow: tree` |
| [State](03-state.md) | every path derived from names; `input.` / `prime.` / `runtime.` |
| [Configuration](04-configuration.md) | no `adapter:` or `model:` anywhere in the document |
| [Errors](05-errors.md) | `on_error: skip` on the optional fetch; a template written for an empty value |
| [If](06-if.md) | `check_diet`, same name in both branches |
| [Loop](07-loop.md) | `each` with `collect`; `while` with the model as sensor; `last` after the loop |
| [Reflector](08-reflector.md) | a `goal` effect, then a bounded planner |
| [Composition](09-composition.md) | `make_sauce.yml` with an interface; outputs auto-generated |
| [Complexity](10-complexity.md), [Decomposition](11-decomposition.md) | switched on in config, invisible to the document |
| [Surfaces](12-surfaces.md) | `--live-state`, `--explain-routing`, `--decompose-out` |
| [Tools](13-tools-and-persistence.md) | `web_fetch` as an effect like any other |

An orchestration is a declared control mechanism which, through reflectors and decomposition, lays its own plans. Dinner is served.
