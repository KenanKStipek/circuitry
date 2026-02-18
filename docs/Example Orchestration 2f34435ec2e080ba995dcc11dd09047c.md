# Example Orchestration

```
# Circuitry Example (Simple, Relatable): Page → Characters → Per-Character Beats → New Stories
# -----------------------------------------------------------------------------
# Goal:
# 1) Loop each page from initial state (state.input.book.pages)
#    - extract characters on that page
#    - store per-page character lists to state
# 2) After all pages:
#    - normalize characters into a unique canonical list
#    - for each character:
#        a) gather the pages they appear in
#        b) summarize that character’s perspective + extract story beats
#        c) write a NEW short story for that character, page-by-page,
#           using those beats and only the pages they appear in
#        d) length scales with how present the character is (by page count)
#
# Types used (kept simple):
# - Prime Dynamic
# - Dynamic (chain)
# - Loop (each, named)
# - Prompt (template, typed json)
# - Nested per-character Loop (each, named)
#
# -----------------------------------------------------------------------------

version: 1

defaults:
  model: gpt-4.1-mini
  provider: openai
  params:
    temperature: 0.35
  timeout_ms: 45000
  stop_on_error: true

prime:
  type: dynamic
  name: book_character_stories
  flow: chain
  effects:
    # -------------------------------------------------------------------------
    # 1) Per-page extraction: loop over pages and extract characters mentioned
    # -------------------------------------------------------------------------
    - type: loop
      name: per_page
      mode: each
      each:
        in: state.input.book.pages
        as: page
      body:
        - type: prompt
          name: extract_characters
          prompt_type: json
          schema:
            type: object
            properties:
              page_characters:
                type: array
                items:
                  type: object
                  properties:
                    name: { type: string }
                    aliases:
                      type: array
                      items: { type: string }
                  required: [name]
                  additionalProperties: false
            required: [page_characters]
            additionalProperties: false
          template: |
            Extract the characters mentioned on this page.
            Return JSON:
            { "page_characters": [ { "name": "...", "aliases": ["..."] } ] }

            Rules:
            - Include only characters that are actually mentioned or clearly present.
            - Use the most common name as "name".
            - Include nicknames in aliases if present.
            - If no characters are mentioned, return an empty array.

            Page text:
            {{page}}

    # -------------------------------------------------------------------------
    # 2) Normalize / dedupe character list across pages
    #    Also compute "presence" by counting how many pages each character appears in.
    # -------------------------------------------------------------------------
    - type: prompt
      name: normalize_characters
      prompt_type: json
      schema:
        type: object
        properties:
          characters:
            type: array
            items:
              type: object
              properties:
                id: { type: string }         # stable slug, e.g. "mara"
                name: { type: string }
                aliases:
                  type: array
                  items: { type: string }
                page_indexes:
                  type: array
                  items: { type: number }    # indexes where present
                presence_weight: { type: number } # 0..1 relative to max presence
              required: [id, name, page_indexes, presence_weight]
              additionalProperties: false
          notes: { type: string }
        required: [characters]
        additionalProperties: false
      template: |
        We extracted per-page character lists. Normalize into a single canonical set.

        Input:
        - A "character" may appear with aliases.
        - Merge duplicates and unify names.
        - Compute page_indexes for where each character appears (0-based index).
        - Compute presence_weight per character:
          presence_weight = (# pages character appears in) / (max # pages any character appears in)
          Clamp to 0..1.

        Output JSON:
        {
          "characters": [
            {
              "id": "stable_slug",
              "name": "Display Name",
              "aliases": ["..."],
              "page_indexes": [0,2,4],
              "presence_weight": 0.66
            }
          ],
          "notes": "optional"
        }

        Per-page extraction results:
        {{book_character_stories.per_page.extract_characters.value}}

    # -------------------------------------------------------------------------
    # 3) For each character:
    #    - gather the pages they appear in
    #    - summarize perspective + beats
    #    - write a NEW short story page-by-page (length scaled by presence)
    # -------------------------------------------------------------------------
    - type: loop
      name: per_character
      mode: each
      each:
        in: state.runtime.book_character_stories.normalize_characters.value.characters
        as: character
      body:
        # 3a) Gather that character's pages into one bundle (and compute target length)
        - type: prompt
          name: gather_pages
          prompt_type: json
          schema:
            type: object
            properties:
              character_pages:
                type: array
                items:
                  type: object
                  properties:
                    page_index: { type: number }
                    text: { type: string }
                  required: [page_index, text]
                  additionalProperties: false
              target_pages: { type: number }   # how many "new story pages" to write
              target_words_per_page: { type: number }
            required: [character_pages, target_pages, target_words_per_page]
            additionalProperties: false
          template: |
            You are preparing inputs for writing a NEW short story from a character's perspective.

            Character:
            {{character}}

            The full book pages (0-based index):
            {{state.input.book.pages}}

            Task:
            1) Select ONLY the pages whose index appears in character.page_indexes.
               Return them as character_pages: [{page_index, text}]
            2) Compute target_pages and target_words_per_page based on presence_weight:
               - target_pages = round( 2 + presence_weight * 6 )
                 (so minor characters ~2 pages, major ~8 pages)
               - target_words_per_page = 180 for short pages, but scale slightly:
                 target_words_per_page = round(160 + presence_weight * 60)
            Output JSON with character_pages, target_pages, target_words_per_page.

        # 3b) Summarize that character's perspective + beats (grounded in their pages)
        - type: prompt
          name: perspective_and_beats
          prompt_type: json
          schema:
            type: object
            properties:
              perspective_summary: { type: string }
              voice_notes:
                type: array
                items: { type: string }
              story_beats:
                type: array
                items:
                  type: object
                  properties:
                    beat_id: { type: string }
                    beat: { type: string }
                    emotional_shift: { type: string }
                  required: [beat_id, beat]
                  additionalProperties: false
            required: [perspective_summary, story_beats]
            additionalProperties: false
          template: |
            Build the character's perspective model from ONLY the pages they appear in.

            Character:
            {{character.name}} (aliases: {{character.aliases}})

            Pages they appear in:
            {{book_character_stories.per_character.gather_pages.value.character_pages}}

            Output JSON:
            - perspective_summary: 4-6 sentences describing what the character wants, fears, and notices
            - voice_notes: 3-6 bullet-like strings describing how they sound/think
            - story_beats: 5-10 beats, each with:
              - beat_id: "b1", "b2", ...
              - beat: one sentence
              - emotional_shift: optional (e.g. "hope -> dread")

        # 3c) Write the NEW story page-by-page, using the beats and target length
        - type: loop
          name: write_story_pages
          mode: each
          each:
            # We iterate over a synthetic range by asking the model to materialize an array of page numbers.
            # Keep it simple: we first create that array with a prompt.
            in: state.runtime.book_character_stories.per_character.write_story_pages_plan.value.page_numbers
            as: page_num
          body:
            - type: prompt
              name: write_page
              description: "Write one page of the new short story for this character."
              template: |
                Write page {{page_num}} of a NEW short story from {{character.name}}'s perspective.

                Constraints:
                - Use the voice notes.
                - Follow the story beats in order.
                - Stay consistent with the original pages (facts/setting), but this is a NEW story.
                - Write ~{{book_character_stories.per_character.gather_pages.value.target_words_per_page}} words.
                - End this page with a small hook (a question, reveal, or decision) unless it's the final page.

                Inputs:
                - Character pages (source grounding):
                {{book_character_stories.per_character.gather_pages.value.character_pages}}

                - Perspective summary:
                {{book_character_stories.per_character.perspective_and_beats.value.perspective_summary}}

                - Voice notes:
                {{book_character_stories.per_character.perspective_and_beats.value.voice_notes}}

                - Story beats:
                {{book_character_stories.per_character.perspective_and_beats.value.story_beats}}

                - Total pages to write:
                {{book_character_stories.per_character.gather_pages.value.target_pages}}

                Guidance:
                - If page_num is 1, establish setting + character desire.
                - If page_num is the final page, resolve the main tension and close emotionally.

        # 3c.0) Plan pages: materialize [1..target_pages] as an array for the each-loop above
        - type: prompt
          name: write_story_pages_plan
          prompt_type: json
          schema:
            type: object
            properties:
              page_numbers:
                type: array
                items: { type: number }
            required: [page_numbers]
            additionalProperties: false
          template: |
            Create a JSON object with page_numbers as an array from 1..N where N is:
            {{book_character_stories.per_character.gather_pages.value.target_pages}}

            Output:
            { "page_numbers": [1,2,3] }
```

