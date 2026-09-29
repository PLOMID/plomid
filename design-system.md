# PLOMID design system

The canonical reference for how the PLOMID site looks, moves and behaves. Everything here
describes the **actual implementation** in `src/styles/global.css` `src/scripts/site.ts` and the
components under `src/components/`. If you change a token or a pattern, change it here in the same
commit.

Scope: static Astro 5 site. No client framework, no runtime dependencies, no tracking.

---

## 1. Brand

**PLOMID** makes unified data infrastructure for applications and AI clients: one data layer that
holds structured rows, documents and time-ordered events, with one planning and execution path
across all of them. Whoever asks, an application, an AI client, a service or a background job,
reaches the same layer through the same interfaces.

| | |
| --- | --- |
| Tone | Precise, technical, unhyped. States status plainly. |
| Visual personality | Calibrated and instrumented, closer to a measuring tool than a marketing page. |
| Metaphor | The **plumb line**: verticality, alignment, calibrated measurement. |
| Voice | Second person, present tense. Short sentences. No superlatives. |

### Design principles

1. **Two worlds, one accent.** Ink and paper surfaces alternate by section; copper is the only
   accent and it always means *this is the live part*.
2. **Hairlines do the work.** Structure is drawn with 1px rules and grids, never with boxes and
   shadows. Depth is a last resort.
3. **The interface is the product.** Diagrams, consoles and code are real surfaces with chrome, not
   illustrations pasted onto a page.
4. **Status is communicated, not announced.** Solid = available today. Dashed / ghosted = roadmap.
   One legend per section is enough; the design carries the rest.
5. **Motion is atmosphere, not decoration.** Ambient layers drift. Nothing blocks the pointer.
6. **Honesty is a design constraint.** Nothing in development may look usable. No invented
   customers, numbers, benchmarks or dates.
7. **Whitespace frames something.** A large empty region must hold a visual, create hierarchy or
   let type breathe. Otherwise it gets smaller.

---

## 2. Colours

Defined once in `:root` (`src/styles/global.css`). Components never hardcode a colour, they read
the variables set by the active tone.

### Surfaces

| Token | Value | Use |
| --- | --- | --- |
| `--ink` | `#08090b` | Primary dark surface (`data-tone="dark"`) |
| `--ink-2` | `#0c0e11` | Slightly raised ink |
| `--graphite` | `#14171b` | Mid surface (`data-tone="graphite"`) |
| `--graphite-2` | `#1c2026` | Raised graphite |
| `--paper` | `#f3f1ec` | Light surface (`data-tone="light"`) |
| `--paper-2` | `#e9e6df` | Raised paper |

### Text

| Token | Value | Use |
| --- | --- | --- |
| `--t-d` | `#f2f2ef` | Primary text on dark |
| `--t-d2` | `#a0a6ae` | Secondary text on dark |
| `--t-d3` | `#6b7178` | Tertiary / labels on dark |
| `--t-l` | `#0a0b0d` | Primary text on light |
| `--t-l2` | `#4b5057` | Secondary text on light |
| `--t-l3` | `#83888f` | Tertiary / labels on light |

### Hairlines

| Token | Value |
| --- | --- |
| `--line-d` | `rgba(255,255,255,.10)` |
| `--line-d2` | `rgba(255,255,255,.20)` |
| `--line-l` | `rgba(10,11,13,.12)` |
| `--line-l2` | `rgba(10,11,13,.24)` |

### Accent and data-model identity

| Token | Value | Meaning |
| --- | --- | --- |
| `--accent` | `#e2683c` | Copper. Live, active, current. |
| `--accent-2` | `#f0925f` | Copper light (hover, text on dark) |
| `--accent-ink` | `#2a0f04` | Copper on ink |
| `--accent-08/14/30` | copper at 8 / 14 / 30 % | Fills, rings, borders |
| `--sql` | `#e2683c` | SQL |
| `--json` | `#5c93d6` | JSON / documents |
| `--time` | `#3fa98c` | Time series |
| `--future` | `#6b7178` | Anything in development |

### Semantic states

Three states, one vocabulary: `available` `building` `future` (`src/data/site.ts`).

| State | Dot (`.dot[data-status]`) | Treatment classes |
| --- | --- | --- |
| `available` | filled copper + `0 0 0 3px --accent-14` halo | `.is-now`, solid `--accent` border `--accent-08` fill |
| `building` | hollow, 1px `--fg-2` ring | `.is-ghost`, dashed border, hatched fill `--fg-2` text |
| `future` | hollow, 1px dashed `--fg-3` | `.is-ghost`, plus a dotted frame where a third tier is needed |

Rule: **dashed always means "not available yet"**. Solid always means "available today". A selected
roadmap element may take the copper accent, but it keeps its dashed frame, selection must never
make something on the roadmap look shipped.

The `.status` chip (dot + label) is the **exception**, reserved for surfaces whose whole purpose is
status: the roadmap tables, the architecture layer detail, the footer legend. Everywhere else the
state is carried by the treatment, and `.key`, a one-line swatch legend, teaches the language
once per section.

`.dot` `.is-now` `.is-ghost` and `.key` all live in `global.css` and read the active tone.

### Tones

Every section opts into a tone with `data-tone="dark|graphite|light"`. The tone sets `--bg` `--fg` `--fg-2` `--fg-3` `--line` `--line-strong` `--panel` `--panel-2` and `--accent-on`.
No component below the tone layer references a raw palette token.

`--accent-on` exists because copper needs different contrast values per surface:
`--accent-2` on dark/graphite `#b8481d` on light.

---

## 3. Typography

Two families, loaded from Google Fonts with `display=swap`:

| Token | Stack |
| --- | --- |
| `--sans` | `'Inter', -apple-system, BlinkMacSystemFont, 'Segoe UI', system-ui, sans-serif` |
| `--mono` | `'IBM Plex Mono', ui-monospace, 'SFMono-Regular', Menlo, monospace` |

Body: 16px / 1.6. Inter features `ss01` and `cv11` are on.

### Scale

| Role | Size | Line height | Letter spacing | Weight |
| --- | --- | --- | --- | --- |
| `.display` | `clamp(2.6rem, 7.4vw, 6.1rem)` | `0.94` | `-0.045em` | 600 |
| `.display` ≤460px | `clamp(2.05rem, 9.6vw, 2.7rem)` | `0.99` | `-0.035em` | 600 |
| hero `.display` | `clamp(2.5rem, 5.6vw, 5.1rem)` | `0.98` | `-0.042em` | 600 |
| hero `.display` ≤460px | `clamp(2.05rem, 9.4vw, 2.6rem)` | `1.02` | `-0.034em` | 600 |
| `h2` / `.h2` | `clamp(1.9rem, 4.1vw, 3.35rem)` | `1.04` | `-0.035em` | 600 |
| `h2` ≤460px | `clamp(1.7rem, 8vw, 2.2rem)` | `1.04` | `-0.028em` | 600 |
| `h3` / `.h3` | `clamp(1.15rem, 1.7vw, 1.5rem)` | `1.04` | `-0.02em` | 600 |
| `h4` | `1.0625rem` | `1.04` | `-0.01em` | 600 |
| `.statement-text` (home premise) | `clamp(1.65rem, 3.1vw, 2.55rem)` | `1.08` | `-0.034em` | 600 |
| `.fw-headline` | `clamp(1.25rem, 2vw, 1.7rem)` | `1.04` | `-0.03em` | 600 |
| `.lede` | `clamp(1.0625rem, 1.4vw, 1.3125rem)` | `1.55` | `-0.012em` | 400 |
| `.body-2` | `1rem` | `1.6` |, | 400 |
| `.mono` | `0.6875rem` |, | `0.17em`, uppercase | 500 |
| `.mono-plain` | `0.75rem` |, | `0.02em`, sentence case | 400 |
| prose `p` | `1rem` | `1.72` |, | 400 |
| `.code pre` | `0.8125rem` | `1.5` |, | 400 |
| console code (`.qc-code`) | `0.75rem` | `1.45` |, | 400 |
| SVG labels (`.vt`) | `10px` |, | `0.12em`, uppercase |, |
| SVG hashes | `9px` |, | `0.08em` |, |

Headings are `font-weight: 600` `letter-spacing: -0.03em` `line-height: 1.04` and
`text-wrap: balance`. Body paragraphs use `text-wrap: pretty`.

### Emphasis

Personality comes from *deliberate* mixes of weight, italic and mono inside headlines, never from
styling every sentence.

| Class | Effect | Example |
| --- | --- | --- |
| `.w-strong` | 700 `-0.05em` | The **unified** data infrastructure |
| `.q` (on `<em>`) | italic, 450 `-0.03em`, inherits colour | for *applications and AI clients* |
| `.w-mono` | inline mono `0.6em`, copper | mono kicker inside a headline |
| `em.hl` | normal style `--accent-on` | inline highlight |
| `.quiet` / `.dim` | `--fg-3` | de-emphasised text |

A line may combine exactly one of these. `.display .q` `.h2 .q` and `.statement-text .q` inherit
`color` so the emphasis reads as voice rather than link styling.

### Voice

- Sentence case in headings and links; uppercase only in `.mono` labels.
- Second person, present tense.
- Roadmap language only for unfinished work: *roadmap*, *future direction*, *being explored*,
  *planned*. Never *in development* repeated as a badge on every surface.
- The word **available** appears once per section at most.

---

## 4. Spacing

### Scale

There is no numeric spacing scale, spacing is expressed with `clamp()` so it is fluid and
self-documenting. The recurring values:

| Token | Value | Use |
| --- | --- | --- |
| `--gutter` | `clamp(20px, 4vw, 44px)` | Container inline padding |
| `--nav-h` | `66px` | Fixed header height; drives `scroll-padding-top` |
| Section padding | `clamp(64px, 7.6vw, 124px)` | `.section` block padding |
| Tight section | `clamp(56px, 6vw, 96px)` | `.section-tight` |
| Head gap | `clamp(14px, 1.6vw, 22px)` | `.sec-head` internal gap |
| Head margin | `clamp(38px, 4.4vw, 68px)` | `.sec-head` bottom margin |
| Grid gap | `clamp(20px, 2.6vw, 44px)` | `.cols` |
| Row padding | `clamp(20px, 2.6vw, 36px) 0` | `.row` |
| Micro | 4 · 6 · 8 · 10 · 12 · 14 · 16 · 18 px | Inside components |

`--radius` is `2px`. Nothing on the site is more rounded except the status chip (`999px`) and
cursor-following dots (`50%`).

### Container widths

| Token | Value | Use |
| --- | --- | --- |
| `--max` | `1280px` | `.container` |
| `--max-wide` | `1560px` | `.container-wide` (diagram bleed) |

### Reading widths

Long lines never run the full container:

- `.lede` → `56ch` (`.sec-head .lede` → `52ch`, hero lede → `48ch`)
- `.body-2` → `62ch`
- `.prose` → `72ch`; prose list items → `60ch`-ish via padding
- `.statement-text` → `20ch`; `.statement-lead` / `.statement-copy` → `46ch` (`52ch` when stacked)
- `.col-item p` → `46ch`
- `.row-r p` → `60ch`
- `.note` → `82ch`; `.fw-rule` → `62ch`
- `.console-note` → `74ch`
- `.hero-rail-line` → `76ch` (`60ch` at ≤768px)
- `.fw-honest` → `46ch`
- `.ms-copy h3` and `.fw-headline` → `22–24ch`
- `.ql-summary` → `74ch`; `.ql-points` items are unmeasured inside an `auto-fit` grid
- `.access-step p` and `.access-facts dd` → `40ch`
- `.mvcc-statement` → `22ch`; `.mvcc-note` → `52ch`; `.mvcc-points p` → `44ch`
- `.tier-meaning` → unmeasured (a legend cell, not body copy)

When a section needs a statement *and* support, both live inside these measures and the remaining
width is deliberate breathing room, never a long line running across the page.

---

## 5. Layout

### Breakpoints

| Width | What changes |
| --- | --- |
| `1560px` | `.container-wide` available for diagrams |
| `1180px` | Brand sub-label hides; dropdown nav collapses to the drawer toggle; HeroSystem plate loses its offset shadow plate |
| `1100px` | Convergence drops the pinned scroll drive and renders resolved, at natural height (mesh, movers and apps hidden; resolved plate + vertical stages) |
| `1080px` | Hero splits to one column; `.hero-visual` capped at `680px`, centred |
| `1024px` | `.ms-body` stacks: model nodes become a 3-up grid directly above the stage |
| `1000px` | `.statement-grid` `.ls` (layer stack) go single column |
| `980px` | `.fw-panel` stacks; `.sf-grid` stacks |
| `940px` | `.ph-inner` (page hero) stacks; `.cols-2 / --3` collapse |
| `900px` | `.sec-head--split` collapses; console panes stack; footer grid → 2 columns |
| `860px` | `.nu-grid` (NextUp) → 1 column |
| `840px` | `.row` (hairline rows) → 1 column |
| `820px` | Hero status rail → 1 column; home roadmap → 1 column |
| `768px` | Hero rail measure narrows; `.ls-canvas` diagram gets a `600px` minimum inside its own scroller |
| `700px` | Code blocks shrink; spec table drops 3rd column; `.ms-canvas-body` diagram gets `520px`; StorageFlow gets `560px`; `.fw-tabs` stack |
| `620px` | HeroSystem paddings and micro-type; blob object list → 2 columns |
| `559px` | Model nodes → 2-up grid |
| `520px` | Convergence plate gets a `520px` minimum inside the stage scroller |
| `460px` | hero `.display` steps down; buttons go full width; blob objects → 1 column; hero chips shrink; `.access-flow` → 1 column |
| `400px` | HeroSystem label micro-type |

Compositions added by later passes, listed in the same order:

| Width | What changes |
| --- | --- |
| `940px` | `.explore-grid` (homepage journey cards) → 2 columns |
| `900px` | `.ql-flow` → 4 columns; `.tier-legend` → 2 columns; `.sc-map` stacks; `.mvcc-body` and `.mvcc-points` → 1 column |
| `880px` | `.rs-explore` (roadmap explore band) → 1 column |
| `840px` | `.access-flow` → 2 columns; `.access-facts` → 1 column |
| `780px` | `.deploy-map` stacks, the rail turns horizontal |
| `700px` | `.sf-flow` (storage stage strip) → 2 columns |
| `620px` | `.fabric-targets` → 2 columns |
| `560px` | `.ql-flow` → 2 columns |
| `520px` | `.tier-legend` → 1 column; `.infra-grid` → 1 column; `.explore-grid` → 1 column |

### Diagram legibility on small screens

A 660-unit diagram rendered into a 320px column scales its labels to ~4px. Rather than delete the
visual, the frames pan inside themselves at a readable minimum width, the same rule the site uses
for wide tables and code:

| Frame | Minimum width | Breakpoint |
| --- | --- | --- |
| `.ms-canvas-body` (model visuals) | `520px` | ≤700px |
| `.ls-canvas` (layer stack) | `600px` | ≤760px |
| `.sf-stages` (storage flow) | `560px` | ≤700px |
| `.conv-stage` (convergence) | `520px` | ≤1100px |

Each of these is an `overflow-x: auto` container, so the **page** never scrolls sideways, the
diagram does. The verification probe skips elements inside a horizontal scroller for exactly this
reason.

### Grid rules

1. Every multi-column grid template uses `minmax(0, 1fr)` for its tracks. This is what stops a
   long word or a wide SVG from blowing out the layout.
2. `.section` `.container > *` `.cols > *` `.grid > *` are all forced to `min-width: 0`.
3. Two-column section headers use `.sec-head--split` at `1.15fr / 0.85fr`, aligned on the baseline
   (`align-items: end`).
4. Editorial split: `.cols-2` `.cols-3` with `.cols-top` for `align-items: start`.
5. Page hero: one measured column by default; `0.95fr / 1.05fr` (copy left, product visual
   right, `align-items: center`) when the page passes a hero visual through `slot="visual"`.
   The split collapses to one column at `1080px` and the visual then centres at a `680px` cap.
6. Any wide child (`pre` `table`) lives inside `.table-wrap` / `.scroll-x`, which scroll
   horizontally inside its own frame. The page itself never scrolls horizontally
   (`body { overflow-x: hidden }` is a backstop, not the mechanism).
7. Text and a product visual never share a grid cell. If they must be adjacent, they are separate
   columns with a `min-width: 0` guarantee and an explicit stack breakpoint.

### Mobile composition

Order is recomposed, not removed: copy → primary CTA → product visual. Visual components keep their
content at small sizes; they drop labels and tighten padding rather than disappearing.

The two scroll-driven regions are recomposed rather than shrunk:

- **Convergence** keeps its sticky 200vh drive at ≥1101px. Below that the section takes its natural
  height and renders the *resolved* diagram immediately `--sp` is pinned to `1` on the head and
  stage, the cross-fading captions collapse to the final one, and the three model chips stay
  selectable. Less scrolling, same information, no pinned theatre on touch.
- **Model showcase** keeps the vertical rail beside a sticky stage at ≥1024px. Below that the rail
  becomes a 2–3-up grid of compact nodes directly above the stage, so a selection always answers
  within the same viewport.

### Text and visuals never share a cell

If copy and a product visual must sit adjacent they are separate grid columns with
`minmax(0, …)` tracks and an explicit stack breakpoint. The hero headline in particular never uses
`white-space: nowrap`; its emphasis underline is `text-decoration`, which wraps correctly at any
measure and therefore can never overhang the neighbouring column.

---

## 6. Components

### Structural

