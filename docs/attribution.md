# Attribution (required for every copy and fork)

Sessionmux is licensed under the [Common Public Attribution License Version 1.0](../LICENSE)
(CPAL-1.0). Section 14 of that license, together with Exhibit B, requires every copy, modified
version and larger work that has a user interface to keep the Original Developer's attribution
visibly displayed. This is a **license condition, not a request**: removing, hiding or shrinking the
attribution is a breach of the license, and under section 8.1 the license ends if the breach is
not fixed within 30 days of becoming aware of it.

## Attribution Information (Exhibit B)

| Element | Value |
| --- | --- |
| Attribution Copyright Notice | Copyright (c) 2026 joyal-droid |
| Attribution Phrase | Powered by Sessionmux |
| Attribution URL | https://github.com/joyal-droid/sessionmux |
| Graphic Image | the Sessionmux mark (`assets/icon.ico`; drawn as SVG in the panel header in `web/panel.html`) |
| Larger Works | display is **required** in Larger Works as well |

## Where Sessionmux displays it

A build satisfies section 14 by keeping all three of these, in the form the original code uses:

1. **Panel header, on every screen.** The Sessionmux mark and the name "Sessionmux" at the top left
   of the panel (`.brand` in `web/panel.html`), visible on the Accounts, Sessions and Settings tabs.
   The panel opens on every normal launch.
2. **About card** (Settings, last card). The attribution block
   `Powered by Sessionmux · Copyright (c) 2026 joyal-droid · https://github.com/joyal-droid/sessionmux`, with the
   URL as a working link (`ATTRIBUTION` and `REPO_URL` in `web/panel.js`, rendered as
   `.about__attribution`).
3. **Floating bubble.** The Sessionmux mark in the bubble: the violet `#8A7CFF` ring around the
   active account's avatar and the violet "C" button when no account exists (`src/ui/bubble.rs`).

## Rules for forks and redistributions

- Keep the three elements above. You may restyle them to match your theme, but each must remain
  visible, legible (contrast of at least 4.5:1, see [design.md](design.md)) and of a size
  consistent with the surrounding UI.
- Do not change the Attribution Information. The Attribution URL points to this original
  repository, not to your fork.
- If you add your own name, logo or attribution, Sessionmux's attribution must be **no less
  prominent** than yours.
- The attribution may not exceed what Exhibit B lists (a copyright notice, a phrase of at most ten
  words, one graphic image and a URL), and you do not have to display more than that.
- Keep the CPAL-1.0 header comment at the top of every source file, and keep `LICENSE` (installed
  as `LICENSE.txt`) with every distribution. You may add yourself as a contributor in the header.
- Modified files remain under CPAL-1.0, and their source must be made available to everyone you
  distribute the software to, or who uses it over a network (sections 3 and 15).
- Keep the Anthropic disclaimer (`brand::DISCLAIMER`) wherever Claude is mentioned.

## If a fork removes the attribution

The obligations of section 14 apply to every copy, modification and larger work, however it was
produced: edited by hand, generated or rewritten with an assistant or other tool, or translated
into another language. A derivative that leaves out the attribution breaches section 14.

If you find such a fork or build, the owner's steps are:

1. **Keep the evidence first:** screenshots of the missing attribution in the running build,
   permalinks to the fork's commits and files (press `y` on a GitHub file page for a link to the
   exact commit), and the download page or release where the build is offered, with dates.
2. **Ask politely:** open an issue on the fork that links to this page, names the elements that
   are missing, and asks for them to be restored. Most removals are oversights and are fixed
   this way.
3. **If nothing changes** within 30 days of your issue (the cure period of section 8.1), report
   the fork through GitHub's DMCA takedown form (<https://github.com/contact/dmca>), citing
   CPAL-1.0 section 14 and Exhibit B and attaching the evidence. GitHub publishes DMCA notices in
   its public `github/dmca` repository, so include only contact details you are willing to make
   public. Builds hosted elsewhere go to that host's copyright contact in the same way.

Anyone can point out a missing attribution in an issue on this repository; please include a link
to the fork or build.

## Checking a build

Run the smoke check (`Sessionmux.exe --smoke`, see [CONTRIBUTING.md](../CONTRIBUTING.md#smoke-check))
and confirm in the captures:

- `smoke\panel-accounts.png`, `panel-sessions.png`, `panel-settings.png`: mark and name in the header;
- `smoke\panel-about.png`: the full attribution line in the About card;
- `smoke\bubble.png` and `smoke\bubble-tray.png`: the mark in the bubble.

Pull requests that remove or weaken any of these are not accepted.