Initial State (the only external input)

```
state:
  input:
    book:
      title: "Lanterns Over Brinewater"
      pages:
        - |
          Mara kept her hands deep in the pockets of her salt-stiff coat as the docks creaked awake.
          Brinewater always smelled like kelp and copper coins, and today it smelled like change.
          A shipment had arrived before dawn—unmarked crates, guarded by strangers with bright clean boots.
          Jonah, the lighthouse keeper’s son, watched from the end of Pier Nine, pretending not to.
          “If you stare harder,” Mara whispered, “they’ll charge you rent.”
          Jonah didn’t smile. He was listening, the way he always listened, like the sea might confess something.

        - |
          Old Tamsin said the lantern festival was a promise the town made to itself—one night a year,
          when every window glowed and every grudge got tired.
          Mara loved the festival for the same reason she feared it: light revealed what dark could hide.
          At the market, she heard the rumor twice: someone was buying deeds.
          Not homes—land. Tide-cut land. The kind that vanished and returned and belonged to no one for long.
          Jonah found her by the lantern stall. “My father saw a ship,” he said. “No flag. No name.”
          Mara’s stomach sank. “That’s not a ship,” she said. “That’s a question.”

        - |
          That night the tide came in wrong—too fast, too sure of itself.
          Jonah ran to the lighthouse, lungs burning, to warn his father, but the lamp room was empty.
          The great lens was dark, and on the floor lay a single copper coin, freshly minted.
          Mara followed the shoreline until the sand turned to slick stones. She found the strangers there,
          prying open a crate with careful hands.
          Inside wasn’t contraband or treasure. It was a lantern, glass-clear and humming faintly,
          as if it had swallowed a small star.
          “Don’t touch it,” Jonah said, arriving breathless.
          Mara touched it anyway.

        - |
          The lantern’s light didn’t spill—it pulled.
          The sea stilled as if listening. The wind turned its face away.
          Mara saw her mother’s hands, remembered at once and too late: stained with brine, shaking over papers.
          Jonah saw his father’s silhouette far out on the rocks, waving, or drowning.
          The strangers spoke finally. “Brinewater is on a seam,” one said, voice flat as slate.
          “This lantern holds it shut.”
          Mara’s fingers tightened around warm glass. “Then why bring it here?”
          The stranger looked at her like she’d asked why storms existed. “Because you’re the only ones who can keep it.”

        - |
          Lantern night arrived with a sky the color of old bruises.
          The town hung lights anyway. They always did.
          Mara stood on Pier Nine with the humming lantern hidden under her coat, heartbeat syncing with its pulse.
          Jonah stood beside her, wet hair plastered to his forehead, eyes fixed on the black horizon.
          “We can’t close a seam with wishes,” Jonah said.
          Mara thought of deeds being bought, of land that belonged to everyone and no one, of promises that rotted in ink.
          “No,” she said. “But we can close it with a story people agree to live inside.”
          When she lifted the lantern, every window in Brinewater flared bright—too bright—and the tide held its breath.

  runtime: {}
```