| Component | File | Purpose |
| --- | --- | --- |
| `Base` | `layouts/Base.astro` | HTML shell, SEO/OG tags, fonts, header/footer `data-js` flag, optional FAQPage structured data |
| `Header` | `components/Header.astro` | Fixed nav: Product / Solutions / Industries / Developers / Company, bottom-line active state (see §11), search trigger, scroll progress hairline, mobile drawer as a flat always-expanded directory. The **Developers** dropdown carries only technical resources (Developers, Docs, Playground, GitHub); **Blog** and **Press & Brand** are organisation surfaces and live under **Company** |
| `Footer` | `components/Footer.astro` | Closing CTA, then **one line**: the brand anchor (PLOMID + company line + origin line + socials) beside **four link groups** (Product / Developers / Industries / Company — footer-short industry labels, Industries closed by one quiet *Explore industries →*), contact row, legal bar (copyright in full white), over its own quiet atmosphere (faint engineering grid + `Atmosphere` particles + one pointer light). **No status legend**, that lives on the roadmap page. **No Solutions group**, that stays in the navbar, the drawer and /solutions/ |
| `PageHero` | `components/PageHero.astro` | Inner-page hero: breadcrumb trail derived from the route registry, an optional statement line under the `h1`, eyebrow, lede, actions, an optional hero visual (`slot="visual"`), an optional page backdrop (`slot="backdrop"`), and an optional ambient field (`ambient`, drifting signals + one pointer light). **No facts panel** |
| `NavIcon` | `components/NavIcon.astro` | The one icon family: sixty line glyphs in a 24-unit box, `currentColor`, `aria-hidden` unless labelled. Includes ten environment marks named for what they depict: `plane` (aerospace), `twin` (digital twins), `truck` (logistics), `tower` (telecom), `mining` (pickaxe), `oil` (droplet), `robot` (arm), `cart` (retail), `money` (e-commerce), `mesh` (IoT) |
| `DomainVisual` | `components/DomainVisual.astro` | The hero drawing on industry and solution pages: sources → PLOMID layer → outcomes, in eleven archetype geometries |
| `DomainBackdrop` | `components/DomainBackdrop.astro` | The subject's background geometry for a page (contour, mesh, rails, field, frames), one slow drift |
| `DataFlow` | `components/DataFlow.astro` | The four-column band in two modes: `landscape` (systems → data → layer → work) and `architecture` (workloads → models → layer → surfaces) |
| `WorkloadIndex` | `components/WorkloadIndex.astro` | "One environment, many workloads": rows that open their detail in place, each with its own drawn mark |
| `WorkMark` | `components/WorkMark.astro` | The six small signal marks used beside use cases and outcomes |
| `SectionRail` | `components/SectionRail.astro` | The sticky in-page navigation on long product pages, with a scroll-spy indicator |
| `MegaMenu` | `components/MegaMenu.astro` | The names panel behind the **Industries** and **Solutions** nav items: six industries / four families, each a destination heading with its one-line context and its rows (environments or solutions) as names with marks. Markup only, styled in `global.css` with the rest of the navigation. One component, two datasets |
| `IndustryExplorer` | `components/IndustryExplorer.astro` | The `/industries/` **industry map**: six industry nodes over one bus resolving into the data layer, each node opening a panel of environments, shapes, workloads, solutions and capabilities. Server-rendered panels; the only script is the tablist |
| `IndustryTopology` | `components/IndustryTopology.astro` | One industry drawn as its environments over one data layer (root → environments → layer), in `hero` (compact) and `section` (full band) modes. HTML + CSS connectors, so every environment is a real link; hover/focus lights that environment's own wire and node in copper |
| `PrimaryIndustry` | `components/PrimaryIndustry.astro` | The body of a primary industry page (`/industries/industrial/` etc.): hero topology, environments band, data landscape, workload index, architecture band, derived capabilities and recurring solutions, the environment directory, closing band |
| `IndustryDiagram` | `components/IndustryDiagram.astro` | The workload map used by both industry and solution pages: five compositions (`flow` `network` `graph` `documents` `sovereign`), a `role="tablist"` stage rail, data-type chips that light in both directions, and a read-out directly beneath the rail. Owns its own diagram palette (`.idg-frame` pins the paper scope and `--dgm-*`, exactly as `.dm-frame` does) |
| `RelatedLinks` | `components/RelatedLinks.astro` | The relation rail, used in both directions of the industry ⇄ solution graph and for platform capability lists. One implementation for related industries, related solutions and capabilities, so the three cannot drift |
| `NextUp` | `components/NextUp.astro` | Journey continuation, 3 linked cards, hairline-divided |
| `Search` | `components/Search.astro` | Site-wide ⌘K palette in the site's instrument language: ink panel, hairlines at `--radius`, one copper rule on the top edge (the seam's echo), sans input, results with category label + canonical route + description `--accent-08` selection with a copper left edge, key-legend rail; near-full-screen sheet on mobile |
| `SocialLinks` | `components/SocialLinks.astro` | The one social component: real icon links (GitHub, LinkedIn, YouTube, X) plus quiet non-link placeholders for channels without a URL (Discord, Slack, Facebook, Instagram, Twitch, Reddit). `community` / `upcoming` props opt into the placeholders |
| `Faq` | `components/Faq.astro` | Accessible disclosure FAQ: `button` in `h3` `aria-expanded` + `aria-controls`, small height/opacity step, Enter/Space native. **Strictly one open per instance**: opening a question closes its siblings, and clicking the open question closes it, the list reads as one line of inquiry. Each page carries its own set — home (product/capability), company (team/company), pricing, download (install), playground, sovereignty — never one shared block. Each set emits its own FAQPage JSON-LD via `Base` |
| `Pricing` | `pages/pricing.astro` | Corporate pricing: hero proof strip, three tiers with price anchors (Free / Custom / Usage-based, never figures), Community (available, solid, corner tick), Enterprise (available, primary, middle), Managed Cloud (building, dashed). Included-everywhere band, grouped comparison matrix (4 groups × 3 rows, tier headers isolate a column, rows open their reasoning in place), enterprise procurement band (3 steps + offer card), cloud roadmap panel (dashed frame). No invented prices, the probe asserts it; own FAQ (6); `next-up` routing |
| `Download` | `pages/download.astro` | The install surface: detector hero visual + Recommended + 9-artefact matrix in 4 OS groups + Verify/requirements + First run + FAQ + production band. All artefacts point at `github.com/plomid/plomid/releases` (`repoUrl`, never the org URL), with SHA256 checksums beside each file. No versions, sizes or dates stated here — the releases page is the source of truth. The probe asserts `pkgs` `suggested` `releaseLinks` and no fake CTA |
| `Design` | `pages/design.astro` | The identity page: wordmark samples, the four-colour palette with tokens, and the six writing/diagram rules. Links press enquiries to `/press` and reuse questions to the repo |
| `Atmosphere` | `components/Atmosphere.astro` | The `.atmos` data-atmosphere layer, deterministic drifting point signals (also used in the footer) |
| `GeoForms` | `components/GeoForms.astro` | Geometric atmosphere: a cropped arc, a coordinate crosshair or a partial grid at one section corner. Deterministic SVG, one mark per instance `pointer-events: none`, strength-capped per tone (see §9) |

### Product visuals

| Component | File | What it is |
| --- | --- | --- |
| `HeroSystem` | `components/HeroSystem.astro` | The system map: clients (AI clients, applications, services, jobs) → today's models → the PLOMID layer → roadmap models. No local glow and no grid, the hero's single `[data-ambient]` field is the only light. Runs its own ambient life: staggered wire signals, a pulsing bus node, storage cells ticking left to right and live dots breathing out of phase |
| `Convergence` | `components/Convergence.astro` | "How the pieces fit", **the product story, full screen** (the section owns the viewport under the fixed header via `padding-top: var(--nav-h)`): PLOMID **is** the database, never a layer around other stores. Six scroll phases over **520vh**, driven by `--d` (the raw `--sp` rescaled so the whole narrative completes at ~62% of the travel) — the remaining **~160vh is a pure CSS hold**: the resolved architecture stands pinned, explorable, with no scroll ever frozen. **Fragmented**: a full-plate mesh — sync lines joining **38 icon cells** (database cylinders, service blocks, queues, documents, SQL tables, JSON braces, time waves, vector tents, graphs, blobs, API rings, cache hexes, sized into the glyph geometry and each sitting exactly on its line endpoints; three fill tiers: white for data, copper for vector/graph/api/cache, ghost for plumbing) and **6 labelled copies**, with packet lights riding the edges; the **burden line** (`COPIES · PIPELINES · SYNC · REINDEX · DUPLICATED STATE`) is its own group so it stays readable while the mesh dissolves around it; **Converging**: the mesh dissolves while the core forms, and the **six labelled movers travel pixel-true into the core and dissolve into the model chips** (translucent boxes, never solid) — the structure is the mess, resolved; **Ways in**: five doors — the row itself is the bus, no line through them (SQL · Document · Time series · SDK · API·Tools) — with each app **L-routed to the door it actually uses** (ERP→SQL, Analytics→Documents, CRM/IoT→Time, AI APP→SDK, Agent→Tools), `fill: none` on every routed path (an open SVG path otherwise fills black); **Connected**: apps arrive only here — same state, not a copy each; **One foundation**, the **brightest act**: the reader band blooms on `.in` (copper-stroked box with a one-time drop-shadow bloom, `SERVES ANYTHING` in full white, each consumer word drawing its own copper underline on a stagger, brighter copper read-currents out of the core), DEPLOY ANYWHERE beneath (Docker/self-hosted solid, cloud/edge dashed, **no lines touching the band**), and the **foundation current** — a copper dash current circling the core and running down every app wire. The real **PLOMID emblem** (footer geometry) presides at the plate's top-right. A **phase legend panel** sits in the plate's top-left corner (ink panel, active mark lit — a legend where a legend belongs). A **phase-reactive background** (radial glow pools on the phase clocks, resting pool low-centre on the foundation band) breathes with the story. The left rail carries the phase captions, a **REPLAY THE STORY** control, and the **story rail** — all six phase sentences stacked in one fixed-height slot ("What changes in each step") with exactly the active phase's line lit. Roadmap protocols (Mongo wire, GraphQL, KV·Geo access) sit beside the core as dashed legends, never active doors. Status is treatment throughout, read from `site.ts`. **28 copper popups** (`[data-conv-tip]`, one shared script-positioned element, keyboard-focusable). Isolate-a-model idiom retained (hover/click, truth-layer answer in `[data-conv-note]`). Static resolved block below 1101px; below 641px a 6-stage vertical story. Reduced motion freezes packets, movers, current, glows and blooms |
| `ModelShowcase` | `components/ModelShowcase.astro` | **The data-model experience, the centre of the product.** Nine surfaces: `SQL · JSON · Key-Value · Time · Geo · Vector · Graph · Objects` and the **Hybrid** plate that shows what the eight add up to (one request holding several shapes, with objects as a smaller inner application). See §11"The data-model surface system" |
| `LayerPipeline` | `components/LayerPipeline.astro` | The system pipeline: the statement at the entry, the nine stages on one rail, and the selected stage's detail in place. Replaces the two oversized stack diagrams (a 760×520 SVG and a nine-node vertical rail) with three tight rows, same information, a third of the height, more interaction |
| `ArchGlimpse` | `components/ArchGlimpse.astro` | The homepage's one architecture picture, now **interactive**: nine hairline rows on a spine, the `Data` row filled copper. Hover/focus previews a stage, click pins it, ↑/↓ + Home/End walk the stack (roving `tabindex`, one tab stop), the read-out strip answers in place, and the copper signal leaves its loop and **holds on the inspected row**. One link out, the caption below the plate, never the plate itself |
| `ArchSystem` | `components/ArchSystem.astro` | `/architecture`'s system diagram: the request path down a left spine (Clients → Query → Parser → Planner → Execution → MVCC → Index → Storage → Data) with the planner's side-path fan to the right (streaming, time series, documents, vector, graph, storage engine — vector and graph keep the dashed treatment). Hover/focus previews, click pins, ↑/↓ + Home/End walk the spine, the read-out beside the diagram carries role/in/out, and one copper signal travels the spine **only while the plate is on screen and nobody is inspecting a node** (it does not exist under reduced motion). A **Back to the whole path** control releases a pin, and other sections select a stage here through the `plomid:arch-select` event |
| `StorageFlow` | `components/StorageFlow.astro` | Values packing into pages, pages grouping into blocks, plus an **interactive stage rail** (Write → Page → Block → Storage): hover/focus previews a stage, click pins it, and the SVG stage groups answer in sync. The SMIL arrow dots are removed under reduced motion; the dashed lines carry the direction |
| `QueryConsole` | `components/QueryConsole.astro` | Runnable sample query with staged plan output |
| `CodeBlock` | `components/CodeBlock.astro` | Syntax-highlighted figure with filename bar and copy button |
| `ModelGlyph` | `components/ModelGlyph.astro` | The recurring mark for each data model, one family, every size |

### Primitives

| Class | Meaning |
| --- | --- |
| `.surface` + `.surface-head` / `.surface-body` | Product chrome: hairline frame, mono title bar, quiet body |
| `.panel` | Neutral tinted block |
| `.lift` | Hover affordance: `translateY(-3px)` + long soft shadow `:active` settles back |
| `.btn` `.btn--primary` `.btn--ink` `.btn--sm` `.btn--lg` `.btn--block` | Buttons |
| `.tlink` | Text link with a hairline underline that turns copper and widens the gap on hover |
| `.status` | Status chip: dot + label. `data-status` drives the dot |
| `.dot` | The status dot alone, no label, read at a glance |
| `.is-now` / `.is-ghost` | The two status treatments: solid + live, or dashed + hatched |
| `.key` | One-line swatch legend teaching the treatment, once per section |
| `.legend` | Footer-level key built from `.status` chips |
| `.hint` | The small pulsing cue that marks a region as interactive |
| `.note` | Callout with a 2px copper left rule |
| `.hairline-list` | List where each item is topped by a hairline and a copper tick |
| `.rows` / `.row` / `.row-l` / `.row-r` | Two-column hairline row list |
| `.defs` | Definition list, copper mono term `--fg-2` body |
| `.spec` | Specification table, mono uppercase headers |
| `.cols` `.cols-2` `.cols-3` `.cols-top` | Grid helpers |
| `.prose` | Long-form text block for docs |
| `.code` | Code frame (see `CodeBlock`) |
| `.readout` `.band` `.panel` `.table-wrap` `.scroll-x` | Supporting primitives |
| `.sig` `.sig--v` `.sig--h` `.bars` | Signal rails and stored-data forms |
| `.grid-lines` `.grid-dots` `.plumb` `.plumb-tick` | Structural texture |
| `.atmos` (+ `components/Atmosphere.astro`) `[data-ambient]` | Atmosphere and pointer light |
| `.sr-only` `.skip` | Accessibility utilities |
| `[hidden]` | Global guard: `display: none !important`, so an author display value (a grid panel, an inline-flex link) can never defeat the attribute and leak every tab panel at once |

### Page-level compositions

Sections that are composed on one page rather than shared as a component. They follow the same
rules, tone variables, one accent, treatment instead of badges, so they read as part of the
product rather than one-off art direction.

