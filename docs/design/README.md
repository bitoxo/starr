# Design system: "Nachtaufnahme" (night exposure)

Starr's visual design system; the web UI follows it. This file is the source of truth for tokens, typography, shapes, motion and patterns, and it applies to everything in `web/`. UI contributions should follow it: use the tokens and components described here instead of new colors, fonts or one-off styles, and propose a change to this file first when a pattern is missing. The labels A1–A7 on some sections are stable names that code comments may cite.

## Direction

The poster love and warmth of Letterboxd, the calm and generosity of Apple TV. A dark, slightly blue night sky on which the covers glow. Exactly one accent, Polaris gold, which always means "something happens here": request, focus, attention.

1. **Covers are the stage.** The chrome steps back; color comes from posters, albums and scenes, and only from the title being shown, behind it (see "Color from the cover").
2. **One accent, one meaning.** Polaris gold only for the primary action of a view **or of a focused context** (hero, preview card, info strip, detail request card, inbox detail panel) and for focus; rows of equal rank never get gold. Categories and states have their own quiet colors, used as dots and chips only.
3. **Two audiences, one system.** Members get a streaming-like stage with generous whitespace; admins get the same tokens in a dense, calm working view.

Starr ships **dark only** in v1. All colors are tokens so a light theme can be added later.

## Logo

Star trails around Polaris: a long exposure where all stars circle the one fixed star, much like Starr is the fixed point the services revolve around. It is deliberately not a four-pointed sparkle, which today reads as "AI".

- Files: `logo/starr-mark.svg` (on dark), `logo/starr-mark-on-light.svg`, `logo/starr-app-icon.svg` (with background), `web/public/favicon.svg`.
- Wordmark: "starr" in lowercase, Bricolage Grotesque 800, `wdth` 78, letter-spacing −0.035em. Set as text, not an image.
- Minimum size 16 px. Never recolor the trails in a category color; Polaris stays gold.

## Color tokens

| Token | Value | Use |
|---|---|---|
| `night-0` | `#0B0F15` | page ground |
| `night-1` | `#121821` | surfaces, sidebar, tables |
| `night-2` | `#1A2230` | raised: dialogs, popovers, active nav |
| `line` | `#263042` | hairlines, borders |
| `text` | `#E9EDF2` | primary text ("starlight") |
| `text-2` | `#9AA6B8` | secondary text |
| `text-3` | `#7E8BA1` | captions, table headers (≥ 4.5:1 on all night surfaces) |
| `polaris` | `#F3C46B` | the accent: primary action, focus ring, logo star |
| `polaris-ink` | `#1D1505` | text on polaris |
| `cat-movies` / `cat-series` / `cat-music` / `cat-adult` / `cat-podcasts` | `#E8935A` / `#5FB8C9` / `#E07AA6` / `#D95B55` / `#A99CF2` | category dots and chips only |
| `ok` / `info` / `warn` / `bad` | `#5CC08A` / `#6AA8EA` / `#E3A54A` / `#E5645B` | states: available, in progress, needs attention, failed |

Chips: tinted background (`color-mix` 16 %), text 82 % toward white, leading dot in full color.

### Color from the cover (A1)

More color comes into the member area only from the title being shown, and only behind it. Polaris stays the only accent.

- **Ambient layer** (Discover hero, movie/series detail overlay, scene page): the current title's backdrop, scaled to cover the page, blurred (≈ 70 px), slightly saturated and dimmed, behind the content as a non-interactive `aria-hidden` layer; it cross-fades with `--dur-ambient` when the title changes (the blur is static, only opacity changes). Its opacity has a **hard cap in CSS** (not per image) so that `text`, `text-2` and `text-3` keep ≥ 4.5:1 against the darkest to lightest pixels it can produce over `night-0`, measured on sample images.
- **Flat tint** (album detail overlay, genre tiles): one color sampled from the cover in the browser (8×8 canvas from the proxied cover, averaged in OKLCH, kept in memory) fills the surface as `color-mix(in oklch, var(--tint) 22%, var(--night-1))`, the tint clamped to OKLCH lightness ≤ 0.30 and chroma ≤ 0.08. No gradient. A unit test covers the clamp and the contrast of all text tokens against the clamped extremes.
- **Image blend:** a backdrop may fade into the ground with one mask at its edge, and text on an image sits on an image-level scrim. These belong to the image, not to chrome, and are the only gradients allowed.
- **Never** on sidebar, header, tab bar, buttons, chips, dialog chrome or admin screens; the primary button never takes the cover color; category colors stay dots and chips.
- **Private:** only inside the unlocked private area; nothing sampled from an adult image is stored (no `localStorage`, no server field).
- **Fallback:** without an image the surface stays `night-0` / `night-1`, never a category color.

