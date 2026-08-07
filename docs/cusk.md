# Cusk — architecture of record

The HadalOS window manager. Floating and dynamic tiling in one compositor,
configured with KDE's discoverability and Hyprland's reach.

## 0. Decisions

| Area | Decision | Date |
|---|---|---|
| Display protocol | **Wayland**, not X11 | 2026-08-07 |
| Compositor toolkit | `smithay` (Rust) | 2026-08-07 |
| Legacy apps | XWayland, rootless | 2026-08-07 |
| Dev backend | winit — nested in the running session | 2026-08-07 |
| Config source of truth | a text file, typed schema | 2026-08-07 |
| GUI config | an editor over that file, comment-preserving | 2026-08-07 |

## 1. This reverses ARCHITECTURE.md §0

That section chose XLibre and `x11rb`, and was explicit about why:

> the choice is not "legacy X vs. modern Wayland." It is "the maintained X
> server, on which writing a compliant window manager is a tractable project"
> vs. "writing a Wayland compositor," which is a categorically larger job.

**The X11 half of that still stands.** XLibre is maintained, an X11 WM is
genuinely smaller, and nothing about Wayland makes that reasoning wrong.

What has changed is the second half. "Writing a Wayland compositor" no longer
means implementing the protocol. `smithay` provides the plumbing — surfaces,
seats, outputs, xdg-shell, layer-shell, XWayland glue — and two shipping
desktops are built on it: COSMIC (Epoch 1.0.12, May 2026) and niri (25.11).
The job went from "implement a display server" to "implement window
management on top of one", which is the same job the X11 plan had.

It is still the larger option. It is no longer *categorically* larger, and
that word was carrying the decision.

**§0 needs amending upstream**, not quietly contradicting. The rows for
`X server`, `WM core` and the layer map in §1 all describe a plan that is no
longer the plan.

## 2. What Wayland actually costs

Stated up front, because these are the things that surprise people who have
written X11 window managers:

- **You own every frame.** There is no server compositing for you. Damage
  tracking, buffer age, frame callbacks and vsync are yours.
- **Input is yours.** libinput gives events; pointer constraints, focus
  policy, key repeat and xkb state are the compositor's problem.
- **Privileged things need portals.** Screen capture, screen sharing, global
  hotkeys for other apps, and clipboard access from unfocused clients are all
  *deliberately* unavailable to ordinary clients. `xdg-desktop-portal` plus a
  backend is not optional if the desktop is to be usable.
- **XWayland is a rootless server you host.** Legacy apps work, but their
  window management is a second code path with different semantics.
- **A crash takes the session.** On X11 a WM crash loses window decoration; on
  Wayland it loses every client. This raises the bar on error handling from
  "should" to "must".

None of these are reasons not to do it. They are the reasons the X11 estimate
was smaller, and pretending otherwise now would repeat the boot layer's
mistake of discovering the hard parts by hitting them.

## 3. Two modes, one compositor

Not two window managers behind a switch. One layout engine with two policies
over the same window set, so a window does not change identity when the mode
does.

- **Floating** — position and size are the window's own. The compositor
  places new windows and otherwise stays out of the way.
- **Dynamic tiling** — position and size are computed from a layout over the
  workspace. Windows have an order, not coordinates.

The interesting cases are the seams, and they should be decided now rather
than emerged:

- A floating window in a tiled workspace (dialogs, pickers) — tiling must have
  a floating exception, or every file chooser becomes a tile.
- Switching a workspace from tiled to floating — tiled windows need remembered
  floating geometry, or they all pile at the origin.
- Fullscreen and maximise are neither mode and must not be modelled as either.

## 4. The config, which is the actually hard part

"KDE's friendliness with Hyprland's power" is not two feature sets to add. It
is one requirement: **a GUI and a text file editing the same thing, without
fighting.**

The usual failure is well known. The GUI rewrites the file and drops comments
and ordering, so anyone who hand-edits stops using the GUI; or the GUI keeps
its own store, so hand edits are invisible and get clobbered. KDE mostly
avoids this with kconfig. Hyprland avoids it by having no GUI.

The design:

- **One typed schema.** Every setting has a type, a default, a range and a
  description. The GUI is generated from it; the parser validates against it;
  documentation is generated from it. There is no second list to keep in sync.
- **The text file is the source of truth.** It is what ships in dotfiles, what
  goes in git, what a user diffs. Not a cache of a database.
- **The GUI is a round-tripping editor.** It parses to a syntax tree that
  keeps comments, blank lines and ordering, edits the node, and writes back.
  A GUI that eats your comments is a GUI nobody uses twice.
- **Hot reload on change**, whichever editor made it, with validation errors
  surfaced rather than silently reverting to defaults.

### The part that pays for itself

A typed schema means Hadal can propose configuration changes **as typed
actions**, exactly as `write-config` already does for Portage. Not "here is
some text to paste" — a validated `SetSetting { key, value }` that the broker
range-checks and the user confirms, with the same summary-from-parsed-action
guarantee as everything else.

"Make my terminal open on workspace 3" becomes a proposal, not a suggestion.
That is the first thing in this project where the WM and the assistant are
better together than apart, and it falls out of the schema being typed rather
than being designed in.

## 5. First milestone

Deliberately small, and provable — the boot layer's lesson was that a thing
which has never run is not a thing that works.

> A nested compositor that opens a window in the current session, runs a real
> client (`foot`, `alacritty`) inside it, moves and focuses it, and exits
> cleanly.

No tiling, no config, no shell. That exercises the whole spine — smithay
setup, an event loop, seat and output, xdg-shell, rendering, input routing —
and it runs windowed inside the KDE session, so it costs nothing to fail.

Only then: floating policy, then tiling, then the schema, then the GUI.

## 6. Scope, honestly

This is larger than everything else in HadalOS combined. The broker is ~3k
lines of Rust and does one well-defined thing behind a protocol that already
exists. A compositor plus a shell is a desktop environment, and the README
currently marks both `hadalwm` and `HadalOS Shell` as *not started*.

That is not an argument against it. It is an argument for the milestone in §5
being the next thing, rather than a schema, a mode-switching design, or a
GUI — none of which can be validated until something is on screen.