| Where | Surface | What it is |
| --- | --- | --- |
| Home `10` | Get PLOMID | Three real entry points (playground, docs, GitHub) with a plain statement that no packaged download exists. No fake download button, ever |
| Home `11` | FAQ | `Faq` component with eight visitor questions answered only with what other pages state. Emits FAQPage structured data via `Base` |
| Home `12` | Community | "Explore the project. Follow the work." `SocialLinks community upcoming` in a bordered panel with the origin line and a quiet note that Discord/Slack wait for real URLs |
| Home `07` | Explore PLOMID | Four `.explore-card` destinations (playground, docs, architecture, roadmap) with a shared top-edge interaction mark. Deliberately **not** a second code sample |
| Home `08` | Deployment map | One drawn deployment: four **named** destinations (ON-PREM · EDGE · CLOUD · SECOND REGION) wired to a core that carries the storage contract; choosing a place lights its wire and runs a labelled pulse (`offset-path`), and the readout flips between REACHABLE and fabric language. Region boundary and multi-storage are **dashed frames around the core** (properties of the deployment, not destinations) and turn copper when their option is chosen, dimming the destination wires. Reduced motion keeps the lit wire and the words, minus the dot |
| Home `09` | Platform evolution | The three-bucket view, treatment-led with a `.key` legend and no status chips. |
| Home, | Architecture glimpse | `ArchGlimpse` between storage and the future note: the whole stack as one picture that *is* the link to `/architecture/`. See "The glimpse rule". |
| Roadmap `01` | The plan | Four moves on one horizon line, the capability map read by *when*, not by *what*. See "The plan and the map" below. |
| Architecture `03` | Query lifecycle | An 8-stage tab set (`role="tablist"`) whose detail panel sits directly beneath the flow, showing the fragment of the statement the selected stage reads, the stage's **why** (one quiet bordered line), in/out, and a **Next: <stage> →** affordance that advances the same tabset (7 of the 8 stages; Distribution, the roadmap stage, is the end of the line). A **transport rail** (restart / prev / play / next + `stage n / 8`) lets the visitor drive the walkthrough; the strip still walks itself while on screen and nobody is driving, and the first hover, focus or click hands control over for good. |
| Architecture `04` | Planning | The plan as an **explorable figure** (`plan-fig`): each operation (plan, limit, aggregate, scan, read, range) is a tab button with its tree glyph and one note; the read-out beneath carries the operation's **why**, the **fragment of the statement it reads** (`reads <mark>…</mark>`), and a *<Concept> on the map* link that pins that stage on the system diagram via `plomid:arch-select`. No benchmark claims; the figure caption says the shape is illustrative. |
| Architecture `05` | Access path | The statement shown once as the **query source**, then predicate → index → page → row as **selectable steps** (buttons narrowing left to right): hover previews, click pins (`aria-expanded`), and the read-out beneath carries the step's own wording plus a *Find it on the map* link that hands focus to the matching stage on the system diagram. |
| Architecture `07` | Transactions & MVCC | A wide statement, a two-lane snapshot diagram and three flat callouts. The two lanes carry **cursors** that travel at the same speed, half a phase apart — neither waits for the other, which is the existing claim and nothing more. Motion runs only while the diagram is in view (`IntersectionObserver` toggles `data-live`) and never under reduced motion. |
| Architecture `06` | Layer detail | The layers as hairline rows, each stating its **neighbour relationship** in mono (`sits below X · above Y`) — the boundary visible without interaction. A **Find it on the map** link on each built layer pins its stage on the system diagram via `plomid:arch-select` (the deployment fabric, still roadmap, has no stage to point at). Status appears once, only on the roadmap layer. |
| Sovereignty `02` | Interactive destinations | `[data-dest]` buttons answering in `[data-dest-panel]` directly beneath the map |
| Roadmap `03` | Capability map | The five-tier table plus its one `.tier-legend`, now an **explorable composition** (`.cap-layout`): table left, a sticky read-out (`.cap-detail`) right. Hovering a row previews its direction, example, tier, the plan move that brings it in and its related surfaces (model explorer, architecture, infrastructure table); clicking **pins** the answer (roving press state, `data-cap-pin`), rows outside the selected tier quiet to `opacity: .45`, and focus/Enter reaches the same answer. Every relationship points at content that already exists — no invented capabilities. Mobile: the read-out sits directly beneath the table (the in-place rule) |
| Roadmap `05` | Infrastructure | `.infra-grid` of roadmap items, stated as direction, each term carrying a `NavIcon` mark (network / globe / transaction / storage) so the enterprise-facing sections read as infrastructure at a glance |
| Roadmap `06` | Enterprise integration | The direction diagram, sources → PLOMID → targets, as selectable systems: 5 source categories + 4 consuming surfaces, each a button with the site's own `NavIcon` mark + kind + one-line direction, answering in a readout directly beneath the map (hover previews, click pins, focus answers). Three pattern cards below (read without moving / write back where it belongs / one answer). Sources keep the dashed direction frame; the PLOMID core carries the **real brand lockup** (`BrandMark`, paper variant). **Never third-party logos**: names are text categories, a logo would imply a partnership that does not exist. Count line states `0 integrated today` |
| Sovereignty `02` | Data-control map | `your data` → rail → PLOMID → rail → four destination targets, all dashed. |
| Sovereignty `03` | Control dimensions | Seven `.row`s: location, residency, ownership, access, replication, jurisdiction, portability. |
| Sovereignty `05` | Deployment targets | The four topologies, stated as direction under the fabric. |
| Sovereignty `06` | FAQ | Four deployment/residency questions; the answers restate the roadmap boundary rather than promising it. |
| Playground `03` | FAQ | Four questions about what the demo is and is not (no server, no custom SQL yet). |
| Company `05` | Working with PLOMID | The three external audiences, Investors, Press & Media, Partners, each a real page. No invented funding, coverage or partner logos. |
| Company `06` | FAQ | Company-level questions (who builds it, hiring route, how to work together, where PLOMID is based). Emits FAQPage structured data via `Base`. |
| Developers `05` | Access / GET PLOMID | The honest access section: playground + docs + GitHub actions, and a mono note that packaging is being prepared. Replaces a download button that would be a lie. |
| Download hero | Detector | `PageHero slot="visual"`: a `.surface` detector (OS / Arch / File / Method rows, per-OS `NavIcon`, live confidence, Download + manual-pick links). Browser-only detection — UA regex for fast paint, then `userAgentData` high-entropy `platform` + `architecture` + `platformVersion` where offered. Nothing uploaded. The hero primary CTA follows the same suggestion as the Recommended card |
| Download `01` | Recommended | Dark. Suggested package card (icon, `auto-detected` pill, OS/Arch/Format/Manager meta grid, Download + Checksums) beside three install methods (shell / Docker / source, each `NavIcon` + `CodeBlock`). Suggested card carries the corner tick; Linux ARM falls back to Docker/source with the mismatch stated |
| Download `02` | Package matrix | Graphite. OS filter tabs (All / macOS / Windows / Linux / Docker & source, `aria-pressed`, hide non-matching groups) over 4 groups, 9 cards: icon box + format badge + `arch · req` + install hint (`sudo apt install…` `tar -xzf…` etc.) + Download + checksum link. The detected card takes `data-suggested="true"` (copper border + `matches your platform` + corner tick). No sizes, versions or dates |
| Download `03` | Verify + requirements | Light. Three steps with marks (`download` / `shield` / `check`), `shasum -a 256 <file>` + `sha256sum <file>` blocks (compare with the published value, never a pasted hash), and a `.spec` requirements table derived from the package facts |
| Download `04` | First run | Dark. Three steps with marks (`database` / `operations` / `query`: `plomid init` / `plomid start` / `psql -h localhost`) plus Docker and shell one-liners. Same interaction language as the lifecycle strips: hover lifts, icon takes the copper edge |
| Download `05` | FAQ | Light. `Faq` with five install questions (which file / verify / package managers / Docker vs binary / ARM + older systems). Answers restate only what the matrix, verify section and releases page state |
| Pricing tiers | Three cards with price anchors (Free / Custom / Usage-based): audience + status dot, name, price + mono note, flag, checklist, block CTA + mono note. Enterprise sits middle as lead (copper top rule, badge). Available tiers carry the corner tick; cloud keeps the dashed treatment. Hovering a tier previews its column in the comparison below |
| Pricing comparison | The 12-row matrix in 4 groups (start and cost / run and operate / control and trust / build and support). Each row is a question button (`NavIcon` + hint + plus/minus mark) opening its `why` directly beneath it, one open at a time. Tier headers are buttons: hover/focus previews the column, click pins it, clicking the pinned tier shows all again; a live readout names the isolated tier and the rest quiet down. Every cell restates what the tiers and FAQ already claim |
| Pricing enterprise + cloud | Enterprise band: three numbered steps (workload → reference architecture and review → deploy with a direct line) beside an offer card (checklist + Reach our team + contact link). Cloud panel: dashed frame stating the fabric dependency and the first-tenants-from-partnerships rule, with register-interest and roadmap links |

### The in-place rule

The model explorer is the reference implementation of a rule that applies to every interactive
region: **click or hover → the answer appears around the thing you touched.**

- The selected model opens directly beneath its own row (`row → panel` gap is `0`), never in a
  neighbouring column and never further down the page.
- Collapsed rows are compact, so there is no empty region to scroll through while nothing happens.
- The expanded composition is full container width, which is also what makes the visual large
  enough to read (the model SVG renders around `700px` wide on desktop, and pans inside its own
  frame rather than shrinking below legibility on a phone).
- The panel only nudges itself into view if a very short viewport pushed the row to the bottom
  edge. It never jumps the page.

`LayerPipeline` does it with the detail pinned directly beneath the stage rail; the architecture
query lifecycle does the same, and adds the statement fragment the selected stage reads. The
pricing comparison does it with the reasoning row directly beneath its question row.

### The glimpse rule

The homepage explains **nothing** about the architecture in depth, that is `/architecture/`'s job,
and a deep explanation is not repeated on `/`. What the homepage carries instead is the
**glimpse** (`ArchGlimpse`):

- **A complete picture, not the architecture.** One plate: nine hairline rows on one spine `Clients` (context, quieter) → the seven stack rows → `Data` (the one filled copper row, because
  it is what the whole stack exists to keep). No tabs, no panels, no hover detail, no explanation
  of any stage.
- **The picture is the way in.** The entire plate is one `<a>` to `/architecture/` with an
  `aria-label` that says what the full page holds. No CTA button, no "Learn more" row, the caption
  under the plate names the destination in mono and one sentence.
- **No status language.** Capability status lives on `/roadmap/` only; the glimpse carries none.
- **The stage names are the shared truth.** They are the same nine names `LayerPipeline` uses on
  `/platform/` and `/architecture/`; keep the two lists in step if the stack ever changes.
- **One primitive of motion.** A single copper dot travels the spine and stops under reduced
  motion. Nothing else moves.

The same rule removed the homepage's standalone premise diagram (`premise-field`"Several
systems, several copies"): the fragmentation→convergence story is told once by `Convergence`,
and the statement section keeps only its text and three definitions.

### The plan and the map