## Typography

| Role | Face | Settings |
|---|---|---|
| Display (titles, cover titles, wordmark) | Bricolage Grotesque (variable: wght, wdth, opsz) | 650–800, `wdth` 78–90, tight tracking |
| UI and body | Instrument Sans (variable: wght, wdth) | 400 / 500 / 600 / 700, 15 px base, line-height 1.55 |
| Meta and data (years, runtimes, sizes, IDs, table headers) | Geist Mono | 400 / 500, tabular numbers |

Scale (px): 12 · 13 · 15 · 18 · 21 · 28 · 40 · 52. All fonts are OFL and bundled via `@fontsource-variable/*` (no font host at runtime).

## Shape, space, motion

- Radius: 6 (controls), 10 (covers, cards), 16 (panels); pills for buttons, chips, search.
- Spacing on a 4 px grid; layout with flex or grid `gap`. Tailwind's spacing unit is set to 4 CSS px (not 0.25 rem, because the root font is 15 px), so `*-1` is 4 px and `*-11` is 44 px.
- Touch targets: every control reaches 44 × 44 px below 1024 px (`max-lg:h-11`/`max-lg:size-11`, built into `Button`, `Input` and the shared filters); small visuals (checkbox, switch, inline title links) keep their size and get a 44 px hit area through an `after:` pseudo-element.
- Rows: in list and table rows, thumbs, checkboxes, meta columns, chips and action groups are vertically centred against the row (`items-center` / `align-middle`); only multi-line text blocks stay top-aligned inside. Dialogs are bounded by the viewport (24 px margin): header and actions stay, `DialogBody` scrolls.
- Cover formats per category: movies and series 2:3, music and podcasts 1:1, adult 16:9.
- Focus and hover on covers: lift 3 px, scale 1.03, 2 px Polaris ring; `--dur-2`. Under reduced motion: ring only, no lift or scale.
- Icons: Lucide, one stroke width. No emoji.

### Motion (A2)

Motion explains where something came from and where it goes; it never carries information alone and never delays a task.

| Token | Value | Use |
|---|---|---|
| `--dur-1` | 120 ms | press, hover tint, chip changes |
| `--dur-2` | 200 ms | cover lift, focus ring, small reveals |
| `--dur-3` | 320 ms | preview card, sidebar, popovers, tabs, list insert/remove |
| `--dur-4` | 480 ms | detail overlay and scene page entrance, page transition |
| `--dur-ambient` | 1 200 ms | ambient layer cross-fade |
| `--dur-hero` | 7 s dwell, 600 ms cross-fade | hero auto-advance |
| `--ease-out` | `cubic-bezier(0.2, 0.8, 0.2, 1)` | entrances, reveals |
| `--ease-in-out` | `cubic-bezier(0.65, 0, 0.35, 1)` | moves, sidebar, cross-fades |
| `--ease-playful` | `cubic-bezier(0.34, 1.4, 0.64, 1)` | at most one element per view: the detail cover or scene image "settling", a request success tick |

