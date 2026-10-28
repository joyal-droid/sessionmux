# Design

The visual rules for the panel (`web/`) and the floating bubble (`src/ui/bubble/`), the same on
Windows and macOS. The look is
called "Dark Glass": a dark, slightly translucent card with flat colour fills.

## Principles

1. **Flat colours.** Fills, borders and text use solid colours or solid colours with alpha. No
   gradients anywhere (CSS `*-gradient()`, Direct2D or Core Graphics gradients).
2. **Tokens only.** Every colour, radius, spacing step, font size and duration in CSS comes from a
   custom property in `web/shared.css`. `panel.css` never hard-codes a colour. The bubble uses the
   same values as Rust constants that mirror the dark tokens.
3. **Contrast.** Text and meaningful icons reach at least **4.5:1** against their actual background
   in both themes, including text on `*-soft` fills (use the matching `*-text` token) and white text
   on filled buttons (`--accent-strong`). `--fg-subtle` (about 3.8:1) is for decorative or disabled
   content only.
4. **Both themes.** Every rule works in dark (default) and light. Check both before sending a UI
   change.
5. **Quiet motion.** Short transitions (`--dur-fast` 120 ms, `--dur` 180 ms, `--ease`), and none
   with `prefers-reduced-motion: reduce`.
6. **Account colour is identity.** An account's colour appears only on its avatar and its bubble;
   UI chrome uses the accent.

## Themes

| Setting | Result |
| --- | --- |
| Dark (default) | no `data-theme` attribute, or `<html data-theme="dark">` |
| Light | `<html data-theme="light">` (frosted light) |
| System | no attribute; resolves to light only when the OS prefers light |

The windows are transparent; `html`/`body` never paint a background. The panel card paints
`--bg` with a 20 px backdrop blur (with an opaque fallback where `backdrop-filter` is unsupported);
the native window behind it is `--bg-solid` so it never flashes white.

## Colour tokens

| Token | Dark | Light | Use |
| --- | --- | --- | --- |
| `--bg` | `rgba(16,17,20,.94)` | `rgba(246,246,248,.94)` | panel card |
| `--bg-solid` | `#101114` | `#f6f6f8` | opaque base, focus-ring gap |
| `--bg-float` | `rgba(29,30,35,.97)` | `rgba(255,255,255,.96)` | menus, dialogs, toasts |
| `--bg-elev` / `--bg-elev-2` | white 4.5 % / 7.5 % | white 65 % / 90 % | cards / nested cards |
| `--bg-sunken` | white 3.5 % | black 4 % | inputs, segmented tracks |
| `--bg-hover` / `--bg-active` | white 7 % / 10 % | black 5 % / 8 % | interactive states |
| `--fg` | `#ecedef` | `#141416` | primary text |
| `--fg-muted` | `#9a9da6` | `#5f5f66` | secondary text, disclaimer, attribution |
| `--fg-subtle` | `#737784` | `#75757e` | decorative and disabled only |
| `--border` / `--border-strong` | white 8 % / 14 % | black 8 % / 16 % | hairlines |
| `--accent` | `#8a7cff` | `#6d5df5` | brand, focus, selected tab |
| `--accent-strong` | `#6d5df5` | `#6d5df5` | filled primary buttons (white text) |
| `--accent-soft` / `--accent-text` | accent 16 % / `#aba1ff` | accent 12 % / `#5646d9` | notices, badges |
| `--success`, `--success-text` | `#34d399` | `#0e9f6e`, `#0a6e4c` | running state |
| `--warn`, `--warn-text` | `#fbbf24` | `#b7791f`, `#7a5000` | sign-in needed |
| `--danger` | `#f87171` | `#dc2626` | destructive actions |
| `--on-color` | `#fff` | `#fff` | initials on avatars, toggle knob |
| `--on-color-shadow`, `--ring-avatar`, `--inset-hairline`, `--shadow-knob` | fixed | fixed | legibility marks on colour fills |
| `--shadow-sm` / `--shadow-md` | black 25 % | black 12 % / 14 % | selected pills, inverted buttons |

Account palette (assigned in order to new accounts): `#F08A5D`, `#5B9CF6`, `#3DD6A1`, `#B388FF`,
`#F6C453`, `#F2709C`, `#4FD1DB`, `#9BA3B5`. Avatar initials are white with a 1 px dark shadow so
they stay legible on every palette colour.

## Shape, spacing and type

| Token | Value | Use |
| --- | --- | --- |
| `--radius-xl` | 20 px | the panel |
| `--radius` | 16 px | cards |
| `--radius-sm` | 12 px | buttons, inputs, icon buttons |
| `--radius-xs` | 8 px | small chips |
| `--radius-pill` | 999 px | pills, badges, the mini tray |
| `--s-1` … `--s-6` | 4, 8, 12, 16, 24, 32 px | spacing on an 8 px grid with a 4 px half step |
| `--font` | Segoe UI Variable Display / Segoe UI (the system font on macOS) | all UI text, weight 400 (500 or 600 for a few emphasised labels) |
| `--font-mono` | Cascadia Code / Consolas (the system monospace font on macOS) | paths |
| `--fs-xs` … `--fs-lg` | 11, 12, 13, 15 px | text sizes (13 px body) |

The panel is 380 × 580 DIP: a header with the Sessionmux mark and name and three tabs (Accounts,
Sessions, Settings), then a scrolling view. Focus is always visible (`--focus-ring`: a 2 px gap in
`--bg-solid` and a 2 px accent ring).

## Bubble

All sizes are DIP (points on macOS) and scale with the monitor DPI (the display's backing scale
on macOS). The macOS bubble draws its text with the system font at the same sizes.

- **Icon mode** (80 × 80 window): a 52 px circle in the active account's colour with its initials
  (15 px), a 2.5 px `#8A7CFF` ring, a soft shadow made of three concentric translucent rings, and a
  10 px running dot (`#34D399`, or `#FBBF24` when the account still needs to sign in) with a 2 px
  `#101114` ring. With no accounts, it is a `#8A7CFF` circle with a white "C".
- **Mini tray** (72 px high): a `#101114` pill at 94 % opacity with a 1 px white 14 % border, a
  14 px grip (2 × 3 dots), up to three 36 px avatars in 44 px slots 6 px apart (12 px initials; the
  active one has a 2.5 px accent ring, the others a 2 px white 14 % ring; 8 px running dots), and a
  32 px round **⋯** button.
- Hovering an avatar scales it to 1.08. Dragging snaps to the nearest left or right screen edge
  with a 12 px margin.

## Attribution elements

The Sessionmux mark and name in the panel header, the attribution line in the About card (in
`--fg-muted`) and the bubble's mark are required by the license and must stay visible and legible
under these rules; see [attribution.md](attribution.md).