`/roadmap` opens with **the plan**: four moves on one horizon line, each carrying its honest tier
flag, the capabilities it brings in, and the sentence of body that says what it stands on"Start from what already runs""Land the models already being built""Open the remaining paths
to the same data""Then take the layer outward". The last move also carries the four
infrastructure terms, which live outside the capability map. A copper signal travels the horizon
line (the page's one loop; removed under reduced motion), with a waypoint diamond per move.

The plan and the capability table below it are **two readings of one list**: every capability row
carries a `move` key, and the plan's per-move lists are derived from it, a capability can never
drift out of the move that claims it. Nothing in the plan adds a claim the tiers do not already
make: no dates, no promises, and a move whose tier is not `current` says so in its own flag
(`In development · not available to use yet` `Being explored · not scheduled` `Design open · not scheduled`).

**One model visual system.** `ModelShowcase` is the only place the six data models are drawn. The
separate Future Direction panel that previously repeated vector, graph and blob visuals was
removed, and the direction is now a short editorial note on the homepage plus the platform-evolution
sections on `/platform` and `/roadmap`. Two diagrams of the same three models in one product was
the single worst duplication on the site; a model is drawn once, in the explorer, and referenced
everywhere else.

### Status indicators

Status is normally communicated **without** a chip: the section states the situation once in prose,
then the treatment carries it (solid vs dashed, filled vs ghosted, active vs muted). Every
"available" surface additionally carries the **corner tick** — a small drafting-mark bracket
(10px, copper, top-right, `::after` on the cell) that reads as a quality stamp on what ships
today; roadmap cells stay quiet. `.status` is
reserved for the surfaces whose whole purpose is status, the roadmap tables and the architecture
layer detail. **The footer carries no status legend**; it represents company, product, resources,
community, legal and contact.

### Social presence

`SocialLinks` is the single source of social markup. Rules:

- Real channels are `<a target="_blank" rel="noreferrer noopener">` with `aria-label` and `title`
  ("GitHub, Source, issues and releases").
- Channels without a confirmed URL render as `<span>` with dashed border and `opacity: .34`,
  **never** as a link, **never** with a loud "coming soon" badge. The accessible label says what it
  is ("Discord, Join community (not connected yet)").
- When a real URL exists, set `url` in `site.ts`; the same component starts rendering a link. No
  markup changes anywhere else.
- Interaction: `--t-fast` lift (`translateY(-2px) scale(1.04)`), copper border + soft local glow,
  pressed state, visible focus ring; disabled under reduced motion.

### Footer composition

The footer is recomposed as seam + brand + navigation + contact + wordmark + atmosphere, not a
list of link columns:

1. **The seam**, a glowing copper rule across the footer's top edge, **before any footer
   content, never after it**. It draws left→right as it enters view, and a blurred copy of the
   same rule falls into the footer beneath it so the seam reads as light sitting on the edge of
   the page. Under reduced motion it arrives complete.
2. **Closing CTA band**"One data layer. Built to carry more." with Start building / Talk to the
   team, over the shared atmosphere.
3. **Brand anchor**, the PLOMID mark at 30px with a 22px wordmark, the full company line
   (`Platform for Modern Intelligence and Data`) in mono caps, a two-line blurb (38ch), the origin
   line, and the `SocialLinks` row. The brand column is the visual anchor; link groups are quieter.
4. **Four link groups on ONE line beside the brand anchor** (the groups' nav steps aside with
   `display: contents`, so the body grid lays brand + 4 columns on a single row): *Product*
   (Platform, Architecture, Data Sovereignty, Roadmap, Pricing, Download), *Developers*
   (Developers, Docs, Playground, GitHub), *Industries* (the six primary
   industries under footer-short labels — Industrial, Financial, Commerce, Science & Health,
   Infrastructure, Public & Sovereign — closed by one quiet copper *Explore industries →*), and
   *Company* (About, Blog, Careers, Investors, Press & Brand, Partners, Legal). Group heads sit on a
   hairline **that is alive**: on group hover or focus-within a copper rule draws across it,
   left to right, the group announcing itself the way its links do. **Every link is a canonical
   route with a trailing slash.** No Solutions group: it stays in the navbar, the drawer and
   /solutions/.
5. **Contact row** `hi@plomid.in` in mono beside a copper `LET'S TALK →` hairline link.
7. **The closing mark, the real logo, one horizontal lockup.** Every `<path>` in `.f-mark` is
   **extracted directly from `public/assets/PLOMID_OFFICIAL_CLEAN_VECTOR.svg`**: the emblem
   symbol (`.sig-symbol`, scaled to the letter band via `scale(0.2472)`) sits **beside** the six
   letter contours (`.sig-letter`, P·L·O·M·I·D, shifted by `translate(49 -1386)`) under
   `viewBox="-8 -8 1910 240"`, symbol left, wordmark right, one line. Only transforms
   reposition the geometry; nothing is redrawn and no font is substituted. The mark renders as
   **OUTLINE, not fill**: `fill: none` permanently, only the true contours are stroked, at **one
   optical line width**: the symbol group's `0.2472` scale means its stroke is `36.4 → 18.2`
   (≈ `9 → 4.5` effective, exactly the letters' weight), so the emblem never reads hairline-light
   beside a heavy wordmark. Contours draw in once on reveal (`sig-draw`, symbol first, then
   letters) and settle via `sig-weight`/`sig-weight-sym`. It then settles into a **quiet loop**:
   `sig-sheen` is a breathing copper sheen crossing each stroke on a `7.2s` cycle with per-part
   offsets, colour only, transform-free, routed through the `--sig-stroke` variable so hover
   keeps authority over the base colour, and paused entirely under reduced motion. **Hover or
   focus flips to the opposite state**: the outline fills **solid copper** (the O's counter stays
   hollow via `fill-rule: evenodd`), the stroke holds copper through the sheen, a soft
   `drop-shadow` lifts the mark, and the whole lockup **floats** (`sig-float` `2.8s`
   ease-in-out loop, also disabled under reduced motion). The block is `aria-hidden` (the brand
   is already named in the anchor above).
8. **Legal bar**, Privacy / Cookies / Terms / Legal and the copyright line.
9. **The end glow, the edge of the website.** A copper line `position: fixed` to the bottom of
   the viewport, after everything (footer included), with a soft blurred light **shining
   upward** over the footer's last rows. It draws outward from the centre as it enters view,
   never loops, and arrives complete under reduced motion. `z-index: 40` `pointer-events: none`.

Atmosphere: the footer body carries its own faint engineering grid (vertical hairlines at 11.11%
columns, masked radially, opacity 0.28 → 0.18 on tablets → hidden under 400px), one shared
`Atmosphere` particle layer (10 signals, seed 57) and **one** pointer-follow light, the footer is
a single `[data-ambient]` field, so there is never more than one light in it. All layers are
`pointer-events: none` behind content at `z-index: 1`.

Responsive: brand + groups in a 1.35fr/1fr split → stacked at 1080px; groups 3-up → 2-up at 700px →
1-up at 560px; contact and legal stack and left-align under 560px. Body text never drops below
~14px; links carry comfortable tap targets (6px block padding).

### Search (⌘K)

One site-wide search: `Search.astro` renders a header trigger, and the palette itself is a fixed
`role="dialog"[aria-modal]` filtering the merged corpus (`searchEntries` in `registry.ts`: the base
`searchIndex` from `site.ts` plus the entries `industries.ts` and `solutions.ts` generate, which is
what puts 34 industry pages and 18 solution pages in the palette without a second list). No service, no dependency. Keyboard: `⌘K`/`Ctrl K`
toggles `↑/↓` moves `Enter` opens `Escape` closes, focus is restored on exit. The drawer's
"Search PLOMID" button opens the same palette on mobile. To add content to search, add an entry to
`searchIndex`, nothing else needs to change.

The palette composition, top to bottom: a head row (glyph + input + `⌘ K` chip + `esc` button),
the results list, and a key-legend rail (`↑ ↓ navigate · ↵ open · esc close`). It speaks the
site's own instrument language, not a terminal's: **ink** panel with hairline `--line-d` borders
at `--radius` (2px), **one copper rule along the panel's top edge**, the footer seam's echo, a
**sans-serif input** in the site's voice, a solid dim backdrop (no blur), and no mono brand row.
Results are three-part, **title** (500 weight), **canonical route** (mono, right-aligned, e.g.
`/developers/`) and **description**, grouped under uppercase mono category labels. Selection is
a quiet `--accent-08` background plus a 2px copper left edge, never a glow, never a white flash;
hover and `↑/↓` set the same state. The overlay transition is 200–220ms. Under 560px the palette
becomes a full-height sheet (`100dvh`, no border radius) with the ⌘K chip dropped and larger
touch targets, the desktop layout is never squeezed into a phone width.

### Breadcrumbs

Inner pages render **one** breadcrumb trail, derived centrally, pages no longer pass their own
`crumbs` props. The merged trail map in `registry.ts` (`routeTrail` from `site.ts` plus the
generated industry and solution trails) covers every canonical path, so the same array feeds the
visible trail (`PageHero`), the `BreadcrumbList` JSON-LD (`Base.astro`) and nothing else can drift.
A segment may also be a **labelling segment** (a level with no page of its own, such as the industry
category in `Industries / Industrial / Oil & Gas`): it renders as text and is published without an
`item` in the schema.

```
/roadmap/    → PLOMID / Product / Roadmap       (URL stays /roadmap/)
/docs/       → PLOMID / Developers / Documentation
/playground/ → PLOMID / Developers / Playground
/cookies/    → PLOMID / Legal / Privacy / Cookies
```

The trail provides hierarchy; the h1 provides the page's editorial voice, and the page name is
stated **once**. The hero renders no plain page-name line under the trail: `PageHero` suppresses its
eyebrow whenever the trail already terminates at the page being viewed, which is true when the
deepest segment has no destination (`PLOMID / Product / Architecture`) *or* when its destination is
this URL (`PLOMID / Product`, where `Product` links to `/platform/`). A name comparison is kept as
a second guard for a label that names the page in different words (`Press & Media` vs the
`Press & media` eyebrow). Verified on every built page: the trail goes straight into the `h1`.
The eyebrow would only reappear if a future trail deliberately stopped at an ancestor rather than at
the page. The final segment carries `aria-current="page"`. The trail is `aria-label="Breadcrumb"`
semantic `<nav> + <ol>`.

**The terminal segment is never a link.** The last crumb names the page you are on, so
`PageHero` strips the href from any final segment that still points at the current URL, the
degenerate case was the group-root trails (`/company/` → Company → `/company/`, same for
`/developers/` and `/legal/`), which rendered the page's own name as a link to itself and only
then the h1. `routeTrail` now ends those trails on a destination-less segment, and the `PageHero`
rule is the permanent guard. The `BreadcrumbList` JSON-LD is unaffected: a terminal crumb's
schema `item` is the page's own URL (correct per schema.org). The probe asserts per route:
`terminalIsCurrent=true terminalNotLink=false eyebrow=false`, i.e. exactly one trail, every
non-terminal segment linking to its group root, the terminal segment current and not a link, and
no repeated page-name row after it.

### Canonical route display

Displayed route strings are **always canonical with a trailing slash** (`/developers/` `/docs/` `/roadmap/`), never bare abbreviations. They appear in search results (right-aligned mono column),
structured data and internal references. The display hierarchy (`PLOMID / Product / Roadmap`) is a
labelling layer only: URLs never change, and no fake nested routes exist.

### Navigation active state, one owner per route

`groupForPath()` in `registry.ts` (it moved there when the trails gained two more sources) is the
single authority for which navigation group owns a URL, and it is derived from the **merged trail
map** rather than from a second hand-written list: the trail's first segment names the group
(`Product` `Industries` `Developers` `Company` `Legal`), and a small label→group map in `site.ts` is
the only place the two vocabularies meet. Adding a route therefore fixes its breadcrumb, its JSON-LD
and its navigation ownership at once; `/industries/oil-gas/` activates **Industries**, and
`/solutions/digital-twins/` activates **Product**, because that is what their trails say.

A destination reachable from two groups (Playground sits under both Product and Developers)
activates exactly **one** group, the canonical owner (`/playground/` → Developers). Verified on
the built site: `/platform/` `/architecture/` `/roadmap/` `/solutions/` and `/sovereignty/`
activate Product; `/developers/` `/docs/` and `/playground/` activate Developers;
`/company/` `/careers/` `/investors/` `/press/` `/partners/` `/blog/` and `/contact/` activate
Company.
The bottom line language is unchanged: hover grows left→right, active is fully present,
active+hover strengthens it.

### Search (⌘K)

### Durations and easing

| Token | Value | Used for |
| --- | --- | --- |
| `--t-fast` | `180ms` | Hover, press, colour |
| `--t-med` | `380ms` | Component transitions, panel swap |
| `--t-slow` | `700ms` | Reveal, layout change, system movement |
| `--t-ambient` | `14s` | Atmosphere drift (the layer pans over `2 × --t-ambient`) |
| `--ease` | `cubic-bezier(.22,.61,.36,1)` | **The** family. Everything eases out. |
| `--ease-io` | `cubic-bezier(.65,.05,.36,1)` | Reserved for symmetric in/out |

Per-component durations (reveal `0.8s`, panel swap `0.5s`, hover `0.4s`, signal run `2.8s`, hint
pulse `3.4s`, particle drift `18–30s`) are deliberate instances of these four tiers, not new
values.

One easing family across the site is deliberate: it is what makes every page feel designed by the
same company. No bounce, no elastic, no spring.

### Motion tiers

| Tier | Duration | What belongs here |
| --- | --- | --- |
| Fast | 120–220ms | Button press, hover colour, link arrow, status dot |
| Medium | 300–450ms | Node activation, panel swap, dropdown, tab selection |
| Slow | 600–1000ms | Scroll reveal, convergence stage change, model stage transition |
| Ambient | 2.8s–30s | Signal runs, particle drift, node pulse |

### Reveal behaviour

`[data-reveal]` starts at `opacity: 0; transform: translateY(18px)` and gains `.in` when an
`IntersectionObserver` (root margin `0 0 -8% 0`, threshold `0.12`) sees it. Delay comes from
`--rd`. Variants: `data-reveal="fade"` (opacity only) and `data-reveal="wipe"` (clip-path sweep).
Children of `[data-stagger]` get `--rd: index * 70ms`.

All of it is gated behind `html[data-js]`, set by an inline script in `<head>`, so with scripting
disabled every element is visible rather than stuck at opacity 0.

### Hover behaviour

- Buttons: `translateY(-2px)`, border → `--fg`, arrow slides 4px.
- `.tlink`: copper text, copper underline, gap widens 9px → 13px, arrow slides 3px.
- `.lift`: `translateY(-3px)` + `box-shadow: 0 26px 54px -38px rgba(0,0,0,.6)`.
- Nodes/cards: border → `--fg-3` or `--accent`, background lifts one panel step, a 2px copper
  bar scales in from the top.
- Physical press everywhere: `:active` returns to `translateY(0)` and scales to `0.985–0.996`. The
  interface answers the pointer.

### Scroll behaviour

- Header gains `data-scrolled="true"` past 8px: blurred ink backdrop, bottom hairline, and a
  copper scroll-progress hairline scaled by `--progress`.
- `[data-scrolldrive]` elements expose `--sp` (0→1) as they travel through the viewport. CSS reads
  it to cross-fade stages. Currently used by `Convergence`.
- **Making an animation observable.** `Convergence` is `300vh` tall, so the drive covers ~`200vh` of
  travel and its thresholds are spread across that range (tangle out by `0.20`, structure in
  `0.20–0.42`, layer `0.46–0.66`, outputs `0.66–0.84`). A scroll-driven moment has to last long
  enough to be watched; if a state resolves in half a screen of scrolling, the visitor never sees
  it.
- Reveals fire once and unobserve.
- **No scroll hijacking.** The user always controls the wheel; sticky regions only hold a visual in
  place while its own section passes.

### Pointer behaviour

- `[data-parallax="0.06"]`, the element shifts by `pointer / viewport * amp`, written as `--px` /
  `--py`, throttled to one `requestAnimationFrame`.
- `[data-ambient]`, a section-wide radial light following the pointer via `--hx` / `--hy`.
  Soft, wide, low opacity. Never a visible cursor follower, never a trail.
- Component-local responses: `ModelShowcase` moves its own query point (the time-series scan and the
  vector probe). `HeroSystem` has **no** local glow and writes no pointer variables, the hero's
  ambient field is the only light in that section. `LayerPipeline` writes nothing: its signal is
  pure CSS and pauses on `data-lp-engaged`.
- **One ambient field per hero.** The hero has exactly one pointer light (`[data-ambient]` on
  `.hero`) plus the data atmosphere. The product visual deliberately carries no glow and no
  decorative grid, so the copy and the diagram sit in one visual field rather than two lit boxes.

### Selection and sequence

Two motion patterns carry the interactive surfaces added by later passes:

- **Soften the field around a selection.** When one thing is chosen, the things *around* it step
  back rather than the chosen thing shouting. `ModelShowcase` writes `data-open` on the list and
  dims every other row to `opacity: .5`; the open row keeps full contrast, and hovering or focusing
  a dimmed row restores it. A selected deployment destination on `/sovereignty` takes the copper
  accent while its siblings stay muted. Selection never removes information and never makes
  anything unreachable.
- **A staged sequence reads as movement.** `StorageFlow`'s `.sf-flow` strip lights one stage at a
  time (`4.8s` cycle `calc(var(--i) * 1.2s)` offset) beneath a signal that scans left to right, so
  write → page → block → storage is *watched* rather than read. It is the same requirement that
  makes `Convergence` observable: a sequence must last long enough to follow and be offset enough to
  be legible as a sequence rather than a single flash.
- **Enter, settle, respond.** Nothing appears and vanishes instantly. Hover lifts and settles
  (`translateY(-3px)` → `:active` back to `0`), panels fade and rise `6–10px` on open, and state
  changes cross-fade over `--t-med` to `--t-slow`.
- Pointer listeners are `{ passive: true }` and only attached when
  `(hover: hover) and (pointer: fine)` matches. Both effects share **one** `pointermove` listener
  and **one** `requestAnimationFrame`, so adding another ambient region costs nothing.
- Sections that opt into `[data-ambient]` toggle `data-ambient-on` on `pointerenter`/`pointerleave`,
  so the light only exists while the pointer is actually inside that section.

### Reduced motion

`prefers-reduced-motion: reduce` is honoured globally:

- `scroll-behavior: auto`.
- All animations and transitions clamp to `0.001ms` so nothing moves, ever.
- `[data-reveal]` is forced visible with no transform and no clip-path.
- `--sp` is pinned to `1`, so scroll-driven visuals render in their resolved state.
- Pointer effects are not attached (`prefersReduced()` is checked before wiring listeners).
- Console step delays collapse to zero; the result appears immediately.

---

## 8. Interaction

| State | Treatment |
| --- | --- |
| Hover | Colour shift + 2–3px lift + border-strengthen. Always has a non-hover equivalent. |
| Focus | `:focus-visible` → 2px copper outline, 3px offset, 1px radius. Never removed. |
| Active | Settles back down (`translateY(0)`) and scales to ~`0.985`. |
| Pointer proximity | Ambient light + small parallax. Purely additive, no information depends on it. |
| Keyboard | Tabs respond to `←/→` and `Home`/`End` (`LayerPipeline`, the architecture query lifecycle) and `↑/↓` (`ModelShowcase`). Focus drives activation, so the detail panel follows the keyboard exactly as it follows the pointer. Roving `tabindex` is maintained, so a tab set is one tab stop, not nine. |
| Touch | Every hover interaction also responds to `click`/`pointerenter` fallbacks; `@media (hover: none)` never leaves a control inert. |

### Button perimeter signal

Buttons carry the strongest interaction in the system, and it is the same language everywhere
(`.btn`, defined once in `global.css`).

| State | Treatment |
| --- | --- |
| Default | Quiet border, no motion. The button sits still. |
| Hover | A short copper segment **travels once** around the 1px border (`.72s` `--ease`) and stops. |
| Focus | The same travel, plus the standard `:focus-visible` copper ring. |
| Active | Compresses: `translateY(0) scale(0.985)`. |
| Reduced motion | The segment appears at its end position, no travel. |

The ring is one painted layer: a conic gradient masked to the frame (`padding-box` XOR
`border-box`), with the angle animated through a registered `@property --btn-angle`. No extra
markup, no SVG, no spinning border, the line reads as a signal arriving at the edge of the
control. On the copper primary button the segment flips to white for contrast. Text links keep the
quieter `.tlink` underline language; the perimeter is reserved for buttons, so no page becomes a
field of travelling lines.

### Instrumentation

Interactive elements carry a semantic `data-interaction="…"` attribute. There is no analytics SDK,
no cookies and no tracking, these are hooks for whatever measurement is added later, and they
document intent in the markup.

| Attribute value | Where |
| --- | --- |
| `hero` | the home hero section |
| `hero-model` `hero-model-roadmap` | HeroSystem model and roadmap chips |
| `social` | the real social icon links rendered by `SocialLinks` |
| `model-select` | model nodes (ModelShowcase) and the platform model map |
| `lifecycle-stage` `lifecycle` | the eight query-lifecycle stages and their strip on /architecture |
| `collab-cta` | the architecture collaboration CTA |
| `corporate` | the company page's investors / press / partners cards |
| `convergence` `convergence-model` | the convergence section and its model picks |
| `architecture` `architecture-layer` | architecture section and layer items |
| `storage` | storage flow section |
| `playground-run` `playground-sample` `playground-copy` | console controls |
| `code-copy` | CodeBlock copy button |
| `primary-cta` `secondary-cta` | page hero and band actions (overridable per action) |
| `lifecycle-stage` | the eight query-lifecycle stages on /architecture |
| `explore-link` | the four homepage Explore PLOMID cards |
| `deployment-target` | the four selectable deployment destinations on /sovereignty |
| `next-up` | NextUp cards |

---

## 9. Visual language

| Form | Meaning | Where |
| --- | --- | --- |
| Hairline (`--line`) | Structure, boundaries, quiet separation | Everywhere |
| Strong hairline (`--line-strong`) | Framing a live surface | `.surface` `.panel`, hero plate |
| `.plumb` | Vertical calibration line with end ticks | Section transitions |
| `.grid-lines` | Vertical rules, masked top and bottom | Heroes, roadmap |
| `.grid-dots` | Dot field | Storage, roadmap, model explorer |
| `GeoForms` | Cropped arc / coordinate crosshair / partial grid at one section corner | Home premise, future direction and get bands, architecture collaborate band, company corporate band, roadmap enterprise band, investors, partners |
| `.is-ghost` hatch | Dashed 45° hatch = "not yet" | Roadmap frames, ghosted rows |
| `.sig` + `::after` | A single copper light travelling along a wire, 2.8s linear | Wires, rails |
| `.bars` | Stacked bars = stored data; first bar is `--fg-3`, rest `--line-strong` | HeroSystem, apps |
| Node marker | Small square/circle at a wire junction | HeroSystem, Convergence, layers |
| `.atmos` | 9–16 faint dots (1.5–3px, opacity ≤0.3) drifting over 18–30s plus a hairline grid panning over 28s | Hero, convergence, storage, future direction, roadmap |
| Data form | Stacked rectangles standing for rows/documents/events | Model visuals, storage |
| `ModelGlyph` | The per-model mark; outline shape + copper live part | Every model mention |

Atmosphere rules: dots are `1.5px`–`3px`, peak opacity `0.3` and rarely visible at more than `0.16`,
drift over `18–30s`, positions and durations are deterministic (no randomness at runtime), and the
layer is `pointer-events: none`. It must be felt before it is noticed. Never a starfield, never a
nebula, never a particle cloud. The pointer light is a `44vmax` radial at `--accent-08`, ambient
light, never a visible cursor follower and never a trail.

**One atmosphere per field.** A hero gets one pointer light and one `Atmosphere`; a product visual
inside it gets neither. Two glows in one viewport is the most common way a designed hero starts to
look decorated rather than designed.

**Bounded drift.** Signals drift toward the nearer edge: dots on the left half of the layer travel
right, dots on the right half travel left (`Atmosphere.astro` signs `--dx` by `--x`). A dot can
therefore never drift out of its own layer at any width. The layer also clips itself
(`overflow: hidden`), so the atmosphere is structurally incapable of widening the page, the drift
is bounded by design, not by luck.

**Status as treatment.** Five tiers, read from the frame rather than a badge:

| Tier | Treatment |
| --- | --- |
| Current | Solid copper border `--accent-14` fill |
| Current foundation | Solid neutral border, one panel step of fill |
| Roadmap · in development | Dashed border, hatched fill |
| Potential | Dotted border, no fill |
| Long-term | Dashed at `--line`, reduced opacity |

`.tier-swatch` / `.tier-bar` carry the five tiers in the roadmap legend and table, and the homepage
uses the same treatment on `.rm-swatch`. A selected roadmap element may take the copper accent but
keeps its dashed frame: selection must never make something unshipped look shipped.

**Moving data forms.** Where a section holds still long enough to be looked at, something in it
moves: a copper signal travelling a wire (`.sig::after` `2.8s` linear), a dashed join or graph
path marching (`3s`/`3.4s`), a live dot pulsing, the HeroSystem bus node, the convergence layer
ring. Each is one small element, not a whole animated plate.

### Data-model surface and visuals

The model explorer sits on its own engineered field, graphite tone, faint masked grid, one
pointer light, so it reads as a dedicated product surface rather than a section of cards. The
diagrams are drawn **light-on-dark**: table surfaces one panel step up, keys as bright bars,
values quieter but never invisible. This replaced the earlier ink-on-paper fills that rendered the
SQL diagram effectively black.

- **SQL**, tables with tonal separation (surface / header / hairlines), primary keys bright,
  foreign fields mid, highlighted row with copper bar, an animated join path with anchor nodes.
- **JSON**, nested document with brace + guide structure; keys bright, nested values in the
  `--json` hue.
- **Vector**, a conceptual embedding space: three organic clusters of differing local density plus
  field noise (deterministic at build time), proximity rings, a query point that follows the
  pointer; hovering a cluster activates its neighbourhood and recedes the rest. Labelled
  *conceptual*.
- **Graph**, meaningful topology with `data-e="a-b"` edges; focusing a node lights its edges,
  rings its neighbours, softens the rest.- **Blobs**, the storage story in one frame: application → objects → queryable metadata → storage band, with coupling lines that light when either side is active.

**The diagrams run.** Each model names the operation it is depicting in its canvas head `join · plan` `resolve path` `window scan` `probe` `traverse` `put · get`, beside a pulsing copper dot, and each one then actually moves something: a signal rides the SQL join; a cursor descends the JSON document; the time series is swept by an autonomous cursor whose dot rides the `s1` polyline waypoint for waypoint; a lookup cursor walks the key-value rail and dwells at each row; a traversal walks the graph path and pauses at each node; a write and a persist carry objects into storage. It is all one primitive, a small copper dot moving on a wire, which is what makes the models read as one working system rather than eight illustrations.

**Cursors ride their wires, they do not approximate them.** The JSON, Key-Value, Graph and
Objects cursors are native SVG `animateMotion` elements whose `path` is the same `M…V…H…/L…`
geometry as the drawn connector, so the dot is pinned to the line by construction, drift-proof
against easing, caching and viewport. (CSS `translate` keyframes were tried first and cut
corners; they are gone.) Each plate owns one clock: JSON `2.4s`, Key-Value `4.8s`, Graph `4.2s`,
Objects `2.4s` with the persist leg at `+0.8s` (`begin`). Where a cursor dwells (rail rows,
graph nodes) the dwell points are `keyPoints`/`keyTimes` on that same clock. The SQL signal is
still CSS keyframes, the join curve it approximates is short; convert it if it ever reads wrong.

**Touch glows: blocks light when the cursor touches.** Every block a cursor touches carries a
transparent overlay `rect`/`circle` (`.touch`: accent fill at 9% + accent stroke,
`pointer-events: none`), lit by SMIL `<animate>` on the SAME clock as that plate's cursor, with
`keyTimes` computed from path-length fractions of the motion path (e.g. the profile corner sits
at 155/311 ≈ 0.50 of the JSON path), so the light lands exactly when the cursor arrives, every
loop, and never drifts. The glow colour follows the plate's own lit-state token: `--json` on the
JSON plate, `--accent` on Key-Value, Graph and Objects. The choreography names the system's
story: JSON resolves document → profile → location; Key-Value lights key-then-value per row;
Graph lights customer → invoice → payment along the highlighted path; Objects lights the full
application → asset → metadata → storage relationship. If a wire moves, retune the plate's
`keyTimes` with it, the comment at each `.touch` rule says so. Hover/pin highlighting is a
separate system and is untouched by all of this; glows are overlays, never state.

Inactive panels are `hidden`, so a closed model costs nothing. Everything moves with
`transform`/`opacity` only, and every one of these animations is off under reduced motion (the
active dots and touch glows are `display: none`, the rings settle, the points hold full opacity,
so the diagrams read as static schematics). Pointer-driven motion still wins: the moment a
pointer drives the time-series scan or the vector probe, the autonomous version stands down
(`data-live` on the `<svg>`).

### Storage explorer

The stage rail (Write / Page / Block / Storage) is an interactive control, not a caption strip:
buttons with `aria-expanded` + `aria-controls`, hover/focus previews, click pins, the explanation
opens directly beneath the rail (`aria-live="polite"`), and the SVG stage groups dim/brighten in
sync (`data-sf-hl`). Details stay technically grounded in the stated storage contract, pages,
blocks, one durability path, with no invented behaviour.

### Architecture entry

`LayerPipeline` reads as the entry into the system. A `.lp-head` line spells the whole path in
mono (`clients · query · parser · … · data`) above a three-row instrument: the statement at
the entry on the left, the nine stages on one rail to its right, and the selected stage's detail
beneath. The rail carries a copper signal that steps stage by stage on a `9s` cycle, each stage's
node lighting on the signal's own clock; the signal pauses the moment a visitor hovers, focuses or
clicks, and `data-lp-engaged="true"` holds it paused. The detail shows the stage's role, summary,
points, and an `in → out` readout, and the single dashed roadmap note appears only on the one stage
that is still design. No numeric prefixes anywhere.

### 11. The data-model surface system

`ModelShowcase` is the centre of the product and its own section: **eight models, eight
surfaces, one system**. It replaced the six-row expander (`.ms-*`) wholesale, the row
list, the concentric ring diagram and the `data-ms-item` contract are gone.

#### Surfaces and rhythm

- **One plate is one `<article data-dm-surface="key">`**, full-width, in the order
  `SQL → JSON → Key-Value → Time → Geo → Vector → Graph → Objects → Hybrid`. Each carries: a
  plate head (glyph · name · workload · operation), the plate's tagline as its title, and a
  body of `diagram | reading`.
- **Two tones only, alternating by index.** `data-tone` is `light` on even plates and
  `graphite` on odd ones (`tones[i % tones.length]`), so the section reads as an alternating
  editorial rhythm in two colours: warm paper and graphite. No rich black plates, and status
  is **not** carried by the surface, status is treatment (solid vs dashed frame `data-status`),
  stated once in the section legend and detailed only on `/roadmap`.
- **The diagram and the reading swap sides by index** (`data-side="visual"` on even plates `copy` on odd ones). The swap is done with explicit `grid-column`/`grid-row` placement, not
  `order` alone: with `order` the canvas lands in the narrow band and half the frames shrink.
- **The reading column is a fixed band** (`clamp(240px, 24vw, 330px)`), measured against the
  frame's height (`align-items: stretch`), so the diagram frame cannot be stretched by copy.
- **Identical frame geometry is a hard rule.** Every frame is the same width and height at
  every viewport: a fixed 33px head row (`PLOMID · name · op · hint`), a fixed 38px control
  row (chips for driven models, a one-line `drive` caption for the rest), one shared SVG
  viewBox (`0 0 660 340`), and a fixed-height interaction note. The head's operation slot
  (`[data-dm-readout]`) doubles as the live answer slot on driven surfaces, the time scrub
  names the moment, the vector probe names its nearest object, and both restore the
  operation label on leave. The control row is
  `flex-wrap: nowrap` with `overflow-x: auto`, a driven plate's chips pan instead of wrapping,
  so a long chip set can never change the frame's height. Nothing inside a frame may re-wrap
  on hover. The probe asserts one unique frame width/height across all plates.

#### The interaction engine (`src/scripts/models.ts`)

One script drives all eight surfaces. The plate binds `.dm-visual svg` (never any other
`<svg>`, glyph icons share the surface).

- **Topics are the joints, and they are one-to-one.** A `topics` map assigns each copy item
  (`data-dm-topic`, a real `<button>`) its own key; the matching SVG part carries `data-dm` +
  a written `data-note`. **Each text item lights exactly its own part**, no many-to-one
  families. That is why `Objects` has `object-0/1/2` `metadata` and `storage` as separate
  parts `Geo` has `radius / search / layers / scale` `Graph` has `path / edges / nodes /
  hub` (the hub being one real node), and key-value has a `hop` part for the wire itself. A
  topic carried by one element may also name a graph node (`data-gn`), so hovering the copy
  lights that node's neighbourhood; a topic carried by many elements is a set and lights as a
  set. Hovering or focusing either side lights both and rewrites the note; click pins, click
  again releases; `aria-pressed` tracks the pin.
- **Per-model drives.** On top of the shared system:
  · *time*, range chips (`1h/1d/7d/30d`) swap series + ticks; scrubbing reads out
  time·value (head and in-SVG read-out);
  · *geo*, the search point follows pointer **and tap**, on the plate's own projection; radius
  chips (`R 250 / 500 / 1k`) resize a real distance in kilometres; layer chips
  (`Hubs/Nodes/Sites`) toggle real point layers; zoom chips (`World/Region/Metro`) scale the
  geography and re-label the scale bar; hovering an object reads out its code + distance, and
  clicking pins it;
  · *vector* `k` chips (`3/5/8`) choose how many neighbours answer; filter chips
  (`Tickets/Docs/People/Noise`) remove a cluster from the candidate set entirely; the
  nearest neighbour is named in the frame-head readout (`nn: tickets · 42u`, or `nn: —`
  when the field is emptied); tap drops the query point. One `rerank()` is the single
  writer of the answer: pointer move, `k` chips and filter chips all call it (the chips
  re-rank the query's LAST position, so touch works without a following move), and
  `.near` is derived from the ranked top-k, never from a hard radius, so a point cannot
  glow without a line or carry a line without glowing;
  · *key-value*, pairs light end to end through the lookup rail;
  · *hybrid* `shape` chips add or remove a shape from the single request, the clause and its
  lane soften, and the in-SVG read-out counts how many shapes the statement is using.
- **The geo map is real geography, drawn small.** One equirectangular projection in
  `lib/viz.ts` (`GEO_SPAN` `GEO_X0/Y0` `geoLon/geoLat` `GEO_KM_PER_UNIT`) places both the
  coastlines and the points, so the map cannot disagree with itself: the scale bar is computed
  from the same constant the distances are, radius rings are drawn at their true size for the
  view, and every point is a real city at its real coordinates. Coastlines are twelve drawn
  landmasses from real anchors (straight segments, deliberately: smoothing would invent land),
  with a 30° graticule, continent names and hub labels that counter-scale against the zoom
  (`transform: scale(1 / var(--gz))` with `transform-box: fill-box`) so type stays legible at
  every detail level. Only the `world` layer is on by default; `region` and `metro` reveal the
  rest.
- **The Hybrid plate is the summary.** One statement at the top with a clause per shape, four
  lanes below it (`sql · geo · time · graph`), one result read-out, and, the point of the
  picture, a smaller inner application holding objects (`asset → metadata → storage`). It is
  `status: building` and says so: mixing models in one statement is direction, not a shipped
  capability, and the roadmap owns the detail.
- **One control row, no duplication.** A driven plate shows ONE toolbar: zoom chips
  (`World/Region/Metro`), radius chips (`R 250/500/1k`), layer chips (`Hubs/Nodes/Sites`).
  Never repeat the same label in two groups; never stack toolbars.
- **The note never resizes the frame.** Two fixed-height variants only: the interaction
  hint (available) and one plain-language direction line (building/future). Status prose
  still belongs to `/roadmap`.
- **Plate list vs model truth layer.** The nine plates are assembled in `ModelShowcase`
  (eight from `models` in `src/data/site.ts` + the Hybrid descriptor) so the truth layer keeps
  exactly the eight models the rest of the site, hero chips, roadmap, search, talks about.

#### Diagram palette (`--dgm-*`): the frame owns it

**Every diagram is drawn on paper, on both plate tones.** `.dm-frame` sets
`background: var(--paper)` and pins the whole light token scope inside itself (`--fg` `--fg-2` `--fg-3` `--line` `--line-strong` `--panel` `--panel-2` `--accent-on`) together with the four
`--dgm-*` values, so ink, hairlines and points keep one contrast system no matter which plate
wraps them. The plate-level `--dgm-*` rules are gone; the frame is the single owner.

| Token | Value on the frame | Used for |
| --- | --- | --- |
| `--dgm-fill` | `rgba(10 11 13 / .05)` | boxes, table bodies, ocean, lanes |
| `--dgm-fill-2` | `rgba(10 11 13 / .085)` | headers, emphasis fills, land |
| `--dgm-ink` | `rgba(10 11 13 / .72)` | keys, points, nodes, visible by default |
| `--dgm-ink-2` | `rgba(10 11 13 / .32)` | secondary values, noise, hairlines |

Accents inside diagrams stay `--accent` / `--accent-08/14` and the model hues
(`--sql --json --time`) regardless of plate tone; hover is always copper. A diagram drawn with
hardcoded colours instead of the palette is a bug, and so is a default fill that only becomes
visible on hover, the drawing must read before anything is touched.

### Two-way model explorer (superseded details)

The expander row-list is gone; the interaction contract it defined survives in the surface
system above. The probe still exercises the two-way rule (topic ⇄ hotspot ⇄ note) per
model.

---

## 10. Content rules

- **Capability status** is one of `available | building | future` in `src/data/site.ts`, and it is
  the single source of truth for the nav, roadmap tables, footer legend and sitemap.
- **Canonical URLs.** The production domain lives once `siteOrigin` in `src/data/site.ts`
  (`https://plomid.in`), imported by `astro.config.mjs`. Every public path is canonical and explicit
  with a trailing slash (`/developers/` `/docs/` `/playground/` …): the same spelling is used by
  `routes` (sitemap source) `Base.astro` canonical links, footer links and the search palette's
  route column. There are no abbreviated labels (`/dev` `/ref`) and no fake nested URLs.
  `routes` in `site.ts` publishes the site's own **22** paths (`/` `/platform/` `/architecture/`
  `/roadmap/` `/pricing/` `/download/` `/developers/` `/docs/` `/playground/`
  `/solutions/` `/sovereignty/` `/company/` `/careers/` `/investors/` `/press/` `/partners/` `/blog/`
  `/contact/` `/privacy/` `/cookies/` `/terms/` `/legal/`), and `registry.ts` merges those with
  the routes `industries.ts` and `solutions.ts` generate (**75** published pages in total). `/404`
  is noindex and stays out of the sitemap.
- **Section numbering.** **Decorative section numbers are not part of the PLOMID visual language.**
  No `01`–`12` eyebrow indices, no numbered stage chips, no "08 Deployment". Numbers that are real
  content (a `LIMIT 10`, a query result, a computed aggregate) may remain; internal data
  (`Layer.index` `WorkloadPattern.no` `captions[].k` `WorkloadPattern.idx`) keeps numbers in
  code, never on screen.

  This was audited across the built site, not just the pages that were obvious. The last visible
  decorative numbers were removed from: roadmap expansion columns (`evo-i`), roadmap definition
  columns, the roadmap system-layer table (`layer-idx`), the roadmap explore band (`08`), the
  roadmap plan (`plan-n` renders `01`–`04`, a real sequence, not decoration),
  developers getting-started steps and request stages, playground sample numbers and flow-strip
  numbers, platform cost columns (`Cost 01`) and request steps, solutions pattern numbers,
  sovereignty control dimensions and today-facts, company principles, blog topics and note numbers,
  the `StorageFlow` eyebrow (`04`) and
  the homepage roadmap group counts. `.idx` survives only as a **class name** on labels that carry
  words (`SQL + JSON` `Time series` `Next`), styling, never a number.

  The verification is mechanical: the probe strips `<style>`/`<script>`, then reports every visible
  text node matching a bare `0?\d{1,2}` or `Step XX` / `Cost XX`. The remaining matches are all
  query content (`LIMIT 5` `LIMIT 10`, order quantities).
- **Corporate pages.** `/investors/` `/press/` and `/partners/` are real pages, not shells: each
  states what the company is, who to write to, and what it refuses to claim. They are in the Company
  menu, the sitemap `routeTrail`, the search index and the company page's corporate band. Nothing
  on them invents a funding round, a media mention, a partner or a logo. Company-registration
  details are explicitly *not published yet* rather than guessed.
- **Newsletter.** `/roadmap` ends with a subscribe form that has **no backend**. It composes the
  request in the visitor's own mail client (`action="mailto:…"` `enctype="text/plain"`, plus a
  guarded script that builds the subject and body) and the visible note says so: "Opens a subscribe
  request in your email client. This site stores nothing and sends your address nowhere." The form
  is properly labelled (`<label for>`), typed (`type="email"` `required` `autocomplete`),
  keyboard reachable, and answers through an `aria-live="polite"` status line. No address is posted
  anywhere by this site and no fake success state is shown.
- **Status language is not a badge on every surface.** The homepage states its situation once per
  section and then lets the treatment carry it (solid = current, dashed = direction); the model
  explorer's caption strip now teaches only the interaction. `Developer`-facing phrasings that read
  like an internal tracker"Available surfaces""Fixed sample dataset, computed in your browser""Conceptual visualization of a planned model, not available to use yet""Direction, not
  available yet", were rewritten as product language. Nothing was hidden: the honest limits
  (no packaging, no enterprise integration, no deployment fabric) are still stated, in prose, where
  they belong `/roadmap` `/docs` and `/sovereignty`.
- **Text hierarchy.** Dark-surface tokens were lifted for readability without going pure white:
  `--t-d2 #b3b9c1` (readable secondary) `--t-d3 #878e97` (clear tertiary), panels one step
  stronger. Metadata stays smaller but never muddy; important explanations never look disabled.
- **Code rhythm.** `.code pre` runs `line-height: 1.38` with tight padding, a technical editor
  feel, not a marketing text block.
- **Presentation tiers.** The roadmap refines those three states into five tiers for readers `Current` `Current foundation` `Roadmap · in development` `Potential` `Long-term`, with one
  legend that explains each. The detailed capability map lives **only** on `/roadmap`; the homepage
  keeps the three-bucket view and links there. A tier is explained once and then carried by the
  treatment. The same page reads the tiers a second way as **the plan**, four moves in order,
  derived from the same rows (see "The plan and the map").
- **One query surface per page.** The developer story is told once. The homepage runs the console
  and then routes outward through *Explore PLOMID*; it does not repeat the same SQL walkthrough in a
  second section. The same applies to status: a layer's identity is its *role* (`What enters.`), not
  its release state, so badges appear only where status is the point of the surface.
- **Roadmap wording.** `Current` `roadmap` `in development` `potential` `long-term` `being
  explored` `not scheduled`. Never `in development` repeated as a badge on every surface, and never
  a date.
- Unfinished work uses roadmap language: *roadmap*, *future direction*, *being explored*,
  *planned*. It is never described as shipped, and no dates are published.
- The word *available* is stated once per section, in prose. Repetition is replaced by the
  solid-vs-dashed visual rule.
- **Clients, not just applications.** The audience is deliberately broader than the app in a
  browser: AI clients, applications, services and jobs all reach the layer the same way. The hero
  block is headed *Clients* (`apps · agents · services · jobs`), the pipeline's entry stage is
  `Clients`, and the tagline reads "for applications and AI clients". The site never reduces the
  product to either audience alone.
- No invented customers, logos, testimonials, benchmarks, certifications or adoption numbers.
- Numbers shown in the console are computed in-browser from a fixed sample dataset and labelled as
  such.
- Terminology is fixed: *data model* (SQL, JSON, time series, vector, graph, blobs), *system layer*
  (interfaces, planner & coordination, execution, storage, deployment fabric), *workload*, *query
  surface*, *storage contract*, *data layer*.
- Emphasis is rationed: one `.q` or `.w-strong` per headline.

### Identity and contact

- The **mark** is never redrawn and never set in a font. Every rendered lockup — header, footer
  brand anchor, mobile drawer, the enterprise-integration core on `/roadmap/` — is `BrandMark.astro`,
  the official vector's own paths inline; the press kit (`/press/`) ships the generated SVG/PNG
  variants, and the favicon/OG set derives from the same geometry (`scripts/make-favicon.mjs`,
  `scripts/make-og.mjs`). Dark grounds take the `paper` variant, light grounds `ink`.
- The public email is `hi@plomid.in` (`contactEmail` in `site.ts`), used in the header CTA
  (`Let's talk` → `mailto:`), the footer note, the community section, the company/careers pages and
  the contact form. No placeholder email exists anywhere in the site data.
- The origin line **"Built in India. For the world."** (`originLine` in `site.ts`) appears in
  exactly three places, the footer brand column, the homepage community section and the company
  page (follow section + FAQ). It is deliberately not repeated elsewhere.
- Real social destinations live once in `site.ts` (`githubUrl` `linkedinUrl` `youtubeUrl` `xUrl`) and flow to the header, footer `SocialLinks`, structured data (`sameAs`) and prose
  links.

### Social presence

`SocialLinks.astro` is the single source of social markup, used by the footer, the homepage
community section and the company follow section. Rules:

- Real channels (GitHub, LinkedIn, YouTube, X) are `<a target="_blank" rel="noreferrer noopener">`
  with `aria-label` and `title` ("GitHub, Source, issues and releases").
- Channels without a confirmed URL (Discord, Slack, Facebook, Instagram, Twitch, Reddit) render as
  `<span>` with a dashed border and `opacity: .34`, **never** as a link, **never** with a loud
  "coming soon" badge. The accessible label carries the state ("Discord, Join community").
- When a real URL exists, set `url` in `site.ts` (`communityChannels` / `upcomingChannels`); the
  same component starts rendering a live link. No markup changes anywhere else.
- Interaction: `--t-fast` lift (`translateY(-2px) scale(1.04)`), copper border with a soft local
  glow, pressed state, visible focus ring; the lift is disabled under reduced motion.

### Community and download honesty

- **No invented invite URLs.** Discord and Slack appear only as the quiet placeholders described
  above until a real public URL exists.
- **No fake download.** Every button on `/download/` points at the repo releases
  (`https://github.com/plomid/plomid/releases`, `repoUrl` derived from `githubUrl`, never the
  org URL): 9 artefacts (`.dmg` ×2, `.exe`, `.zip`, `.deb`, `.rpm`, `.tar.gz`, Docker image,
  source) plus the shell installer served from this site. Checksums ship beside each file and
  the Verify section shows how to compare them; no versions, sizes or dates are stated on the
  page. Detection is browser-only (UA + `userAgentData` hints) and the manual matrix + OS filter
  is always present. The probe asserts `pkgs` `suggested` `releaseLinks` and no
  `download now|get the binary` CTA. The Get PLOMID sections (home `10`, developers `05`) offer
  the real entry points, playground, documentation, GitHub, and state plainly that source/release
  packaging is being prepared.

### Search content rule

`searchIndex` in `site.ts` is the one search corpus: page names, page descriptions, major section
headings, docs topics, architecture topics (MVCC, transactions, planning, access paths), roadmap
topics, developer topics, playground and legal pages, each with keyword strings. When a page gains
a major new section, add one entry, the palette picks it up at the next build with no other
change.

### FAQ content rule

FAQ answers may only restate what another page already states. Each question is checked against
the roadmap/architecture/company copy before it ships, and the same pairs are passed to `Base` via
the `faq` prop so the FAQPage JSON-LD mirrors rendered content exactly (tags stripped in the
schema). Pages currently publishing FAQs: home (8), pricing (6), download (5), company (4),
playground (4), sovereignty (4), deliberately different questions per page, never one shared block.

### Breadcrumbs

There is exactly one breadcrumb implementation: `PageHero`, fed by `routeTrail` in `site.ts`. Pages
do not pass `crumbs`, the trail is looked up from the canonical URL, so the visible trail, the
`BreadcrumbList` JSON-LD emitted by `Base.astro` and navigation ownership (`groupForPath`) all read
the same array and cannot drift. The current page is the deepest segment `aria-current="page"` in
the accent tone. The standalone `Breadcrumbs.astro` was deleted: it was a second way to render the
same hierarchy, and nothing imported it.

The page name appears once. `PageHero` suppresses its eyebrow (`showMeta`) whenever the trail's
deepest segment already terminates at the page being viewed, either because it has no destination
(`/architecture/` → `PLOMID / Product / Architecture`) or because its destination *is* this URL
(`/platform/` → `PLOMID / Product`, where `Product` links to `/platform/`). The eyebrow row is not
hidden with CSS: the element is not rendered. This was the last surviving duplicate: `/platform/`
previously printed `Platform` directly beneath its breadcrumb.

---

## 11. Accessibility

- **Landmarks**: `header` `nav[aria-label]` `main#main` `footer`, a `.skip` link to `#main`. The
  search palette is `role="dialog"[aria-modal="true"]` with a labelled input and listbox.
- **Headings**: one `h1` per page. The page hero owns it, or the home hero does. Sections use `h2`,
  components use `h3`/`h4`. Nothing is sized with a heading tag.
- **Names**: every icon-only control has `aria-label`; decorative SVG is `aria-hidden="true"`; the
  big diagrams are `role="img"` with a written `aria-label` describing what they show. Social
  placeholders are `role="img"` with a label that says the channel is not connected yet.
- **Selection patterns**: three are used, chosen by structure, never mixed.
  - *Tab set* `LayerPipeline` and the architecture query
    lifecycle: `role="tablist"` owns only `role="tab"` children (`<li>`
    wrappers are `role="presentation"`) `aria-selected` + `aria-controls` `←/→` (or `↑/↓`) move
    between tabs, and the panels live in a separate container, for the lifecycle, the container
    sits directly beneath the flow, so the answer is never somewhere else on the page.
  - *Disclosure* `ModelShowcase` and `Faq`: each trigger owns the panel it reveals, so it carries
    `aria-expanded` + `aria-controls` and the panel is `role="region"` labelled by the trigger. A
    tablist would have to own the panels, which the in-place rule forbids.
  - *Listbox (transient)*, the search palette results: `role="listbox"` + `role="option"` with
    `aria-activedescendant` following the cursor.
  - In the tab and disclosure patterns, activation follows focus, so keyboard users get the same
    response the pointer does.
- **Live regions**: console output and dynamic detail panels use `aria-live="polite"`.
- **Focus**: never removed. `:focus-visible` draws a copper ring on every control. The search
  palette moves focus into the dialog on open and restores it to the trigger on close.
- **Pointer-only information**: never. Anything revealed on hover is also revealed on focus and
  tap, and the same information exists as text in the DOM.
- **Contrast**: body text is `#f2f2ef` on `#08090b` and `#0a0b0d` on `#f3f1ec` (both > 15:1).
  Secondary text is `#a0a6ae` on ink (7:1) and `#4b5057` on paper (7.4:1). The tertiary token
  `--fg-3` is reserved for labels at `11px+` and never for body copy.
- **Touch**: minimum 40px hit area for header controls; 44px effective for primary actions. Below
  400px the header action row compacts (search icon only, tighter CTA) so nothing overflows a 320px
  viewport.
- **Reduced motion**: see §7.

### Active navigation

The current section in the header is marked with **one bottom line only**, there is deliberately no
top bar, no pill and no glow. The line is the entire interaction language, shared by hover and
active state:

- default → normal `--t-d2` text;
- hover / keyboard focus → the 1px copper line grows from left to right (`transform: scaleX(0→1)` `transform-origin: left` `200ms var(--ease)`);
- active page → the line is already fully present at 75% opacity;
- active + hover/focus → the same line strengthens to full opacity and 2px height.

`aria-current="page"` stays on the resolving link (and the dropdown/drawer equivalents), so state
never depends on colour alone. Dropdown triggers carry the same bottom line. The mobile drawer
mirrors the active state with a copper left border on its primary rows and copper text inside the
directory.

### The mobile drawer is a directory, not an accordion

Below 1080px the drawer shows **everything, always expanded**. It is composed as one scroll in two
registers:

1. **The five destinations**, as large always-visible rows (`.dlink`): Home, the platform pages,
   Solutions, Industries, the developer pages, the company pages, Playground — the same groups as
   the navbar, in the same order, with mono side-labels (Explore / Try it / Source).
2. **The full taxonomy** (`.drawer-dir`): four solution families, six industry categories and the
   policies as flat groups — a mono key (`.dsub-k`) over a wrapped row of links (`.dsub-body`).
   No `<details>`, no hidden state, no scripting; the drawer answers *"what all is here"* in one
   glance and every one of the fifty-plus destinations is a single visible link.

A shrunken mega panel is never squeezed onto a phone, and the drawer never hides the taxonomy
behind taps: the phone reader gets the whole site map, not a summary of it.

---

## 12. Industries and solutions

The industry surface is not a section bolted onto the site; it is a second axis over the same
data. It exists so a visitor can arrive from a domain (oil & gas, banking, healthcare) and reach the
platform through the workloads that domain actually runs, instead of through a feature list.

### The two-level rule: industries are parent pages, environments are not industries

The single most important rule of the taxonomy: **an industry is a destination, an environment is a
context inside it.**

- The **primary industries** — Industrial, Financial Services, Enterprise & Commerce, Science &
  Health, Infrastructure & Technology, Public & Sovereign — each have their own page at
  `/industries/<slug>/` (`industryCategories[].slug`, e.g. `/industries/public-sovereign/`). These
  are what the navbar, the footer, the landing map, the breadcrumb chain and the sitemap point at.
  There is no numbering anywhere: an industry is named, never ordered.
- The **environments** (oil & gas, banking, government, edge infrastructure … all 33 `Industry`
  records) have their own pages at `/industries/<slug>/`, but they are **not** navigation
  destinations in their own right at the industry level. They are reached from their industry's
  page, from `/industries/`, from search, and from the drawer directory — never presented as a
  numbered sequence of sub-industries.
- A primary industry page explains its environments as connected contexts over one data layer
  (`IndustryTopology`), and each environment page links back up to its industry (`brief-parent`).
- The distinction is enforced in one place: `isPrimaryIndustry()` excludes the six category slugs
  from `/industries/[slug]`'s `getStaticPaths`, so no URL can ever be answered by two pages.

### The data layer

| File | Owns |
| --- | --- |
| `src/data/site.ts` | The platform truth: models and their status, layers, workload patterns (each pointing at the solution that carries it), navigation lists, the site's own routes and trails, the base search corpus |
| `src/data/industries.ts` | The industry taxonomy (six primary industries, each with `slug`, `label`, `blurb`, `summary`, `lede`, `environmentsLead`, `seo`), the data-type vocabulary, the platform-capability vocabulary, and all 33 environments |
| `src/data/solutions.ts` | Four solution families, all 18 solutions, the relationship helpers, and the industry side of every relationship (derived, never written twice) |
| `src/data/registry.ts` | The only place the three meet: merged routes, merged breadcrumb trails, merged search corpus, and `groupForPath` (navigation ownership) |

The graph is deliberately acyclic: an industry may read the model truth layer, the model truth
layer never knows an industry exists, and `registry.ts` is the single consumer that sees both.
Adding an industry therefore registers its route (sitemap), its breadcrumb chain, its navigation
ownership, its mega-menu entry, its footer entry and its search entry in one edit.

### Status is derived, not asserted

`statusForTypes()` reads the models a page declares and returns the **least-finished** one, the same
rule the workload patterns already used. An industry or solution page never asserts status in prose:
the **treatment** carries it. A shape, model or frame that rides on something still being built keeps
the dashed frame (`.defs-item[data-status='building']`, `.idg-frame[data-status]`, a dashed diagram
frame), the capability map is one quiet link away, and the page itself reads as a product page rather
than as a status dashboard. Nothing can claim to be available while depending on something that is
not, and nothing internal is narrated at the reader.

### The relationship, in both directions

`Industry.solutions` is the only place the industry → solution edge is written.
`industriesForSolution()` derives the reverse, which is what the solution pages, the explorer and
the landing page all read. A relationship therefore cannot exist on one side and not the other.

### What these pages deliberately do not contain

No customers, deployments, sites, certifications, benchmarks or adoption numbers; no stock imagery
and no illustration of people, rigs or factories. The visual is data: the hero drawing, the workload
index, the landscape and architecture bands, the use-case marks, the workload map and the capability
rail. Direction keeps the dashed treatment and a link to the capability map rather than implying it
ships, and no page carries a "what this page does not claim" section — an honesty note about the
website is not a product surface.

### Navigation, and why Industries is one item

The navbar has five discovery hubs and no more: **Product**, **Solutions**, **Industries**,
**Developers**, **Company**. Industries and Solutions open `MegaMenu` panels, because both lists
grow and the navbar does not. The panel is a **names surface**, not a document:

- a **feature column** (`.mega-feature`, left, copper eyebrow, darker plate) carrying the section's
  argument in one sentence, the real way in (*Explore all →*) and the sibling hubs under *Also* —
  the menu has a voice, not just links;
- the taxonomy beside it on a **hairline grid** (1px gaps over `--line-d`): each group a cell with
  a mark (`categoryIcon` / `familyIcon`), a bold **destination heading** and its one-line context.
  In the Industries menu the heading IS the industry's own page (`categoryPath`), and the rows
  beneath it are the environments inside that industry. **Names only**: no counts, no numbering and
  no per-row descriptions — a menu that has to be read in full is not a menu. Explanations live on
  the pages, one click away;
- a **start-here strip** (`.mega-start`) on its own hairline row, three concrete destinations with
  their marks, so the panel ends on a decision rather than on a link count;
- `aria-current="page"` on the link you are already on, so an open menu still shows position;
- **anchored to the viewport** (`position: fixed` under the header, `min(1180px, 100vw - 2×gutter)`),
  never to its nav item: a panel this size hung off `left: -12px` would overhang the last item in
  the row and shift with every menu. No layout shift, one width rule for all triggers. Between
  1080 and 1240px the feature column narrows before the taxonomy gives up a column.

**Menu interaction.** Hover, focus and the current page all answer the same way: the label and its
mark turn copper, and a **2px copper signal** (`::after`) draws down the row's own left edge — the
same signal language the primary items use as their underline. The dropdowns (Product, Developers,
Company) carry name rows only; their descriptions were removed because five doors do not need a
paragraph each. Dropdowns open downward only, `aria-expanded` on the trigger, and every row is a
tab stop.

**The surface's own geometry.** The opened panel is not a bare list: it carries the GeoForms
language at nav scale, in three tiers, each RELATED to what it sits with and never one drawing
stamped everywhere —

- **surface marks** — an arc opening from the panel's top-left and a coordinate crosshair cropped
  into its bottom-right, behind the content (`mega-geo`), so the surface reads as constructed
  architecture rather than a floating card;
- **feature-column mark** — one grid or arc in the feature column's corner (`mega-feature-geo`);
- **per-group marks** — every group (each industry, each solution family) carries its own GeoForm
  cropped into its corner (`mega-group-geo`), chosen by `geoForGroup()` from the group's identity:
  Industrial runs an arc (machinery curvature), Financial a coordinate crosshair (calibration),
  Enterprise & Science grids (records), Infrastructure a crosshair (systems), Public & Sovereign an
  arc opening from its corner (boundaries); solution families follow the same logic (foundation =
  crosshair, intelligence = grid, trust = arc). Deterministic, aria-hidden, `pointer-events: none`,
  clipped by the group's own box so nothing can widen the panel.

Below 1080px the navbar collapses to the drawer, and the same taxonomy appears there as the
**flat always-expanded directory** described in §11 — with one change: each industry heading in the
directory is now itself a **link to that industry's page** (`a.dsub-k`), so the two-level rule is
expressed in the drawer too: the industry is a destination, the environments listed under it are
contexts.

### Nav legibility is a floor, not a preference

The five primary labels render at **full text strength** (`--t-d`, weight 550, `0.9375rem`) with
`0.01em` tracking; hover and the current section answer in copper (`--accent-2`) with the copper
underline signal. Dropdown and mega-menu rows render at `0.9375rem` / `--t-d2` (weight 550 in the
dropdowns) and lift to `--t-d` or `--accent-2` on hover. The header is the one surface where quiet
grey is a defect — a nav item a reader cannot read is a nav item that does not exist. The GitHub
header link follows the same floor at weight 500.

### `IndustryTopology`: one industry, its environments, one layer

The drawing a primary industry page is built around. The industry is the root, its environments
hang beneath it, and one layer bar resolves all of them:

```
                PUBLIC & SOVEREIGN
                        │
      ┌─────────────────┼─────────────────┐
      │                 │                 │
  GOVERNMENT     SOVEREIGN INFRA     EDGE INFRA
      │                 │                 │
      └──────── PLOMID data layer ─────────┘
```

It is HTML + CSS rather than one SVG, on purpose: every environment is a real link with real text,
so the picture is navigable, selectable, screen-reader readable and unchanged with scripting off.
The connectors are hairlines sized in the layout, so the topology cannot misalign or overflow at
any width. Hover or focus lights that environment's own wire, node and name in copper — a signal,
not a state. Two modes: `hero` (compact, names only) and `section` (the full band with one-line
summaries). On a phone the fan becomes a spine: the environments stack down one vertical line and
the layer sits at the bottom, so the drawing still reads as one industry with several contexts.

### `IndustryDiagram`: one component, five compositions

The domain's visual story. It is drawn from `stages` (name · note · the data types present) and
`types`, and it follows the site's rules rather than inventing a visual language:

| Variant | Used for | What it draws |
| --- | --- | --- |
| `flow` | Production and process domains | A spine of stages with wires, a stem to the baseline, and the data footprint of each stage as bars |
| `network` | Fleet, plant and network domains | A core plate (*one layer · one plan*) with stages on a ring, wired in |
| `graph` | Transactional and relationship domains | Entity nodes with edges, including the second-hop edges a relationship workload really has |
| `documents` | Evidence-heavy domains | Document sheets with their metadata brackets |
| `sovereign` | Places where the system is allowed to run | A dashed boundary, a core, a rail and the stages inside it, with the exit marked |

Interaction is the documented in-place rule: the rail (a real `role="tablist"` with roving
tabindex and ←/→ Home/End) sits directly above the read-out, and choosing a stage lights that
stage's node, its bars and its data chips in the SVG. The chips work in the other direction too:
focusing a type lights every stage that carries it and answers with the stages by name. Motion is
one primitive: a copper dot on the active node plus marching hairlines; under reduced motion both
stop and the diagram is a complete static schematic, and with scripting off the rail, the read-out
and every stage are still there.

`--dgm-*` and the paper token scope are pinned by `.idg-frame`, exactly as `.dm-frame` does it, so a
diagram reads correctly on ink, graphite and paper sections alike. Below 760px the composition pans
inside its own frame (`min-width: 620px`) rather than scaling its labels to 4px, the same rule the
model plates use.

### `IndustryExplorer`: the industry map

The explorer at the centre of `/industries/` is a **map, not a list**. Six industry nodes sit in a
row above a bus that resolves into one `PLOMID data layer` plate — the same topology the primary
industry pages draw. The nodes are a `role="tablist"` with roving tabindex and ←/→/↑/↓ Home/End;
choosing a node opens that industry's panel: its environments (each a real link), the data shapes,
workloads, the solutions and the platform capabilities counted across those environments. Hover
lights a node and its connector in copper; the chosen node keeps the copper edge. Nothing is
numbered and no node carries a count. With scripting off, the first panel is open and every link in
every panel is still in the document.

### Breadcrumbs: two levels, both real

The chains are now:

- `HOME / INDUSTRIES / PUBLIC & SOVEREIGN` — the primary industry page;
- `HOME / INDUSTRIES / PUBLIC & SOVEREIGN / GOVERNMENT` — an environment inside it.

Both levels are real pages. The middle segment links to the primary industry (`categoryPath`), so a
reader can always walk up from an environment to its industry, and the `BreadcrumbList` JSON-LD
publishes an `item` for every segment that has a destination. The terminal segment stays
`aria-current="page"` and is never a link, and JSON-LD and UI still read the same array.

### The primary industry page (`PrimaryIndustry`)

A primary industry is a parent destination, and its page is composed as an architecture, not as a
category label:

1. **Hero** — the industry, its statement, and its environments drawn as an `IndustryTopology`
   (root → environments → one layer) beside the copy, over the page's `DomainBackdrop`, with the
   hero's ambient field running (`signals`, one pointer light);
2. **Environments** — the full band: `IndustryTopology` at section size, each environment named,
   summarised and linked. Never numbered, never presented as an ordered sequence;
3. **Data landscape** — environments → shapes → one layer → the work it supports (`DataFlow`,
   `landscape` mode);
4. **Workloads** — `WorkloadIndex` across the environments;
5. **Architecture band** — workloads → models → the layer → surfaces;
6. **What carries it** — the platform capabilities and recurring solutions counted across the
   industry's environments (derived, never chosen for the page);
7. **Directory** — `SiblingNav` over every environment;
8. **Closing band** — the argument and the way in.

The environments' content is the union of the taxonomy: nothing on a primary page is written twice
in the data files.

### The environment page links up

An environment page (oil & gas, government, edge infrastructure …) carries a **`brief-parent`
link** at the top of its brief sidebar: `Industry → Public & Sovereign →`, pointing at the primary
industry. The breadcrumb above says the same thing. An environment is never orphaned from its
industry, and never presented as the industry itself.

### The detail pages are editorial, not templates with the serial number filed off

An industry page is composed, not generated. After the hero it opens on **the brief**: the domain in
plain language (`Industry.lede` + `Industry.overview`), set as a two-column spread — wide editorial
column on the left, a **start-with rail** on the right (the two highest-priority solutions of *this*
industry, name + one line, on a copper-edged panel) so a first-time reader always has a path in
chosen from the page's own data, never generic navigation. Then the domain's own composition order:
sovereignty-bound industries lead deployment, knowledge domains lead use cases as paired columns,
operational ones as hairline rows. The measure is held (58–62ch), headings use the `.sec-head`
split, and every section ends in a link outward — the page reads like a document that knows where
it is going.

A solution page opens on the problem/answer spread, then **the shapes defined**: every data type the
workload names gets a one-line plain-language definition (`.defs-list`, status via the same
derivation as everywhere else) *before* the interactive map appears, so the diagram never names a
shape the reader has not already been given the meaning of. Definition first, then the workload map,
then outcomes, platform, status and related work.

The shared rule across both: **no mess of cards**. Structure comes from hairline grids (1px gaps
over `--line`), copper status edges and type hierarchy — not from boxed widgets competing for
attention. The page is one argument, and every section is a paragraph of it.

### The seam: `SectionSpine` fills the gaps between sections

Empty vertical space between two heavy sections is where a page reads as unfinished, so the gap
gets the site's own drawing language: one hairline, the isometric mark, a mono label
(`PLOMID · INDUSTRIAL`, *From shapes to questions*), and a run of squares alternating solid and
dashed — the same solid/dashed language the site uses for status. It is a **seam, not a
section**: `aria-hidden`, small type, quiet lines, used **sparingly** (one or two per page, where
the composition genuinely changes; never between every section). The label names what is coming
next, which is how a long page announces its own strategy.

### The "you are here" rail: `SiblingNav`

A detail page is one of a set, and a set has neighbours. `SiblingNav` lists them — the other
industries of the same category, the other solutions of the same family — each with its status
dot and one line, plus the hub they all hang from. It is derived from the data (the sibling set
minus the current page), never hand-written, so the rail cannot go stale or disagree with the mega menu and
footer. On the primary industry pages the rail lists the industry's **environments**; on
`/industries/` it lists the **six primary industries**. This is what makes the section read as one
designed system: every page ends by pointing at its own siblings.

### Routes

The site publishes **81** pages: the 22 base routes from `site.ts`, the `/industries/` landing
page, the **6 primary industry pages**, the 33 environment pages and the `/solutions/` hub with
its 18 solution pages. The primary industries sit between the hub and the environments in the
hierarchy (`/industries/` → `/industries/public-sovereign/` → `/industries/government/`), and
`isPrimaryIndustry()` keeps the two levels from ever colliding on one URL. Every page is in
`sitemap.xml` (generated from the merged registry) and carries a unique title, a unique
description and a canonical URL.

## 13. Performance

- **No client framework.** Astro islands are unnecessary, the interactive components ship a single
  inline `<script>` each.
- **No dependencies.** No animation library, no canvas, no WebGL, no particle engine. Signals,
  particles and diagrams are CSS + inline SVG. The refinement passes added none, the hero's ambient
  life, the query lifecycle, the access path, the MVCC diagram, the capability map, the interactive
  data-control map, the storage stage strip and the explore cards are all plain CSS, inline SVG and a
  few lines of guarded vanilla JS.
- **Animation**: `transform` and `opacity` only, so the compositor does the work. Loops are
  `linear` or a single easing curve; nothing animates layout properties.
- **Pointer handling**: one throttled `requestAnimationFrame` for parallax across all opted-in
  elements; pointer listeners are `passive`. Effects are skipped entirely on coarse pointers and
  under reduced motion.
- **JavaScript**: shared behaviour in `src/scripts/site.ts` (nav, drawer, reveals, parallax,
  scroll-drive, copy, year) plus per-component scripts. All listeners are guarded, so a missing
  element never throws.
- **Assets**: the favicon set and social images, all derived from the official emblem geometry by
  two design-time scripts (nothing redrawn, no font substitution):
  `scripts/make-favicon.mjs` writes `favicon.svg` `favicon.ico` (16/32/48) `favicon-16/32/48.png`
  `favicon-192.png` `apple-touch-icon.png` (180×180, ink tile) — the **copper emblem on
  transparent**, recognisable at 16px on light and dark browser chrome; `scripts/make-og.mjs`
  writes `public/og.png` (**1200×630**, full-bleed: official lockup + tagline left, the emblem's
  contour traced in copper right, one copper seam along the bottom — no inner frame, no dead
  margins). `Base.astro` serves `favicon.ico` + `favicon.svg` + PNG fallbacks + the apple-touch
  icon, and points `og:image` / `twitter:image` at `/og.png`. Fonts are the only third-party
  request.
- **Layout stability**: SVG diagrams declare a `viewBox` and `height: auto`, so they reserve their
  space. Reveals animate `opacity`/`transform` only, which never reflows.
- **CSS**: plain custom properties, no preprocessor, no utility framework. Tone variables mean a
  component's rules are written once and work on all three surfaces.

---

## 14. Verification

`npx astro check` (types + Astro diagnostics) and `npm run build` must both pass.

`scripts/site-probe.html` drives a real browser across every route and the widths
`320 / 375 / 390 / 430 / 768 / 1024 / 1280 / 1440`, then exercises the interactive surfaces
(run headless Chrome with `--window-size=1600,1200` as the file header instructs, without it
IntersectionObserver is clipped and reveals report as stuck):

- horizontal overflow per route per width (elements inside a horizontal scroller are exempt,
  because panning inside a frame is the documented pattern for wide diagrams)
- scroll-through reveal check, nothing may stay at `opacity: 0` after the page is scrolled
- no-JS check, with scripting disabled the reveals must still be visible
- hero ambient layer and atmosphere presence, and the count of status badges in the hero
- **model response adjacency**, the measured pixel gap between the model node that was clicked and
  the stage that answers it
- **home glimpse**, nine rows, one link, one filled row, and no interactive pipeline left on `/`
- **footer seam + wordmark** `.f-rule` first and above the CTA band; the closing mark renders
  as a horizontal outline lockup (symbol `scale(0.2472)` beside letters `translate(49 -1386)`,
  letter paths keeping the logo's own leading coordinates `202/515/841/1127/1478/1597`), above
  the legal bar; `.f-end-glow` fixed to the viewport's bottom edge
- **roadmap plan**, four moves, their derived capability lists, and no `.evo-col` remnants
- HeroSystem model isolation, the vector similarity field, graph edge focus, blob metadata coupling
- the platform model map → explorer hand-off, the playground run flow, roadmap and architecture
  structure

Run it with the commands in the file header. The report prints inside `<pre id="out">`.

**Last run (experience refinement pass: careers page, nav IA, capability-map exploration, deployment scale, favicon/OG).**
`astro check` clean (**0 errors / 0 warnings / 0 hints**) and `npm run build` clean (**81 pages**).

- **Navigation IA** — Press & Brand and Blog moved out of Developers into Company (both in the
  dropdown and the footer's Developer/Company groups); `Careers` now points at its own page
  `/careers/` with its own route, breadcrumb trail (`PLOMID / Company / Careers`) and search entry.
- **Careers page** (`/careers/`) — a full company surface: the thesis in brief, the six problem
  surfaces (storage/layout, planning, execution, MVCC, developer surfaces, deployment fabric —
  the same areas the company page names), the six principles restated as culture, the honest
  openings status and a three-step application route. No invented roles, perks or claims.
- **Capability map exploration** — rows are focusable; hover previews, click pins (`data-cap-pin`),
  tier companions stay lit while the rest quiets; the sticky read-out (`.cap-detail`) shows
  direction, example, tier, the plan move that arrives the capability and related-surface links
  (model explorer, architecture, infrastructure table) — all existing content.
- **Homepage deployment diagram** — the plate now takes the full container width on its own row
  with the tier controls + readout beneath it (was a 1.2fr/0.8fr split), roughly twice the linear
  scale; the SVG, tiers and labels are unchanged.
- **Favicon + OG** — `favicon.ico` (16/32/48) + `favicon.svg` + PNG fallbacks + ink-tiled
  apple-touch icon, all the copper emblem from the official geometry; `og.png` regenerated at
  1200×630 full-bleed (lockup + tagline left, traced copper emblem right, seam bottom). Wired in
  `Base.astro` and verified in the built HTML (`og:image → https://plomid.in/og.png`).
- **Enterprise integration** — the PLOMID core renders the real `BrandMark` lockup (paper variant)
  instead of a text stand-in; Infrastructure and Enterprise rows carry `NavIcon` marks.

**Previous run (glimpse, footer seam + logo wordmark, roadmap plan, AI-clients pass).**
`astro check` clean (0 errors / 0 warnings / 0 hints) and `npm run build` clean (**20 pages**).
Verified on the built HTML:

- **Home architecture glimpse**, the interactive pipeline is gone from `/`; the page renders
  `ArchGlimpse` instead: nine `.ag-row`s, exactly one filled (`data-tier="payload"` = `Data`), one
  travelling signal, and the whole plate as a single `<a href="/architecture/">` with an
  `aria-label`. `home stack detail removed: lpStages=0 lpPanels=0` is the new probe assertion.
- **Footer seam + wordmark** `.f-rule` is the footer's `firstElementChild` and sits above
  `.footer-cta` (the probe measures both). The wordmark's six paths start `M 202 / M 515 / M 841 /
  M 1127 / M 1478 / M 1597`, the fingerprints of P · L · O · M · I · D in the official vector, so
  a redrawn or font-set mark fails the check, and `.f-signature` sits above `.f-legal`.
- **Roadmap plan**, four moves with derived capability lists `7 / 3 / 5 / 6` (the last including
  the four infrastructure terms), a horizon signal, one tier flag per move, and zero `.evo-col`
  remnants. Move 04's list is exactly `Distribution, Multi-region, Replication, Multi-storage,
  Multimodal data, AI memory`.
- **AI-clients pass**, hero block `Clients`, pipeline entry `Clients`, platform map
  `ai clients · applications · services · jobs`, tagline/OG/footer blurb updated everywhere from
  the single `tagline` source.
- The homepage premise SVG (`premise-field`"Several systems, several copies") was removed;
  `Convergence` remains the single fragmentation→convergence visual.

The full headless-Chrome probe could not be re-run in this session's sandbox (headless Chrome hangs
here; last full run before these changes reported 0 overflow on all 128 route × width checks). The
new probe assertions listed above run in the harness and are quoted from the built output; re-run
the harness per the header instructions when a full report is needed.

**Previous run (public product experience + architecture depth pass).** `astro check` clean (0 errors /
0 warnings) and `npm run build` clean (**20 pages** + `robots.txt` + `sitemap.xml`), with 19 routes in
`sitemap.xml` and `/404` excluded. The probe reported **0 overflowing elements on all 128 route ×
width checks** (16 routes × 8 widths), the geometric atmosphere is flush to its corner and cropped
by its own `viewBox` precisely so it can never be counted as page overflow. Interaction checks:
hero model isolation `sql`; hero structure `lanes=3 wires=6 roadmapChips=3 statusLabels=0`; hero
ambient `layer=true atmos=16 heroBadges=0`; model universe `6` nodes with the vector panel opening
under its row (`gap=-6px`, visual `871px`); **vector field `cx 330 → 132.0, near=10, live=true`**;
**graph `focus=7 litEdges=2 neighbours=2`**; **blob coupling `hotspotsLit=3 dataLink=objects`**;
status language `badges=0 keys=3 roadmapped=13`; platform map → explorer `tab=true
panelHidden=false`; playground flow `mid=[done,active,-,-,-]` `end=[done,done,done,done,active]`
with `6` console rows; roadmap `stations=3 glyphs=6`; architecture reveals `total=46
in-viewport-not-revealed=1`.

The probe's vector/graph/blob checks were **retargeted**, not relaxed: they previously drove the
now-deleted Future Direction component (`[data-fw-tab]` `[data-node]` `.obj-meta`), and now drive
the single model explorer (`[data-ms-item]` → `[data-ms-panel] svg` → `.q` / `.gnode` /
`.hotspot--obj`). Navigation ownership verified on the built site: `/architecture/` `/platform/`
and `/roadmap/` each render exactly one `data-current="true"` group (Product) `/playground/`
renders exactly one (Developers). Breadcrumbs verified: exactly one trail per page and exactly one
page-name line, the eyebrow is suppressed where the trail already names the page (`/architecture/`
→ `PLOMID / Product / Architecture`, no `Architecture` line below it) and kept where the trail stops
at the section (`/platform/` → `PLOMID / Product` + eyebrow `Platform`). Decorative-number audit:
zero bare numbers remain in visible text. One open item remains: a single `[data-reveal]` element in the
architecture page's first viewport that had not gained `.in` at the moment of measurement. It is
cosmetic (the element is visible with scripting off, because every reveal rule is gated behind
`[data-js]`) and is carried forward as a known nit rather than claimed as clean.

**Previous run (visual/product experience pass).** `astro check` clean (0 errors / 0 warnings) and
`npm run build` clean (17 pages + `robots.txt` + `sitemap.xml`). The probe reported **0 overflowing
elements on all 104 route × width checks** (13 routes × 8 widths). The one regression this pass,
the search palette exceeding a 320px viewport because its auto column track sized to max-content,
was fixed by pinning `grid-template-columns: minmax(0, 1fr)` on the panel and a capped viewport
column on the overlay. Interaction checks all pass: hero model isolation `sql`, hero structure
`lanes=3 wires=6 roadmapChips=3 statusLabels=0`, hero ambient `layer=true atmos=16 heroBadges=0`,
model universe `6` nodes with the vector panel opening under its row (`gap=-6px`, visual `871px`),
vector field responds (`cx 316 → 128, near=5`), graph `focus=true litEdges=2`, blob metadata
`3 cells lit`, status language `badges=0 keys=3 roadmapped=13`, platform map → explorer hand-off
`tab=true panelHidden=false`, playground flow `mid=[done,active,-,-,-]`
`end=[done,done,done,done,active]` with `6` console rows, roadmap `stations=3 glyphs=6`,
architecture reveals `total=44 in-viewport-not-revealed=0`.

Breadcrumbs verified on the built site, one trail per page, hierarchy-labelled, matching JSON-LD:
`PLOMID / Product / Roadmap` `PLOMID / Developers / Documentation` `PLOMID / Developers /
Playground` `PLOMID / Company / Contact` `PLOMID / Legal / Privacy / Cookies`. Navigation
active state: exactly one `data-current="true"` group on `/playground/` (Developers).
No `localhost` URLs in any built page. Search-index paths are all canonical trailing-slash.
Titles unique across all 17 pages. The scroll-mode sub-check still hangs under headless virtual
time (known Chrome quirk); the no-JS guarantee remains verified statically, every `[data-reveal]`
rule is gated behind `[data-js]`.

Interaction checks from the same run: hero model isolation `sql`, hero structure `lanes=3 wires=6
roadmapChips=3 statusLabels=0` `hero ambient: layer=true atmos=16 heroBadges=0`, model response
adjacency `row→panel gap=-6px, visualWidth=871px`, vector field responds (`cx 316 → 128` `near=5`),
graph `focus=true litEdges=2`, blob metadata coupling `3 cells lit` `status language on home:
badges=3 keys=3`, no-JS `hidden-by-opacity=0`, architecture `43 reveals, 0
in-viewport-not-revealed`, playground flow `mid=[done,active,-,-,-] end=[done,done,done,done,active]`.

Surface checks for the newer compositions: sovereignty destinations `4`, hover/click selects the
matching `[data-dest-panel]` with no console errors; homepage keeps exactly one query surface
(`qc=1` `.code=0`) and renders `4` explore cards with no `dev-experience` heading; architecture
layer detail renders `5` role labels and `0` status chips, with `8` lifecycle stages/panels; the
storage strip renders `4` steps; roadmap renders `6` workload cards `17` capability rows and a
`5`-tier legend.

---

## 15. The industry and solution page system

An industry or a solution page is a **product page**, not an article. It is read as a picture first,
with copy between the pictures supporting them. The rule that governs every change on these fifty-one
pages: if a paragraph is explaining a system, the section should be showing it.

### The copper hero object (`CopperObject`)

Industry and solution heroes do NOT carry the paper architecture plate. The hero of these pages is
the subject rendered as **one constructed copper object** — solid geometric forms with graphite
technical edges, floating in the page's own dark atmosphere over a faint wire environment, with a
few travelling signals. It is a thing you look at, not a schematic you study; the explanatory
diagrams stay further down the page (`DataFlow`, `IndustryDiagram`). The progression is deliberate:
**SEE** (hero object) → **UNDERSTAND** (bands, maps) → **EXPLORE** (indexes, directories).

Nine object variants (`CopperKey` in `visuals.ts`), each mapped from what the domain IS, never
random and never the same drawing stamped everywhere: `terrain` (mining — layered geological
forms), `machine` (manufacturing, automotive, robotics — machinery geometry), `grid` (energy,
utilities — pathways over one central form), `stream` (oil & gas, IoT, observability — a source
feeding a moving signal), `network` (telecom, fintech, security — dense connected nodes), `route`
(logistics, supply chain — route geometry with stops), `regions` (government, sovereign, edge —
jurisdiction frames over a copper core), `field` (healthcare, retrieval — a resolving field),
`ledger` (banking, payments, enterprise — records feeding decisions). Primary industries map the
same way (`categoryObject`). Copper is the object's material; the environment stays graphite, and
nothing glows beyond one soft halo. SVG + CSS only, no canvas, no library; signals and the halo's
breath stop under reduced motion; the object is `role="img"` with a written description naming the
subject and the shapes in play.

### The page order

| # | Environment page | Solution | What it is |
| --- | --- | --- | --- |
| 1 | Hero | Hero | Eyebrow, `h1` (the domain's own name), statement, lede, two actions, and the **`DomainVisual`** in the `visual` slot over the page's **`DomainBackdrop`**, with the hero's ambient field running (drifting signals + one pointer light) |
| 2 | `SectionRail` | `SectionRail` | Sticky in-page navigation under the header |
| 3 | The environment | The workload | One section of prose, then the `DataFlow` band in `landscape` mode |
| 4 | `WorkloadIndex` | Two drawn states | Industry: the interactive workload index. Solution: separate systems vs one layer, side by side |
| 5 | Landscape band | Shapes | Systems → data in motion → one layer → work |
| 6 | Architecture band | Architecture band | Workloads → data models → the layer → surfaces |
| 7 | Use cases | Workload map + stage table | Interactive, each with its own mark |
| 8 | Data models | Outcomes | The workload map plus the shape grid |
| 9 | Deployment & residency | Platform / related / siblings | Where the data may run, and what else touches it |
| 10 | Closing band | Closing band | The argument and the way in: *Describe your workload*, one secondary action, one quiet *Capability map* link |

The **primary industry pages** run their own order (see §12, "The primary industry page"): hero
with `IndustryTopology`, environments band, landscape, workload index, architecture band, what
carries it, directory, closing band.

Every page ends on the same closing band. There is no "what this page does not claim" section and no
facts panel anywhere on the site: honesty is carried by treatment and by linking to `/roadmap`, never
by narrating the website at the reader.

### `DomainVisual` — the explanatory drawing (no longer the hero)

> **Superseded as the hero visual.** Industry and solution heroes now carry the **copper object**
> (see "The copper hero object" above). `DomainVisual` remains the right tool wherever a page needs
> the full sources → layer → outcomes schematic, but it is no longer rendered in any hero.

Three bands, always in the same reading order, because the argument is always the same:

```
  sources (workloads, systems)            ← names from the page's own data
        ↓                                (input wires, one march)
  PLOMID · one data layer                 ← the slab every wire lands on,
        ↓                                  one scan line crossing it
  outcomes (the work it supports)         ← titles from the page's own data
```

- **Eleven archetypes** (`src/data/visuals.ts`, assigned per industry and per solution):
  `asset-network`, `topology-geo`, `transaction-network`, `entity-graph`, `factory-line`,
  `commerce-chain`, `system-convergence`, `vector-field`, `event-stream`, `region-topology`,
  `convergence-shapes`. The archetype decides **geometry only** — node shape, arrangement,
  connection style and its extra ground drawing (contours, a route, a transaction mesh, entity
  edges, a conveyor, a chain, legacy stacks, a similarity field, stream rails, region frames).
  Labels, workload names, shapes and outcomes come from the page, so fifty-one pages never show the
  same picture.
- **The frame owns the paper scope** (`.dv-frame`), like the model plates and the workload map: a
  paper instrument panel on any page tone, with the light token scope pinned inside it.
- **Motion: two primitives.** One slow dash drift on the wires and one scan line crossing the
  layer, plus a breathing live dot. No glow, no hue rotation, no particles inside the drawing.
- **Accessible by construction.** The SVG is `role="img"` with a written description naming the
  sources, the outcomes and the shapes; the shape row is real `<button>`s with `aria-pressed` that
  light their mark in the slab (and `data-shape-active` dims the rest); pointing at a source lights
  that source and its wire.

### `DataFlow` — the wide bands

One component, two readings. Four columns on a hairline grid with a connector gutter between them,
every column keyed in mono with a mark, every item a real name from the page's data:

- `landscape` — **Systems in the environment · Data in motion · One layer · Work it supports**.
- `architecture` — **Workloads · Data models · The layer · Surfaces**.

The gutter carries a single marching signal. A shape that rides on a model still being built keeps
`data-ghost` (dashed rule, quieter label) instead of a status label. Below `1080px` the band becomes
two columns, below `780px` one, and each column grows a short vertical connector — recomposed, never
shrunk below legibility.

### Icons

One family, `NavIcon`: geometric line glyphs in a 24-unit box, `1px` strokes, round joins, no fills,
`currentColor` so a mark turns copper with the label beside it. Sizes in use: `12–14` inside a chip or
a row, `16–18` in navigation and section marks, `20–22` on a shape card. Sixty glyphs, including ten
**environment marks** named for what they depict — `plane`, `twin`, `truck`, `tower`, `mining`,
`oil`, `robot`, `cart`, `money`, `mesh` — so an industry is recognisable by its own object, not a
generic network mark. Icons are `aria-hidden` by
default because the label always carries the meaning; `title` is only passed when an icon stands
alone. **No emoji, no second icon style, no illustration.** A reader should be able to recognise a
destination by its mark before reading its label.

### Background animation

Industry and solution heroes run the shared atmosphere system rather than one-offs: `PageHero`
accepts `ambient` (drifting `Atmosphere` signals + the site's one pointer light per section via
`[data-ambient]`) and every domain page carries its **`DomainBackdrop`** — the subject's own
geometry (contours, mesh, rails, field, frames) with slow-travelling copper signal paths along it.
The signals are dashed routes whose dash offset moves one dash length at a time, so the line reads
as data moving along a path, not as a loading bar. Motion begins with the document, never on
scroll. On a phone the backdrop keeps one signal and fewer atmosphere dots; reduced motion keeps
the geometry and stops every loop. Never: neon, hue rotation, particle storms, glow beyond the
pointer light, or a second light in one section.

`DomainBackdrop` draws the subject's own geometry behind a page, at low opacity, never taking a
pointer event: contours and a route for a geographic or asset domain, a node-link mesh for financial
and entity domains, rails for factories and event streams, a similarity field for vector domains,
nested frames for convergence and region topologies. One slow drift (`46s`, ±18px) is the entire
motion, removed under reduced motion. Atmospheric particles remain `Atmosphere` (12–16 drifting
points) and the pointer light remains `[data-ambient]`: at most one light per section, and the
background never becomes a particle demo.

### Imagery

The visual language is **drawn, not photographed**: diagrams, topology, telemetry fields, maps and
mono labels, all inline SVG so they cannot inflate the payload. There is no stock photography on
industry or solution pages and there should not be: an image would have to carry an overlay, a
data layer or a topology to belong on one of these pages, and the honest version of that is already
the drawing. The only raster assets are the app mark and `og.png`.

### Navbar

- Five groups and no more: **Product · Solutions · Industries · Developers · Company**.
- Every dropdown and mega-menu row is **mark + label** — names only, no per-row descriptions, no
  counts, no numbers. The group headings in the Industries menu are the industries' own pages.
- **One owner per route.** `groupForPath()` derives ownership from the merged trail registry, so a
  destination reachable from two places still activates exactly one group — and it is **listed in
exactly one place**: `/playground/` lives under Developers only, not in the Product dropdown and not
twice in the drawer.
- Active state remains one copper bottom line (`hover` grows it left→right, active is fully present,
  active+hover strengthens it) plus `aria-current="page"` on the exact link.
- **Mobile drawer** is a scroll of sections (Product / Workloads / Developers / Company), each row a
  mark, a large label and a mono hint, then the full taxonomy as an always-expanded directory with a
  mark on every link. Nothing is hidden behind a tap, nothing overflows horizontally, and tap targets
  stay ≥ 40px.

### Footer

- The footer is the **final architectural surface**, not a link block: seam → closing CTA band →
  **one line: brand anchor + four link groups** → contact row → **the closing copper signal** → the
  wordmark lockup → legal bar → end glow.
- The four groups are **Product · Developers · Industries · Company**, laid on the same row as the
  brand anchor at desktop (five columns), re-gridding to four/two columns only below 1081px. They
  are typography and spacing, never boxes, cards or counts. Group heads are **bold copper mono**;
  the hairline above each head draws copper on group hover or focus-within.
- The *Explore industries* link is a quiet inline copper after-link — no rule above it — matching
  the site's after-link language, not a second heading.
- **Industries** lists the six primary industries (`categoryPath`) — parent destinations only, under
  **footer-short labels** (`shortIndustry` in the component; the taxonomy keeps its full names in
  the navbar, the drawer and `/industries/`) — closed by one quiet *Explore industries →* link.
  There is no Solutions group: the footer names the way in for its four audiences; solutions stay
  reachable from the navbar, the drawer and `/solutions/`.
- The old industries band (`f-industries`, thirty-three links as six columns) is **removed**: it
  read as a spreadsheet, buried the real destinations and duplicated what the navbar and the drawer
  already carry.
- **Link hover** is the navbar's language at footer volume: the label turns copper and a copper
  rule draws under it, left to right (`background-size` 0→100%).
- The legal links live in the **Legal group**; the closing bar is the copyright line alone.
- The **closing signal** (`.f-signal`) is one copper rule drawn outward from the centre into the
  wordmark, the seam's echo at the other end of the page; it arrives complete under reduced motion.
- The **wordmark** stays the official vector as an outline lockup with its per-letter hover (the
  pointer's position picks one letter, ←/→ does the same from the keyboard), the quiet sheen loop and
  the solid-copper hover state — all of it paused under reduced motion.

### Interaction rules for these pages

1. **Answer in place.** A shape chip lights its mark inside the same frame; a workload row opens its
detail directly beneath itself; the workload map answers under its own rail.
2. **One open object at a time** in an index (`WorkloadIndex`, the explorer) — choosing another
   closes the first.
3. **Hover previews, press pins.** Preview state is `data-preview`/`data-shape-active`; a pressed
   chip stays pressed and is announced with `aria-pressed`.
4. **Focus is a first-class path**: every interactive element is a real `button` or `a`, focuses
   visibly, and the same states respond to keyboard.
5. **Nothing jumps the page.** Panels open downward only; the rail changes a marker, never scroll
   position.
6. **Reduced motion** removes every loop (dash drift, scan line, pulse, drift, sheen) and leaves the
   finished picture, the lit wire and the words.

### Mobile diagram rules

| Situation | What happens |
| --- | --- |
| `DomainVisual` under `760px` | Keeps its three bands and pans inside its own canvas at a `520px` minimum (`440px` under `420px`) |
| `DataFlow` under `1080px` | Two columns, gutters hidden; under `780px` one column with vertical connectors |
| Card grids | `minmax(min(<size>, 100%), 1fr)` tracks, so a 320px viewport can never be forced wide |
| `SectionRail` | Scrolls sideways inside itself; the page never scrolls horizontally |
| `WorkloadIndex` | Row becomes number + name + signal, shapes wrap onto their own line, panel stacks |
| Tables (`.stage-table`, `.spec`) | Stack into labelled rows rather than shrinking type |

`body { overflow-x: hidden }` remains a backstop, never the mechanism: each of these frames pans or
stacks on its own.

### Verification for this system

`npm run build` and `npm run check` must both pass, and the routes to spot-check after any change to
these components are one industry page per archetype and one solution page per family, plus
`/industries/` and `/solutions/` themselves.

## 16. The versioned docs system

Mintlify source, PLOMID rendering. `docs/docs.json` + `docs/<version>/*.mdx` are the only inputs;
adding a version is a folder plus a `docs.json` entry, zero code changes.

- **URLs carry the version**: `/docs/` redirects to `/docs/vX.Y.Z/` (latest,
  resolved at build time) → `/docs/vX.Y.Z/` (version home) →
  `/docs/vX.Y.Z/section/page/` (one indexable page per guide, trailing slash canonical).
  Mintlify-absolute links (`/vX.Y.Z/...`) are rewritten to `/docs/vX.Y.Z/.../` at render.
- **Rendering** (`src/lib/docs.ts`): frontmatter title/description → `Base` + `PageHero` (single H1
  in the hero; the leading Markdown H1 is dropped) → GFM via `marked` with design-system code
  figures (`.code`, existing `highlight.ts` palette), SQL-only build-time
  reflow (`wrapSql`, splits at top-level `;`/`,`/space or before `--`
  comments, strings/`$$` respected, un-splittable lines keep scrolling),
  `.docs-table` in `.table-wrap scroll-x`,
  `.note` for blockquotes. Mintlify `<CardGroup>/<Card>` → `.docs-cards`/`.docs-card`;
  `<Steps>/<Step>` → `.docs-steps`/`.docs-step` (JSX indent stripped before inner Markdown).
  `<Note>`/`<Warning>`/`<Check>` → `.note` callouts, `<Tabs>`/`<Tab title="">` →
  an APG tablist. Capitals inside backticks or mermaid labels (RowId, Mutex)
  pass through as inline code, never JSX.
  Any Mermaid syntax renders at runtime via the pinned Mermaid library to
  themed SVG inside a bounded viewport (max ~640px, min 260px so
  single-row chains keep presence; drag-pan and zoom explore within it,
  initial zoom keeps text readable) with hover glow and click popups; the source stays in a `<details>` with copy, and
  offline/invalid syntax keeps the readable source block with hidden
  controls.
  In-docs clicks arm a one-shot `sessionStorage` flag so the next page opens
  at `#docs-content`, not the hero; direct visits keep native scroll.
- **Layout**: `DocsSidebar` (version `<select>` + collapsible grouped nav +
  `⌘K` trigger) sits in a stretched wrap so its own `position: sticky` has
  room to travel; `body` uses `overflow-x: clip` (never `hidden`, which
  silently kills all sticky descendants). Article beside
  `.docs-article` (`.prose` at 88ch on the wide 1560px canvas) beside
  `.docs-toc` (h2/h3, sticky; inline collapsible under 1200px).
  Three columns → two (TOC hidden) → one (sidebar stacks). Tables and code pan in their own
  frames; the page never scrolls sideways.
- **SEO**: each versioned page emits canonical self, `TechArticle` + `BreadcrumbList` (via
  `registry.ts` doc trails), `DocsSidebar` version note links old → latest. `allRoutes` merges
  `getDocRoutes()` into `sitemap.xml` (latest 0.75–0.9, archived 0.5); `robots.txt` allows
  `/docs/` explicitly; `llms.txt` regenerates the full version/guide map per build.
- **Search**: `getDocSearchEntries()` merges into the `⌘K` palette corpus; the header trigger
  plus the sidebar `Search docs…` button open the same dialog. No search service, no dependency.
- **Verification**: `npm run build` + `node scripts/seo-audit.mjs` must pass with no broken
  `/docs/` links, one H1 per doc page, and sitemap/page parity.
