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

---

## Milestone 1: done, 2026-08-07

`src/cusk` — ~380 lines against smithay 0.7.0, run nested in the KDE session.

```
INFO cusk: listening on cusk-1
INFO cusk: spawning alacritty
INFO cusk: mapped toplevel at (40, 40)
```

The client connected, its toplevel entered the `Space`, and the render loop ran
without a single error propagating — which covers `bind`, `render`, `clear`,
`draw_render_elements`, `finish` and `submit`. Closing the window exits cleanly.

What is real: socket allocation, display, seat and keyboard, xdg-shell mapping
and configure, cascade placement, focus-with-raise, frame callbacks, unmapping
with focus handover, and rendering positioned from `Space::element_location`.

Positions come from the `Space`, not from the toplevel list. That is the line
that makes tiling a change of policy rather than a rewrite.

### Understood limitation: clients get no GPU buffers

Running a client inside cusk produces:

```
libEGL warning: failed to get driver name for fd -1
libEGL warning: egl: failed to create dri2 screen
```

Attributed rather than assumed: cusk alone emits none, and the same client on
the host session emits none. They appear only *inside* cusk, because cusk does
not advertise `zwp_linux_dmabuf`. Clients therefore fall back to shared memory,
which works and is slow.

Not a bug, and not urgent — a nested development compositor rendering a
terminal has cycles to spare. It becomes urgent the moment anything is
benchmarked or runs on the tty backend, and the fix is exposing a DMABUF
allocator from the renderer and delegating the protocol.

### Next

Floating policy proper — pointer focus, click-to-focus, move and resize with
the pointer, and remembered geometry. That is §3's floating mode, and it is
also what §3 says tiling must be able to make exceptions to.

---

## Milestone 2: floating mode, 2026-08-07

`src/cusk/src/floating.rs` — pointer focus, click-to-raise, and interactive
move and resize.

| | |
|---|---|
| pointer motion | routed to the surface under it, with enter/leave |
| click | focus and raise, together |
| click on background | clears keyboard focus |
| **Super + left drag** | move |
| **Super + right drag** | resize, from the nearest corner |
| client titlebar drag | `move_request` / `resize_request`, same grabs |

Both are `PointerGrab` implementations rather than a mode flag checked in the
event loop. The difference shows the moment the pointer leaves the window being
dragged — a flag loses it exactly when the drag matters, whereas a grab keeps
receiving events until the button comes up.

### Things that are one line and would each be a bug report

- **Focus is forced to `None` during a grab.** Letting the pointer enter
  whatever it passes over mid-drag sends enter/leave storms to unrelated
  clients.
- **The grab ends on *its own* button.** Checking "any button released" would
  cancel a drag when a second button pressed mid-drag is let go.
- **Consumed bindings are not forwarded.** Without that, Super+drag also
  selects text in the terminal being dragged.
- **Resize clamps the size, then corrects the origin.** Clamping alone lets a
  left-edge drag keep moving the window after it has stopped shrinking.
- **Minimum size is 120×60.** A window dragged to zero cannot be grabbed
  again; the only recovery is killing the client.
- **Corners are tested before edges.** The diagonal regions overlap the
  straight ones, and the other order makes corners unreachable.
- **Physical → logical conversion is explicit.** At scale 1 the numbers are
  identical, which is precisely why it must be written down: it goes silently
  wrong on the first HiDPI output otherwise.

### Verified

Two clients map and cascade — `(40, 40)`, `(70, 70)` — and the compositor is
stable across connect and disconnect.

### The host compositor eats the bindings

First interactive test: clicking focused and raised correctly, and Super+drag
did nothing at all.

Not a grab bug. KDE's default `CommandAllKey` is Meta, bound to **Meta+LMB
move** and **Meta+RMB resize** — the same two gestures — so KWin consumes them
before the nested window ever sees the modifier. Any nested compositor
inherits its host's bindings, and Super is the modifier every desktop claims.

Super remains correct for a real session. `CUSK_MOD=alt` (also `ctrl`,
`ctrl-alt`) exists so the bindings can be exercised under a host that has
already taken it:

```bash
CUSK_MOD=alt cargo run
```

Button presses now log their modifier state at debug level, because "nothing
happened" is indistinguishable from a broken grab, and the difference is one
`RUST_LOG` away:

```bash
RUST_LOG=cusk=debug CUSK_MOD=alt cargo run
```

### Next

Remembered floating geometry, which §3 lists as a prerequisite for mode
switching: a window moved to a tiled workspace and back must return to where it
was, not to the origin. That is the last piece of floating that tiling depends
on.