- Animate `transform` and `opacity` only (one exception: the preview card's area grows out of the cover through `clip-path`, A3). Text blocks of an entrance stagger 40 ms per block, at most 8. Nothing loops except the hero. Exits are faster than entrances (≈ 0.7×).
- Page and overlay transitions use the View Transitions API through React Router's `viewTransition` where supported, else a plain fade. No animation library.
- **Reduced motion** (`prefers-reduced-motion: reduce`): no translate, scale, parallax, auto-advance or cross-fade zoom; cross-fades ≤ 120 ms opacity or none; the preview card appears without growing. CSS through Tailwind `motion-safe:`/`motion-reduce:`, script-driven motion through one `useReducedMotion()` hook.

## Patterns

### Swimlane cards: preview card plus info strip (A3)

- **No caption in lanes:** a lane shows covers only, no title or year below. The cover's link carries the accessible name "Title (Year), state"; the state stays visible without hover as a `StateChip` on the cover's lower corner (form + color): requested, downloading, available, partially available, with labels from the state table.
- **Preview card "Morph"** (pointer hover and keyboard focus): the cover itself turns into a landscape card. The card's area grows out of the cover's rectangle (`clip-path`, the one exception to "transform and opacity only"), the poster flies (FLIP) to land as a small poster over the backdrop's lower edge, the backdrop fades into the card's flat cover tint (A1; no edge, no divider, no text on the image), and the cover's colour glows softly behind the card (blurred, opacity hard-capped at 0.5, nothing on it). Width 380 px (340 px for albums, which show their square cover blurred instead of a backdrop); title, artist, rating · year · runtime or seasons · certification, up to three genres; then "Open in \<server\>" (Polaris) under a state line ("Available in Jellyfin") for an available title, Request (Polaris) for a requestable one, else the state chip (with a bar while downloading); plus a round "Details" button. A click on the poster, the backdrop or the title block opens the detail overlay (one target: interest in the image means "show me more"; pointer cursor, the title underlines while the pointer is on the image or the title block; Request, "Open in" and Details stay separate controls). Intent timing: 320 ms dwell; within 400 ms of a card closing (or while one is open) the next cover's card comes after 90 ms; keyboard focus opens it too and a keyboard move replaces it at once, its poster then carrying the focus ring. Opening `--dur-3` `--ease-out`, text blocks staggered 40 ms; closing runs back 0.7× as long (after an 80 ms grace on pointer leave); a scroll closes it at once, Esc animated. Kept 8 px inside the content area (first and last covers included), overlaps neighbours. `aria-hidden`, buttons out of the tab order, the actions stay in the info strip; not shown when `(hover: none)`. Reduced motion: a 120 ms cross-fade in place, nothing flies or grows.
- **Info strip** (keyboard and touch): a fixed strip under the lane shows the lane's current cover: 16:9 image, title (artist for albums), rating · year · runtime/seasons/track count · certification · genres, a two-line synopsis, Request (or the state chip, plus "Open in \<server\>" for an available title) and Details. It follows the roving focus while focus is in the lane, and the selected cover on touch layouts (first tap selects, a tap on the selected cover opens the overlay); its two actions are the next tab stops after the lane. Collapsed for pointer users without keyboard focus.
- **Gold:** the hero's Request is the view's primary action; Request (or "Open in \<server\>" for an available title) in the preview card or info strip is gold too, because only one of them exists at a time and it belongs to the focused context (principle 2).
- **Grids** ("See all", search, private scene grid) keep a caption under the cover: they are for scanning.
- **Missing image:** one fallback for every shape: `night-2` tile, category dot, title in the display face, line-clamped, full title in a tooltip.

### Hero "Bühne" (stage, A4)

- One full-bleed backdrop at a time, cross-fading with `--dur-hero`; the ambient layer follows the slide. Lower left, on an image scrim: rank line ("#3 Trending this week"), category chip, title in the display face, facts in Geist Mono (rating · year · runtime or seasons · certification · genres), a two-line synopsis, and the actions: Request (Polaris) when not requested; otherwise the **status plaque**: borderless, 44 px high like the buttons, a 36 px icon circle in the state's tone (while downloading a progress ring with the percentage) and two lines, the state from the state table and a detail the API provides ("in Jellyfin since Nov 3", "requested Nov 2", "62% downloaded"; one line when there is none), then a hairline before the buttons; when available or partly available (the hero then offers no request), **"Open in \<server\>" is the slide's one Polaris action**; plus a ghost "Details" that opens the detail overlay. All hero buttons are 44 px. At 390 px the action row wraps under the plaque.
- **Top-10 strip** below the copy: ten poster thumbnails as a tab list ("Choose title"), the current one `aria-current` with the dwell progress as a 3 px bar along its bottom edge; previous/next and pause/play beside it. At 390 px the strip scrolls horizontally and the copy sits below the image.
- Discover shows the mixed top 10 of movies and series the member may see (never music, it has no backdrops); category pages show the category's own top 10.
- Size 62 vh at ≥ 1024 px (max 620 px), 16:9 at 390 px. Backdrops `w1280` (≥ 1024 px) or `w780`, thumbnails `w154`, through the image proxy; the first slide eager, the rest after idle. Text contrast measured for 4.5:1 on the actual pixels of sample slides.

### Carousels (A5: the hero and any later carousel)

- Auto-advance every 7 s; pauses while hovered, while focus is inside, while the tab is hidden and after the user pressed pause; any manual navigation (thumbnail, previous/next, arrow keys) resets the dwell.
- Visible pause/play button with a changing label ("Pause carousel" / "Play carousel"), previous/next buttons, the thumbnail tab list.
- `aria-roledescription="carousel"` on a labelled region ("Top 10 this week"); each slide a `group` with `aria-roledescription="slide"` and the label "3 of 10: Title"; inactive slides `inert`; live region `off` while auto-advancing, `polite` after user navigation.
- Keyboard: Tab reaches the slide's actions and the controls; Left/Right move slides while focus is in the carousel. Touch: native scroll snap, no scroll hijacking.
- Reduced motion: no auto-advance (a quiet "Autoplay off" next to the controls), instant switch instead of the cross-fade.

### People and organizations (A6)

| Entity | Shape | Content |
|---|---|---|
| Performer (private), cast member (detail), artist (music) | round, 1:1 | portrait or monogram on `night-2`; name below (people are recognised by name); "Following" chip when followed |
| Studio (private) | 16:9 logo tile on `night-1`, logo contained (not cropped), radius 10 | name below; "Following" chip |
| Watch provider | 1:1 rounded square, radius 10, provider logo | provider switcher, detail facts; JustWatch credit where shown (TMDB terms) |
| Genre tile (category pages) | 16:9 tile, radius 10, flat tint from a representative cover with the cover art cropped at one edge | genre name in the display face |

Covers keep their category formats. These shapes never take a category color.

### Detail overlay (A7)

- One `DetailView` for movie, series, album and artist, opened by `?title=<kind>-<id>` as a large modal dialog (max 1120 px wide, near full height, radius 16) over the current page; full-screen sheet with a sticky close header below 1024 px. Back or Esc closes it (top layer only) and returns focus to the opening poster (the page's first heading for a deep link). The page below keeps its scroll and lane positions. An unknown `?title=` shows "not found" inside the overlay at once, no retries on 404.
- Header: backdrop with image scrim and the ambient layer behind the dialog; the cover overlapping the header edge and settling with `--ease-playful`; title, tagline, facts row in Geist Mono (certification badge, runtime or seasons, release date, rating with vote count, genres); beside it the request card with one Polaris action (Request / Request n seasons / Open in …), the journey inline (no popover), "Withdraw request" while allowed, then a ghost trailer link; one "Notify me about new seasons" switch (hidden for ended series).
- Tabs (`role="tablist"`, arrow keys, first tab focused on open): **Overview** (synopsis; where to watch: stream/rent/buy provider tiles for the region with JustWatch credit), **Seasons** (series: state per season, an "Episodes" disclosure per season; gold only after a season is selected, "Everything is available" otherwise; the list stays mounted after requesting), **Tracks** (album), **Cast** (top 12, round, not clickable; directors/creators above), **Details** (original title and language, release and digital release dates, writers, studio, networks, external IDs).
- **Album:** the whole dialog surface takes the flat cover tint instead of a backdrop; the tracklist is the first visible content under the header, also on Overview (disc headings, position, title, track artist when it differs, length in Geist Mono, total running time; "Tracklist of the 1999 release" when the group has several releases). Facts: artist (link to the artist view), release date, label, release type, length. **Artist:** round monogram, genres, discography grid with state, follow switch. Wherever MusicBrainz genres are listed, the line "Genres from MusicBrainz (CC BY-NC-SA)" links to the licence.
- Entrance: shared-element cover transition from the clicked poster where view transitions are supported, else a `--dur-4` fade-and-rise; text blocks stagger. Reduced motion: none.
- The private scene page (its own route) shares the request card, journey and entrance motion.

### Member requests

- `/requests` is grouped by phase (On its way · Waiting for approval · Ready to watch · Closed, the last collapsed) with jump links and counts on top; a journey bar while on its way, else the state chip with a sentence in member words; actions always in one fixed right column; rows are of equal rank, so no gold ("Open in" is neutral there). Filters: type and "Show hidden" only.
- Uniform rows: one 48 × 72 px thumb slot in every row and section (music 48 × 48 centred in it), the state always as chip over one detail line (the journey bar on its way), the actions in one fixed column in the same quiet style with icon and label, and Hide as an icon button with tooltip in a rightmost slot reserved in every row. "Following" uses the same grid.

## Components

shadcn/ui as the base (copied into `web/src/components/ui/`, restyled to these tokens), Radix primitives underneath (Dialog, AlertDialog, Tooltip, Tabs, ToggleGroup; not HoverCard, which is not keyboard-reachable). Starr components: `AppShell` (member and admin variants), `BottomTabBar`, `Poster`, `CoverCard` (cover + state chip, no caption), `Swimlane`, `PreviewCard`, `InfoStrip`, `Hero`, `EntityTile`, `GenreTile`, `CategoryDot`, `StateChip` (driven by the one state table in `web/src/lib/states.ts`), `DetailView`, `Tracklist`, `ProviderSwitcher`, `DataTable`, `ConfirmDialog`, `UndoToast`, `PrivateGateNotice`.

## Never

- Gradients, glow or glass as chrome decoration.
- Color from a title outside that title's hero, overlay, scene page or tile.
- More than one Polaris action per view or focused context; gold on rows of equal rank.
- Category colors as large surfaces.
- Pure black, pure grey, Inter or system fallback as a design choice.
- Adult covers outside unlocked areas, not even blurred.
