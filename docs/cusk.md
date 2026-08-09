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
| Layout engine | pure function of area and count, no Wayland types | 2026-08-07 |
| Tile order | its own list, never stacking order | 2026-08-07 |
| Tiled drag | reorders; it does not move the window | 2026-08-07 |
| Per-window state | the window's `UserDataMap`, not a side table | 2026-08-07 |
| Config format | TOML, edited as a syntax tree (`toml_edit`) | 2026-08-07 |
| Schema | one macro declaration; struct and table cannot drift | 2026-08-07 |
| Bad values | rejected and reported, never clamped or defaulted | 2026-08-07 |
| Config watching | poll the path; inotify dies on rename-based saves | 2026-08-07 |
| Settings GUI | iced, its own crate; controls generated from the schema | 2026-08-07 |
| GUI ↔ compositor | the config file, via hot reload — no IPC, no apply button | 2026-08-07 |
| Visual language | sampled from niri/KaOS: purple slate, periwinkle, no borders | 2026-08-07 |
| Blur | CPU, on a static wallpaper, computed once — not a per-frame shader | 2026-08-07 |
| Backdrop cache | split: decode+scale keyed apart from blur radius | 2026-08-07 |
| Rounded corners | painted back with the wallpaper, not clipped from the window | 2026-08-07 |
| Chrome shaders | degrade to square corners rather than refusing to start | 2026-08-07 |
| Palette | `cusk::theme`, shared by compositor and editor — never copied | 2026-08-07 |
| Workspaces | generic over the element type, so the logic is testable without Wayland | 2026-08-07 |
| Hidden windows | unmapped from the `Space`, never parked off-screen | 2026-08-07 |
| Palette source | HadalOS's own launcher icon: deep blue, cyan accent | 2026-08-07 |
| Launcher | a separate client, not part of the compositor | 2026-08-07 |
| Overlay windows | recognised by app id, classified on first commit not on create | 2026-08-07 |
| Cursor | drawn in code, not loaded from an XCursor theme | 2026-08-07 |
| dmabuf | v4 with feedback — v3 alone leaves clients unable to find a GPU | 2026-08-07 |
| Window blur | scene built back to front offscreen; each window blurs what precedes it | 2026-08-07 |
| Panel | drawn by the compositor; iced cannot speak layer-shell | 2026-08-08 |
| Reserved space | one `usable_area`, read by tiling, placement and maximise | 2026-08-08 |
| Text | `fontdue`, no shaping; a system font, never bundled | 2026-08-08 |
| tty backend | built in phases; phase one probes and never takes DRM master | 2026-08-08 |
| Session control | exclusive per session — the tty backend is only testable from its own VT | 2026-08-08 |

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

`CUSK_MOD=alt` was not enough either: KDE's defaults claim Alt as well, and the
symptom was unmistakable once described — *the Smithay window* moved and
resized, the alacritty window inside it did not. KWin was acting on cusk's own
window the whole time.

**Every modifier+drag a nested compositor binds, the host gets first.** There
is no modifier that is reliably free, and chasing one is the wrong fix.

### Test the grabs without a modifier at all

Client-side decorations sidestep the problem completely. Dragging a client's
own titlebar sends `xdg_toplevel.move`, which arrives at
`XdgShellHandler::move_request` and runs the *same* `MoveGrab`. The host cannot
intercept it: the click lands on a surface inside cusk's window, and the client
asks cusk to move itself.

Alacritty draws CSD by default on Wayland, so dragging its titlebar is a
complete test of the move path with no modifier involved. That is the
authoritative check; modifier bindings are a separate concern that only becomes
testable on the tty backend, where there is no host to lose to.

`start_move` and `start_resize` log at info, so the terminal running cusk says
whether a gesture was recognised:

```
INFO cusk: move grab started at (40, 40)
```

No line means the gesture never arrived. A line with no movement means the grab
is wrong. Those are unrelated problems and were previously indistinguishable.

### Next

Remembered floating geometry, which §3 lists as a prerequisite for mode
switching: a window moved to a tiled workspace and back must return to where it
was, not to the origin. That is the last piece of floating that tiling depends
on.

### Two bugs found by "the cursor doesn't change"

Dragging alacritty's titlebar did nothing either, and the reported detail that
mattered was that the cursor never changed shape over the titlebar or the
edges. If the client were receiving pointer events at all, its own decorations
would have reacted.

**Hit-testing stopped at the root surface.** `surface_under` returned
`window.toplevel().wl_surface()`. Client decorations are *subsurfaces*, so the
titlebar never received a click, never sent `xdg_toplevel.move`, and dragging
it did nothing. The window still looked focused, because the root surface was
getting the events. Fixed with `Window::surface_under(point,
WindowSurfaceType::ALL)`, which descends into subsurfaces and popups — popups
for the same reason: a menu that cannot be clicked is worse than one that
never opens.

Surface positions from that call are window-relative and must be made global
before the pointer sees them, or every client computes its local coordinates
from the wrong origin — hit-testing subtly wrong everywhere rather than
obviously wrong somewhere.

**`cursor_image` is a no-op.** Cusk never renders a cursor, so its appearance
could not have changed however correct pointer delivery was. That made it a
useless diagnostic, and it was offered as one. Cursor rendering needs theme
loading and a render element and is deferred — until it exists, cursor shape
says nothing about whether pointer routing works.

### The actual bug: `Window::on_commit` was never called

Instrumenting rather than guessing gave it immediately:

```
291 motions, 0 hit a surface, windows=1
loc=(40, 40)  bbox=0x0  geom=0x0
```

Pointer events were arriving fine. `Space::element_under` consults the
element's cached bounding box, that box was **0×0 for the window's entire
life**, and no point is inside a zero-size rectangle. So nothing ever focused,
nothing raised, and a client's own decorations never got the press that would
have sent `xdg_toplevel.move`.

`Window::on_commit()` recomputes that box from the surface tree, and it has to
be called from `CompositorHandler::commit`. It never was.

**Rendering concealed it completely.** `render_elements_from_surface_tree`
walks the surface tree directly and never consults the cached geometry, so the
window drew perfectly at the right size while being, as far as input was
concerned, zero pixels wide. Every visible signal said the compositor was
working.

After the fix the same report reads `bbox 888×723`.

Three wrong guesses preceded it — the host eating Super, then the host eating
Alt, then root-versus-subsurface hit-testing. The first two were real and the
third was a genuine bug, but none of them was *this*, and each was plausible
enough to spend a round on. The instrumentation took one round and cost less
than any of them.

### Milestone 2 verified, 2026-08-07

Interactive test, from the log:

```
press 0x110 at (405, 31) -> surface at (-4, -4), super=true alt=true
move grab started at (40, 40)
```

Hit-testing resolves, presses find surfaces, grabs start, and windows move.
Click-to-focus, raise, and both move paths work.

### Correction: KWin was never eating the modifier

The log shows `super=true` arriving at cusk. Super reached the compositor the
whole time.

The gestures did nothing because `surface_under` returned `None` for every
press, so the modifier branch sat inside an `if let` that never ran. The
diagnosis that KDE's `CommandAllKey` was consuming Meta was plausible, matched
the documented defaults, and was wrong — and KWin moving the *Smithay* window
simultaneously made it look confirmed. Two systems reacting to one gesture is
not the same as one system stealing it.

`CUSK_MOD` stays, because the host acting on cusk's own window at the same time
is still worth being able to avoid while testing nested. But it was never
required, and the entry above claiming otherwise is corrected here rather than
edited away.

### Surfaces can start outside the window

One press resolved to `surface at (-4, -4)` for a window at `(40, 40)`. That is
alacritty's decoration subsurface, which extends past the window geometry for
its shadow. Worth knowing before tiling: **a window's surfaces are not confined
to its geometry**, so a layout that computes rectangles from surface extents
rather than from `Window::geometry()` will leave gaps the size of every
client's shadow.

---

## Milestone 3: remembered floating geometry, 2026-08-07

`src/cusk/src/geometry.rs`. §3 lists this as a prerequisite for mode switching,
and maximise is its first consumer — §3 also says maximise is *neither* mode, so
it is modelled as a departure from floating rather than as a third mode.

State lives in the window's own `UserDataMap`, not a side table in the
compositor. A side table has to be pruned on unmap; when it is not, the symptom
is a slow leak and eventually a stale rectangle applied to an unrelated window.
Attached to the window, it dies with the window.

### The guard the module exists for

Geometry is recorded on **every** move and resize, because recording only on the
way into another mode loses the last drag. Recording unconditionally is worse:
maximising overwrites the rectangle it is supposed to return to, and "restore"
silently becomes "do nothing" — which reads as a broken keybinding, not as a
lost rectangle. So a window is explicitly marked `displaced`, and while
displaced its floating rectangle is frozen.

This was caught by writing a test named `a_round_trip_restores_exactly` whose
body asserted `assert_ne!` — the test contradicted its own name and encoded the
bug as though it were the spec.

- **0×0 is refused.** Before its first commit a window reports zero size;
  storing that restores it to invisibility later, which looks like the window
  vanishing rather than like a bad rectangle.
- **Nothing displaces without somewhere to return to**, or a window maximised
  before its first commit strands itself with the toggle unable to undo it.

### Verified

```
maximised, will restore to Some(Rectangle { x: 128, y: 84, width: 800, height: 635 })
restored to  Rectangle { x: 128, y: 84, width: 800, height: 635 }
```

Exact, and across an intervening tiling on/off cycle.

---

## Milestone 4: dynamic tiling, 2026-08-07

`src/cusk/src/layout.rs` and `src/cusk/src/tiling.rs`.

**The engine knows nothing about Wayland.** `arrange(area, n, gaps) ->
Vec<Rectangle>` is a pure function, which is what makes the layout testable
without a display, a client, or an event loop — 22 of the 32 tests are that.

Floating is deliberately **not** a variant of `Layout`. A floating window's
rectangle is its own, so floating is the absence of an arrangement rather than
an identity arrangement; a `Layout::Floating` would force every caller to ask
whether the returned rectangles mean anything.

| | |
|---|---|
| layouts | master-stack (adjustable ratio), columns |
| **Super + T** | tiling on / off |
| **Super + E** | cycle layout |
| **Super + H / L** | narrow / widen master |
| **Super + Space** | float this window out of the layout |
| **Super + Return** | open another terminal |
| **Super + J / K** | focus next / previous |
| **Super + Shift + J / K** | move earlier / later in the layout |
| **Super + Shift + P** | promote to master |

### The seams from §3, resolved

- **Floating exception** — a toplevel with a parent is a dialog and is exempted
  from the protocol, rather than waiting for someone to notice a file chooser
  has become a tile.
- **Tiled → floating** — restores through milestone 3, which is what that
  milestone was for.
- **Fullscreen and maximise are neither mode** — carried by the `displaced`
  flag, not by a mode enum.

### Decisions made while building

- **Tile order is its own `Vec<Window>`, not `space.elements()`.** That is
  stacking order and changes on raise; tiling off it means clicking a window
  reshuffles the layout under the pointer and the window jumps away as you
  click it.
- **Focus cycles that same order.** Stacking order is a most-recently-used
  list, so cycling it walks between the same two windows instead of touring
  them all.
- **The same gesture means different things per policy.** A tiled window has no
  position of its own, so dragging it *reorders* and dragging its edge moves
  the *divider*. A free-moving drag would leave the layout and the screen
  disagreeing until the next relayout snapped it back with no explanation.
- **Swap commits on release; the divider updates live.** Opposite choices on
  purpose: swapping on hover makes the layout churn under the drag, while a
  divider you cannot see land is guesswork.
- **Promote swaps rather than reinserts**, so pressing it twice returns to the
  previous arrangement. A window manager has no undo.
- **Wrapping, not clamping**, for focus and reordering. Clamping makes the ends
  dead, and a key that stops responding is indistinguishable from one that was
  never bound.
- **`relayout()` recomputes unconditionally** rather than tracking dirtiness. A
  missed invalidation shows up as a window that silently stops participating,
  which is far harder to see than a redundant recompute.

### Two tests that lied to each other

`tiles_never_overlap` ran on 1920×1080; `tiles_never_collapse_below_the_minimum`
ran on 640×480. Each passed. Checked on the *same* inputs, they failed at once:

```
n=7: Rectangle { x: 386, y: 448, width: 246, height: 80 } escapes the screen
```

Clamping each tile up to a minimum while advancing the offset by the clamped
size overflows the column. Tiles shrink instead, and the minimum now applies
only to the master column, where it genuinely prevents starving a side. Visibly
cramped is honest; silently stacked is not.

The lesson generalises past this file: **two properties tested on different
inputs can both pass while the conjunction fails.** Same shape as the boot
layer, where each component was correct and the composition was not.

### A bug the guard caught before it shipped

A tiled window is already `displaced`, so without a guard `Super+M` took the
*restore* branch and popped it back to its floating rectangle while tiling
stayed on — one window loose over a layout that still believed it owned that
tile. Maximise is now a no-op on tiled windows.

### "I can't open multiple windows" was not a compositor bug

Reproduced rather than guessed: two clients connected and mapped cleanly. The
compositor was fine; there was simply no way to *open* a window from inside
cusk, so every new one meant returning to another terminal. `Super+Return`
fixed it, and spawned children are reaped on a thread — a compositor that never
waits accumulates a zombie per closed window, and a filling process table looks
like anything except a window manager bug.

That is the second time on this project that reproducing beat theorising, after
the three wrong guesses in milestone 2.

### Verified

Interactive, 2026-08-07: four windows via `Super+Return`, master-stack and
columns both correct, drag-to-swap, divider drag, keyboard focus and reorder,
promote, and the floating round-trip. 32 tests pass.

### Known rough edges

- **No drag feedback.** A swap gives no hint of its target until release.
- **Reorder bindings are live in floating mode**, where they change an order
  nothing currently reads. Harmless, and takes effect on tiling — but a
  keypress with no visible result reads as a dead binding.
- **Still no cursor**, and still no `zwp_linux_dmabuf` — both carried from
  milestone 1.

### Next

§4, the config schema — called "the actually hard part" for good reason.
Everything above hardcodes gaps, ratios, layouts and every binding, and each of
those is a setting the schema will have to claw back. Doing it now costs less
than doing it after a shell and a GUI also depend on them.

---

## Milestone 5: the typed schema and hot reload, 2026-08-07

`src/cusk/src/config.rs`. §4 minus the GUI.

**The macro is the design.** §4's constraint is "there is no second list to keep
in sync", and declaring settings twice — struct fields plus a descriptor table —
decays immediately into a key the parser accepts and nothing reads, or a field
the GUI cannot see. One declaration generates the `Config` struct, its
defaults, `SCHEMA`, and `get`/`set` by key. That last pair is the surface §4 has
Hadal propose `SetSetting { key, value }` through, range-checked before anything
is written.

TOML via `toml_edit`, because §4 requires a round-tripping editor and that is a
syntax-tree editor built for the job. A bespoke Hyprland-style format would
mean writing that tree by hand, which is the actual work — the syntax is not.

| decision | why |
|---|---|
| reject, never clamp | a clamped value means the file says one thing and the compositor does another, with nothing to say which |
| per-setting, not all-or-nothing | a compositor that will not start over one bad line leaves no desktop to fix it from |
| unknown keys reported | a silently ignored typo is the most common configuration complaint there is |
| the sample file is generated | a hand-written one is the second list this module exists to avoid, and goes stale on the first default change |

### When a setting takes effect is part of the schema

`Apply::Live` or `Apply::Restart`, declared per setting. The alternative is the
worst thing hot reload can do: the user edits a value, nothing happens, and
nothing says why. `layout.default` and `layout.tile-by-default` describe
*initial* state — reapplying them live would overrule a layout chosen since
with Super+E and make the keybinding feel broken. Changes to them are named in
the log rather than silently skipped, and the generated file says so too.

### Polling, not inotify

Editors overwhelmingly save by writing a temporary file and renaming it over
the target. That replaces the inode and silently kills a watch registered on
the old one — the reload works exactly once and then never again, which is
worse than not having it. Watching the directory instead means handling every
unrelated event in it. Re-stating one path every 500ms costs nothing and cannot
be fooled by a rename.

The fingerprint is mtime *and* length *and* inode: mtime has one-second
granularity on some filesystems, and two saves inside one second is precisely
what iterating on a setting looks like.

### A broken file must not reset the desktop

The most important behaviour in the module. Editors flush partial saves, so a
syntax error is a normal intermediate state, not a user decision. Falling back
to defaults would tear the session apart mid-edit — and from inside a
compositor that just lost its configuration there is no comfortable way back.
`Reload::Failed` keeps what is running. A file that vanishes is treated the
same way, since that is the window a rename-based save passes through.

### Verified live

```
INFO  reloaded cusk.toml
INFO  layout.default changed; takes effect on restart
WARN  cusk.toml: TOML parse error at line 2, column 13 — keeping the running configuration
```

Live settings applied, the restart-only change named, the half-written save
survived. Earlier, from a real file: `mod-key = "alt"` took effect,
`layout.master-ratio: allowed: 0.1 to 0.9` was reported and fell back while
every other setting still applied, and `layout.outer-gpa: no such setting`
caught the typo.

### Two things this caught in itself

`set_in_document` **ate trailing comments.** Assigning a fresh value discards
the node's decor, which is where `toml_edit` keeps `# tight` beside
`inner-gap = 4`. Writing the setting silently destroyed the note — precisely
the failure §4 names as making a GUI unusable. The old decor is carried onto
the new value now.

`focus-follows-mouse` **was declared and not implemented.** It parsed,
validated, documented itself and did nothing; the dead-code warning caught it.
Now implemented, with two guards: an empty hit does not unfocus, or crossing
the gap between two tiles would stop your typing mid-sentence; and an
already-focused window is skipped, since `focus` raises and raising on every
motion event re-stacks the space hundreds of times a second.

`TERMINALS` is gone — the launcher reads the schema's choice list, so a name
the file accepts cannot be one nothing will start.

### Next

The settings GUI, which is the half of §4 that is actually hard.
`set_in_document` and `Config::get` are its foundation: tested, and currently
with no in-compositor caller. The schema already carries everything a generated
UI needs — type, default, range, description, and when the change takes effect.

---

## Milestone 6: the settings editor, 2026-08-07

`src/cusk-settings` — iced 0.14, a separate crate so the compositor does not
link a GUI toolkit.

**It has no model of its own.** Every control is generated from
`cusk::config::SCHEMA` — slider for `Int`/`Float` at the declared range,
toggler for `Bool`, picker for `Choice`, description from `doc`, and a
"Takes effect on restart" line straight from `Apply`. There is no widget list
to keep in step, so a setting added to the compositor appears here with no
work. That required `cusk` to grow a lib target: an editor validating against
its *own copy* of the ranges would be the two-lists failure §4 exists to
prevent, one process further out.

**Writing the file is the entire mechanism.** No apply button, no IPC. cusk is
already watching, so a change reaches the running compositor within half a
second by exactly the path a hand edit takes. The editor watches the file too,
because a GUI that goes stale the moment you touch the file in an editor is one
you stop trusting — which is the whole of "without fighting".

- `Watcher::resync` exists because a program that writes the file it watches
  otherwise reads back its own save as an external edit and rebuilds state on
  top of whatever the user is dragging.
- Sliders write on release; toggles and pickers write immediately. Committing
  every pixel of a drag would hammer the disk and relayout the compositor
  dozens of times per gesture.

### The aesthetic, sampled rather than guessed

Reference screenshots supplied 2026-08-07. Two were niri, two were KaOS — a
different shell, but the dark one shares a language, and both were kept as
references.

| sampled | |
|---|---|
| KaOS panel | `#303243` |
| KaOS deepest surface | `#1D1D2D` |
| KaOS accent | `#8189B9` |
| niri accent | `#A3C9FD` |
| niri panels | vary from `#222226` to `#483040` |

That last row is the important one. The panels vary that much because they are
**translucent over the wallpaper**, which is the defining feature of the look
and the one thing a GUI toolkit cannot supply.

What was adopted: a desaturated blue-purple slate rather than blue-black, a
periwinkle accent between the two references, **no borders anywhere** —
surfaces separate by fill lightness alone, and a hairline around a rounded card
is the difference between a shell and a dialog box — larger radii, and low
contrast on secondary text. Raising that contrast "for legibility" is the
single change that would most make this stop looking like what it copies.

Structure follows the references too: a top tab row with a thin accent
underline, not a sidebar. Both mark the active section with one line and
nothing else.

Everything visual is in `style.rs`, because the compositor's own chrome is
meant to adopt the same tokens and a palette scattered through view code cannot
be adopted by anything.

### What the GUI cannot do: blur

The translucency-and-blur that defines the niri screenshot is **compositor
work, not application work**. A Wayland client cannot sample what is behind its
own window; the compositor has to blur the background and composite the window
over it. niri implements exactly this — `niri/src/render_helpers/blur.rs` is
visible in the reference screenshot's own file listing.

So it is a cusk milestone, not a settings-app one, and it is deliberately not
faked here: a transparent window without compositor blur is not the look, it is
just a window you can see through.

### Next

Either compositor-side blur and window chrome — focus rings, rounded corners,
borders, adopting `style.rs`'s tokens — or the workspace model. Blur is the
larger and more visible of the two, and it is what makes the rest of the
aesthetic land.

---

## Milestone 7: wallpaper and blur, 2026-08-07

`src/cusk/src/wallpaper.rs`, plus a restructured render loop.

**Blur needed something to blur.** cusk cleared to a flat colour, and a blurred
flat colour is the same flat colour — the wallpaper is not a companion feature
here, it is what makes blur visible at all.

### Software blur, on purpose

The textbook implementation is dual-Kawase in a fragment shader, ping-ponging
framebuffers every frame. Wrong here twice over: the wallpaper is **static**,
so re-blurring it sixty times a second produces nothing new; and cusk runs on
**llvmpipe** (`failed to create dri2 screen` in its own logs), where a
multi-pass per-frame blur would be the most expensive thing in the compositor.

Doing it in software also makes the blur an ordinary pure function over a byte
buffer — 13 tests, no GPU, no surface, no running compositor. The properties
worth having are `a_uniform_image_is_unchanged_by_blur` (catches off-by-one
windows, bad edges and integer truncation in one assertion) and
`edges_do_not_darken` (clamping, not transparent-black, or a wallpaper grows a
vignette nobody asked for).

The honest cost: what shows through a window is the blurred **wallpaper**, not
the blurred contents of a window behind it. Real per-frame blur needs the
shader path and is a later milestone.

### Measurement overturned the plan

The first version re-prepared everything whenever anything changed: 2102ms per
change, which would freeze the compositor for two seconds every time the blur
slider was released. Timing the stages, on a 1920x1080 source in debug:

| stage | |
|---|---|
| decode | 179ms |
| **resize (Lanczos3)** | **1278ms** |
| downscale | 190ms |
| blur r20 x3 | 160ms |

**The blur was never the expensive part.** The cache was split on the wrong
axis. Decoding and scaling depend on the path and the output size; the blur
depends on the radius and passes. Separating them, and switching Lanczos3 to
CatmullRom, took a blur-radius change from 2102ms to **702ms**, measured live:

```
wallpaper ready in 1832ms (1280x800, blur r40 x3)
reblurred in 702ms (r90 x3)
```

The blur itself is computed at half resolution and scaled back — blur is a
low-pass filter, so the detail dropped is detail it would have destroyed, at a
quarter of the pixels. That is the downsample step of a dual-Kawase shader,
applied on the CPU.

### A stacking bug found while reading the renderer

`draw_render_elements` does `render_elements.insert(0, element)`, so its input
must be **front-to-back**; smithay's own `space_render_elements` calls `.rev()`
before rendering, and `elements_for_output` is documented "back to front".
cusk passed `Space::elements()` — bottom-first — straight in, so **stacking
order was inverted whenever two windows overlapped**. Cascade placement and
tiling both avoid overlap, which is why nothing looked wrong.

Fixed by the restructure rather than by a `.rev()`: windows are now drawn one
at a time, back to front, each preceded by its own blur patch. A single
flattened list would have forced every patch to be drawn before every window,
putting an upper window's patch on top of a lower window.

### Also

- `Kind::Text` joins the schema, for the wallpaper path. Unvalidated beyond
  being a string, because a wallpaper that does not exist yet is a normal thing
  to have in a config you are still writing.
- New settings: `appearance.wallpaper`, `appearance.blur`,
  `appearance.blur-radius`, `appearance.blur-passes`, all live.
- `to_physical` is written out rather than assumed. At scale 1 it is the
  identity, which is exactly why it is spelled: it stops being so on the first
  HiDPI output.

### Next

Window chrome — rounded corners and focus rings, adopting the settings app's
`style.rs` tokens. That plus translucent clients is what makes the reference
look land; blur only shows through a window that is not opaque.

---

## Milestone 8: window chrome, 2026-08-07

`src/cusk/src/chrome.rs`, and `src/cusk/src/theme.rs` — the palette, now shared.

Two visual moves, drawn by different mechanisms because they are different
problems.

### Rounded corners are subtractive

A client draws a rectangle and the compositor cannot ask it not to. Clipping
the window's own texture would mean routing every surface through a custom
shader, which means reimplementing what `render_elements_from_surface_tree`
already does for subsurfaces and popups.

So the corners are **painted back over**: after a window is drawn, four small
quads of the sharp wallpaper go on top of its square corners, through a texture
shader that keeps only the sliver lying outside the corner arc. The window
appears rounded and what shows through is the desktop behind it — which is what
a rounded corner *is*.

This only works because `wallpaper::load_scaled` produces a texture at exactly
the output size: a screen rectangle is its own source crop, so the shader
recovers a pixel's screen position from its texture coordinate and there is no
second coordinate space to get wrong. The decision in milestone 7 to keep both
backdrop textures output-sized paid for itself here.

Four small patches rather than one window-sized quad. The shader is cheap, but
on a software rasteriser a full-window fragment pass per window per frame is a
real cost where four 12x12 patches is not.

### Focus rings are additive

Nothing needs removing, so the ring is one quad in the band *outside* the
window, with a signed-distance field picking out the area between two rounded
rectangles. It never covers window content — a focus ring that dims the edge of
the thing it is highlighting is worse than no ring. Its radius is the window's
plus the width it sits outside of, or the ring and the corner visibly drift
apart.

### Both degrade rather than fail

Shader compilation can fail on an old driver or a strict ES parser. Neither
program is load-bearing: a failure is reported once, left as `None`, and cusk
runs with square corners and no ring. A compositor that refuses to start
because it could not round a corner is a worse outcome than one that looks
plainer than intended. Both compile on llvmpipe.

### The stated limitation, stated out loud

Rounding paints the wallpaper back, so **with no wallpaper set the corners stay
square**. That is a surprising thing to discover in silence, so cusk says it
once:

```
INFO corner-radius needs appearance.wallpaper: corners are rounded by
     painting the wallpaper back over them
```

### The palette is now shared, not copied

`cusk::theme` holds the sampled tokens and both binaries read them: the editor
styles its widgets from them, the compositor draws its focus ring from them. A
private copy in each would drift, and the drift would show up as a focus ring
that does not match the accent in the settings window that sets it.

### The schema caught its own gap

Adding `Kind::Text` for the wallpaper path made the settings editor stop
compiling: `non-exhaustive patterns: Kind::Text { .. } not covered`. That is
the design working — a new setting type cannot be added to the compositor and
silently go unrendered in the GUI. Text fields commit on Enter rather than per
keystroke, or the compositor would try to load `/home/u`, `/home/us`,
`/home/use` and warn about each.

### Next

Translucent client support is what ties it together — blur only shows through a
window that is not opaque, so a terminal with `opacity = 0.8` is currently the
only way to see any of it. Beyond that: the workspace model, and per-frame blur
of actual window content rather than of the wallpaper.

---

## Milestone 9: workspaces, and the brand palette, 2026-08-07

`src/cusk/src/workspace.rs`. `Super+1..9` switches; `Super+Shift+1..9` sends the
focused window.

**Generic over the element type on purpose.** A `Window` cannot be constructed
without a live Wayland surface, so a `Workspaces<Window>` would be untestable —
every bug in switching, moving and removal would have to be found by clicking.
`Workspaces<u32>` in the tests runs exactly the code the compositor runs, which
is where 18 of the module's tests come from.

### What is per-workspace, and what is not

Order, tiling mode, layout and focus all belong to a workspace: switching to a
tiled workspace and back must not leave the other one tiled, and returning
should put the keyboard where you left it. The layout *engine* stays shared,
because it is a pure function — only the choice of policy is per-workspace.

Window geometry is deliberately **not** stored here. It already lives in the
window's own `UserDataMap` from milestone 3, so a window carries its floating
rectangle across a workspace move for free and there is no second place for
that rectangle to be wrong.

### Decisions worth naming

- **Hidden windows are unmapped, not parked off-screen.** A window at a huge
  coordinate is still in the `Space`: it takes part in hit testing, in layout
  and in "topmost window" queries, so the compositor keeps acting on windows
  nobody can see.
- **Switching to the active workspace returns `None`** — not as an
  optimisation, but because acting on it would unmap and remap everything on
  screen, flickering and dropping focus for nothing.
- **Arriving somewhere populated always focuses something**, falling back to
  the last window. Arriving with no focus makes the keyboard look broken.
- **A moved window is focused where it lands**, so switching after it puts the
  keyboard on the thing you just sent.
- **Removal searches every workspace.** A client can close a window on a
  workspace nobody is looking at, and a leftover entry would reserve a tile for
  a window that no longer exists.
- **Shrinking rehomes windows onto the last surviving workspace.** Losing a
  window because a number in a config file got smaller would be unrecoverable
  from inside the session — which is also why `workspaces.count` is `Live`.

### Digits are read unshifted

`Super+Shift+1` produces a different keysym depending on keyboard layout — `!`
on one, something else on the next. The binding reads the unshifted keysym and
checks the modifier separately. Matching on the shifted symbol works on one
layout and silently fails on every other, which is the kind of bug that only
shows up in someone else's bug report.

### Until there is a panel, the log is the indicator

```
workspace 3 of 4 (windows on: 1, 3)
```

Switching to an empty workspace looks exactly like the compositor having hung,
so it says which ones hold windows.

### The palette is now HadalOS's own

Structure stays niri's — no borders, large radii, low contrast, one accent.
Colour comes from the launcher icon in `HadalOS_Graphics/Icons/menu_icon.png`:
a deep-ocean gradient over trench rock with a single cyan glow at the base.

| | |
|---|---|
| background | `#08111A` |
| surface | `#0F1C2B` |
| accent | `#11C1C6` — the glow |

A desktop whose accent disagrees with its own icon looks like two projects. The
change was one file, `cusk::theme`, and both binaries picked it up — which is
the whole reason the palette was moved there in milestone 8.

### Next

The launcher, which is what the icon is for. After that: per-frame blur of real
window content, and the carried gaps — cursor rendering, `zwp_linux_dmabuf`,
translucent-client handling.

---

## Milestone 10: the launcher, 2026-08-07

`src/cusk-launcher` — `Super+D`. A separate client, for the same reason rofi
and fuzzel are: text input, matching and a scrolling list are an application's
problems, and putting them inside the compositor means a bug in any of them
takes the session down.

Parsing is the whole risk, so it is a pure function over strings with 19 tests
and nothing that opens a window. Against this machine's real applications
directory it reads **149 entries**, and ranks the way a person expects:

```
  fire -> Mozilla Firefox (bin)
  term -> Alacritty, Yakuake
   set -> Print Settings, System Settings, KDE System Settings
  file -> Filelight, Ark, Dolphin
```

Ranking is by kind of match rather than string distance: exact name, then
prefix, then a word inside the name, then anywhere, then the command, then the
comment. Ties break on name, because a list that reshuffles between keystrokes
that score the same is unusable — you aim for the second row and it moves.

### Parsing decisions that are each a bug avoided

- **Field codes are stripped.** `%f`, `%U`, `%i` are placeholders for files the
  launcher is not passing. Left in, they are handed over as literal arguments
  and the application opens and complains it cannot find a file called `%U`.
- **`[Desktop Action …]` groups do not leak into the main entry.** They have
  their own `Name` and `Exec`; reading them into the same map launches the
  wrong command under the right name.
- **Localised keys are skipped**, or whichever locale sorted last becomes the
  name.
- **`NoDisplay` and `Hidden` are honoured.** Offering something the user asked
  to hide is worse than missing an app.
- **First file with a given id wins**, so `~/.local/share` overrides the
  system — which is how someone fixes a broken launcher line without root.

Not implemented, and said out loud rather than left to be discovered: desktop
actions, D-Bus activation, and startup notification.

### Two arrival times, learned the hard way

The compositor special-cases the app id `cusk-launcher`: exempt from tiling,
centred, focused. The first attempt read `app_id` in `new_toplevel` and it was
always `None` — `xdg_toplevel.set_app_id` is a **separate request that arrives
after the toplevel is created**. The symptom was silent: the launcher simply
cascaded like any other window, and the special case looked like it had never
been written.

Moving the check to the first commit fixed the id and exposed the second half
of the same mistake. The launcher was then centred at `x: 640` on a 1280-wide
output — which is `(1280 - 0) / 2`, because a window's geometry is still `0x0`
on its first commit.

So *what* a window is and *how big* it is arrive at different times, and the
classification tracks them separately: the id is recorded only once one has
actually been sent, exemption is applied as soon as it is known (so a relayout
in between cannot tile it for a frame), and placement waits for a non-zero
size. Now: `overlay cusk-launcher centred at (320, 126) (640x420)`.

This is the third time on this project that a value was read before the
protocol guarantees it exists, after `Window::on_commit` in milestone 2 and the
0x0 geometry in milestone 3. The pattern is worth naming: **on Wayland, "the
window exists" and "the window is described" are different events.**

### Details

- Centred horizontally but a third of the way down, not dead centre: a launcher
  pinned to the middle sits under the pointer and covers what you were looking
  at.
- Selection is an accent bar, not a highlight box — the same move the reference
  shells use.
- Arrow keys clamp rather than wrap; holding Down should stop at the bottom of a
  long unlabelled list rather than silently return to the top.
- The query resets the selection to the top on every edit, or Enter launches
  whatever moved into the highlighted row.
- A `Terminal=true` entry is wrapped in the terminal from cusk's *own* config,
  so it matches what `Super+Return` would open.
- The launcher binary is looked for beside cusk before `PATH`, because in
  development the two crates build into separate target directories and
  anything on `PATH` is a stale install. `commands.launcher` overrides it.

### The icon, and a bug it introduced

The launcher shows the HadalOS mark, bundled at `assets/menu_icon.png` — a
**copy** of the artwork outside the repo, because `include_bytes!` resolves at
compile time and an absolute path outside the tree makes the crate build on one
machine only. The cost is the usual one and is written down in
`assets/README.md`: the copy does not follow the original.

The first version built the handle inside `view`. `Handle::from_bytes` stamps a
fresh `Id::unique()` on every call, so each frame handed the renderer a new
texture to upload and a cache that never stopped growing; the launcher mapped
and died about a second later. The comment above that line confidently claimed
iced would hash the bytes and reuse the upload, which is not what the source
says. Built once in `boot` now.

### Next

The workspace indicator the launcher makes room for, per-frame blur of real
window content, and the gaps carried since milestone 1 — cursor rendering and
`zwp_linux_dmabuf`.

---

## Milestone 11: the pointer, 2026-08-07

`src/cusk/src/cursor.rs`. Carried unfinished since milestone 1, deferred twice.

The milestone 2 note said cursor shape "says nothing about whether pointer
routing works". True, and the wrong thing to leave: cusk drew **no pointer at
all**, so every gesture it has — click to focus, Super+drag, drag the divider,
drop a tile onto another — was aimed blind. Running nested, the host's cursor
covered for it. On a tty there would have been nothing on screen.

### Drawn, not loaded

The usual source is an XCursor theme from the filesystem: a search path, a name
lookup, a fallback chain, and a set of failure modes that all end in "no
pointer" — the exact outcome being fixed. A cursor drawn in code is always
available, has nothing to configure wrongly, and is a pure function from a size
to a bitmap, so it is testable without a GPU or a session. Eight tests.

The properties worth having are the ones that catch a shape gone wrong in a way
that still renders: the tip is at the hotspot and is drawn; the arrow is
neither empty nor a solid block (both easy to produce by getting the polygon
test backwards); the far corner is transparent; there is a white body *and* a
dark outline, because a single-colour cursor disappears against something,
always; and the colours are premultiplied, or the outline blends as a halo
instead of a line.

### Client cursors still win

`cursor_image` records the status instead of ignoring it. A client asking for an
I-beam over its text gets its own surface rendered, with the hotspot from the
surface's role data — assuming `(0,0)` there puts an I-beam's tip at its
top-left corner, so text lands a glyph off from where it was aimed.

Every *named* shape gets the arrow, including ones there is no artwork for. The
wrong pointer is usable; no pointer is not.

`Hidden` is honoured, because a client that hides the cursor — a video player,
a game — has a reason to.

### Details

- Drawn last, over everything including the focus ring. A cursor a window can
  cover is one you lose exactly when you are trying to click something.
- Uploaded once. The arrow never changes, so building it per frame would be a
  texture upload per frame for a 24x24 image — the same mistake the launcher
  icon made one milestone ago.
- Cursor surface elements are built before the frame, for the same reason the
  window layers are: constructing elements needs the renderer mutably and the
  frame borrows it for its whole life.

### Next

`zwp_linux_dmabuf`, so clients stop falling back to shared memory — the last
gap carried from milestone 1. Then per-frame blur of real window content, and a
panel to hold the workspace indicator.

---

## Milestone 12: dmabuf, 2026-08-07

The last gap carried from milestone 1. Clients now get GPU buffers.

```
INFO  dmabuf advertised with 133 formats on /dev/dri/renderD128
```

Client stderr, before and after: **4 `libEGL` warnings → 0.**

### Milestone 1's attribution was right; a later inference from it was wrong

Milestone 1 said the `failed to create dri2 screen` warnings appeared only
inside cusk because cusk did not advertise `zwp_linux_dmabuf`. That was
correct. But `wallpaper.rs` later used the same warnings as evidence that
**cusk itself** ran on llvmpipe, and used that to justify blurring on the CPU.

Probing before writing any code: cusk's EGL reports **240 texture formats and
133 render formats, with I915 modifiers**. It has been hardware-accelerated the
whole time. The warnings were always the clients', never cusk's.

The CPU-blur decision survives on its remaining reason — the wallpaper is
static, so a per-frame shader recomputes an unchanging image — and the comment
has been corrected in place rather than deleted, because the wrong reason was
load-bearing when the decision was made.

### Version 3 was not enough, and that was measurable

The first attempt advertised a v3 global with the format list, reasoning that
v4's feedback was "an optimisation for multi-GPU systems". Advertising it
changed nothing: the client still emitted all four warnings and still fell
back.

**Mesa's Wayland EGL learns which render node to open from the feedback's main
device.** A v3 global carries formats but no device, so a client cannot find a
GPU, reports `failed to get driver name for fd -1`, and uses software. Feedback
is not an optimisation here; it is the mechanism.

The device is resolved from `EGLDevice::render_device_path` and `stat`'s
`rdev`, which needs no `backend_drm` feature. If it cannot be resolved, cusk
falls back to a v3 global rather than to nothing — a client that already knows
its device can still use the format list.

### Imports are answered on the next frame

`dmabuf_imported` arrives on the Wayland dispatch, where the renderer is not
reachable — it lives in the winit backend the event loop owns. The dmabuf and
its `ImportNotifier` are queued and drained in the render loop.

Dropping a notifier without a verdict does not make the client fall back: **it
leaves the client waiting for a reply that never comes**, which looks like the
application froze rather than like the compositor failed to answer. Every
queued import is answered, successfully or with `failed`.

### A retry loop found by reading its own output

The verification log had **1020 identical wallpaper warnings in a seventeen
second run**. `Backdrop::build` returning `None` left `backdrop` as `None`, so
every frame tried the same missing file again — and the comment above it
claimed the opposite, that a failure was reported once per change "because the
key is stored either way". It was not stored on failure. A refused key is now
remembered and not retried until it changes.

That is the second comment in two milestones that confidently described
behaviour the code did not have, after the launcher's image handle. Both were
found by running the thing and reading the output, not by re-reading the code.

### Next

Per-frame blur of real window content is now genuinely available — the GPU was
never the obstacle it was assumed to be. Also outstanding: a panel for the
workspace indicator, and translucent-client handling.

---

## Milestone 13: per-frame blur, 2026-08-07

`src/cusk/src/gpublur.rs`, behind `appearance.window-blur`, **default off**.

Milestone 7 blurs the wallpaper. That is most of the effect for none of the
cost, and it is a lie the moment two windows overlap: the top one shows blurred
wallpaper where the window underneath should be. This blurs the composited
scene instead, on the GPU, every frame.

### Assembled in order, not blurred all at once

The obvious version — composite everything, blur it, draw the windows over the
result — is wrong in a way that looks like a feedback loop, because a window's
own pixels are in the blur behind it.

So the scene is built back to front into an offscreen texture, and each window
blurs the texture **as it stands before that window is drawn**. What ends up
behind a window is exactly what is behind it. That ordering is the entire
design; everything else is plumbing.

### Safe API, not raw GL

`GlesFrame::with_context` hands out the raw context, and smithay is explicit
that anything changed there must be restored or the renderer misbehaves far
from the cause. `Offscreen::create_buffer` and `Bind::bind` do the same job
through the API the renderer maintains — no state to restore, no unsafe block.

The awkward part is ownership: blurring needs the scene texture *inside* the
struct while drawing needs it *outside*. It is handed back and forth through
`take_scene`/`put_scene` rather than borrowed, because any other arrangement is
two mutable borrows of one struct.

### Off by default, and that is the point

This is a downsample plus up to six blur steps **per blurring window, per
frame**, replacing something that cost nothing. It is also the most invasive
change the render loop has taken. With the flag off the old path runs
unchanged, byte for byte; with it on, a wrong result is one setting away from
being undone rather than a compositor that will not start.

The shader failing to compile disables the feature and leaves the wallpaper
blur, like the chrome shaders before it.

### Verified, and what is not

Runs clean both ways: 0 GL errors, 0 shader failures, windows mapping normally,
with blur off and on. The transform was reasoned rather than seen when this was
written — offscreen passes render `Transform::Normal` and the output transform
is applied once, at the final blit, where applying it twice or not at all would
both invert the desktop. **Confirmed correct on screen, 2026-08-08.**

### Next

A panel for the workspace indicator.

---

## Milestone 14: window opacity, 2026-08-08

`appearance.window-opacity`, default 1.0, live.

Both kinds of blur are only visible through a window that is not opaque, and
until now that meant every client had to be configured for transparency itself
— `window.opacity` in alacritty's own config, and for most applications no such
setting exists at all. That made blur a property of the terminal rather than of
the desktop.

The hook was already there. `render_elements_from_surface_tree` takes an alpha
as its fifth argument, and cusk had been passing `1.0` since milestone 1.

Setting it there rather than drawing windows through a shader is what keeps
subsurfaces and popups working: they are separate elements in the same tree,
and each one carries the same alpha. A client's own decorations fade with it
instead of floating opaque over a translucent window.

It also switches off occlusion culling for that window, which is required
rather than incidental. `WaylandSurfaceRenderElement::opaque_regions` returns
empty below 1.0, so `draw_render_elements` stops treating the window as
something that hides what is behind it — without that, the blur and wallpaper
underneath would be skipped as invisible and there would be nothing to show
through.

Verified with three windows at 0.82 and a live change back to 1.0: no errors,
and the reload reached the render path.

### Next

A panel for the workspace indicator, which is the last thing `Super+1..9` is
missing — switching to an empty workspace is still indistinguishable from a
hang except in the log.

---

## Milestone 15: the panel, 2026-08-08

`src/cusk/src/panel.rs`. A workspace bar along the top edge, `panel-height`
(default 28, zero hides it).

`Super+1..9` has worked since milestone 9, but switching to an empty workspace
was indistinguishable from the compositor having hung — the only evidence was a
log line. Now each workspace is a pill: **accent and wider when active, filled
when occupied, faint when empty.**

### Drawn by the compositor, and that is a limitation not a design

A panel client would want `wlr-layer-shell`, to sit above windows and reserve
space. iced — which the settings editor and the launcher are built on — speaks
xdg-shell only, so the choice was a compositor-drawn bar or a client pretending
to be an ordinary window and being special-cased into position. The second
would have to be undone the day layer-shell arrives.

### No text, and that is why the pills exist

A window title and a clock are the obvious contents and neither is here,
because cusk cannot draw a glyph: there is no font rasteriser in the
compositor. Rather than add one to get a milestone, the indicator was designed
around what can already be drawn. Rectangles were enough for the thing
`Super+1..9` was actually missing.

### Decisions

- **The active pill is wider, not only a different colour.** Shape survives a
  bad monitor, a colourblind user, and a screenshot at low contrast.
- **`usable_area` is computed in exactly one place**, and tiling, cascade
  placement and maximise all read it. Three separate subtractions would
  eventually disagree by a pixel, and the symptom is a window tucked one row
  under the bar.
- **A click on the panel is consumed whether or not it hits a pill.** The bar
  owns its strip outright; falling through to a window behind it would let a
  press both switch workspace and activate something on the workspace being
  left.
- **The panel is tested before the surface hit**, or a floating window dragged
  over the bar would take the click instead.
- **Pills that would run off the edge are not drawn at all.** Drawing some that
  cannot be reached is worse than drawing none.
- **Drawn after the windows.** Only tiling is obliged to respect the reserved
  strip, so a floating window can still be dragged over it — painted
  underneath, the bar would vanish under the first window someone moved up.

### Verified

Windows now map at `(40, 68)` rather than `(40, 40)` with a 28px bar, so the
reservation reaches placement. Setting `panel-height = 0` live reclaims the
strip. Three windows, no errors.

### Next

A font rasteriser is the gate on everything else a panel wants — window title,
clock, workspace names — and on the launcher showing anything but its own list.
Beyond that, `wlr-layer-shell` would let the panel become a replaceable client,
and the tty backend is what makes cusk a session rather than a window.

---

## Milestone 16: text, 2026-08-08

`src/cusk/src/text.rs`. The panel shows the focused window's title.

`fontdue` rather than a shaping engine: it rasterises a glyph to a coverage
bitmap and reports its metrics, which is the job here and no more.

**What it does not do, stated rather than discovered:** no shaping. No
ligatures, no cursive joining, no reordering. Latin, Greek and Cyrillic come
out right; Arabic and Devanagari do not. A title in a script that needs shaping
will be visibly wrong rather than absent, which is the honest failure and still
a failure. `cosmic-text` is the upgrade path when it matters.

**Nothing is bundled.** A font is found on the system, because shipping one
means a licence decision and a few hundred kilobytes to repeat what
`/usr/share/fonts` already says. A *configured* font that does not exist is not
silently replaced — the user asked for that file, and falling back would leave
them looking at the wrong typeface with nothing said.

### The tests that matter

- **Descenders sit below the baseline of round letters.** `ymin` is the offset
  of a bitmap's *bottom* edge from the baseline; applying it with the wrong
  sign flips every glyph about its own baseline, and that reads as a broken
  font rather than a sign error. Comparing the lowest inked row of "g" against
  "o" catches it in one assertion.
- **Truncated text actually fits its budget.** Cutting by character count
  instead of measured width is the obvious shortcut and is wrong in both
  directions in a proportional font.
- **Ink somewhere, but not everywhere.** All-transparent means nothing drew;
  all-opaque means glyph bitmaps were pasted as blocks.
- **Premultiplied**, like every other texture cusk uploads, or the text haloes
  against the panel.

Advances are summed as floats and rounded once at the end. Rounding per glyph
accumulates up to half a pixel per character, and the measurement is what
truncation and centring depend on.

### Uploaded when the string changes, not per frame

A title is drawn every frame and changes rarely. This is the third place that
mistake could have been made — after the launcher icon and the cursor — so it
is cached by string, and the rasterised image is cached inside `Face` as well.

### Also

The title is centred, kept clear of the pills on **both** sides so it cannot
slide underneath them on a narrow screen, and truncated to what is left.

### Next

A clock is the obvious next panel item and needs a date/time dependency for the
local timezone, which `std` has no way to determine. Beyond that:
`wlr-layer-shell` to make the panel a replaceable client, and the tty backend,
which is what turns cusk from a window into a session.

---

## Milestone 17: the tty backend, phase one, 2026-08-08

`src/cusk/src/tty.rs`, behind `--probe-drm`. Nothing is driven yet.

Running on a virtual terminal is what turns cusk from a window inside someone
else's session into a session of its own, and it is the largest single piece of
work in the project: session management, DRM mode-setting, GBM buffers,
libinput and udev hotplug. The failure mode is also unusually harsh — take DRM
master, get the mode wrong, and the result is a black screen on a VT with no
way back but a hard reset.

So it is being built in phases, and phase one cannot hurt anything: it opens
devices, reads what is connected, prints it, and exits. **It never becomes DRM
master.** Master is exclusive and KWin holds it; asking would either fail or
take the display away from the running desktop. Reading connectors and modes
needs no master, which is exactly why this much is checkable from inside a
working session.

### Dependencies, checked before writing code that needs them

Present on this machine: libseat 0.9.3 (linked against libsystemd, so the
logind backend exists), libinput 1.31.3, libudev, libgbm, libdrm 2.4.134,
`/dev/dri/card0`, and membership of `video`, `input` and `seat`. The five
smithay features — `backend_drm`, `backend_gbm`, `backend_libinput`,
`backend_session_libseat`, `backend_udev` — compile against them.

### The finding: session control is exclusive

Acquiring a session fails from inside the desktop, with `ENOSYS`. That is not a
misconfiguration. logind grants `TakeControl` to **one process per session**,
the running compositor holds it, and libseat — finding the logind backend
unusable and `seatd` not running — reports that no backend worked. Two session
controllers on one session would each believe they owned the input devices.

Worth knowing now rather than after the mode-setting code exists: **the tty
backend can only ever be tested from its own VT.** There is no arrangement that
lets it come up beside a running desktop.

So the probe falls back to opening the device directly, which needs only the
`video` group, and says plainly why. From inside the KDE session:

```
  no session (Failed to open session: Function not implemented (os error 38))
  reading devices directly instead — this is expected inside a
  running desktop, because session control is held by one process
  at a time and the compositor already running holds it.

  /dev/dri/card0
      EmbeddedDisplayPort-1 connected
          preferred  1920x1080@60
      HDMIA-1      disconnected
      DisplayPort-1 disconnected
```

### Confirmed from a VT, 2026-08-08

```
  session acquired, seat: seat0

  /dev/dri/card0
      EmbeddedDisplayPort-1 connected
          preferred  1920x1080@60
```

The fallback message is gone, so libseat's logind backend granted
`TakeControl` once no other compositor held it — **without root**. Every
prerequisite for phase two is now verified rather than assumed: the libraries
link, the session is obtainable, the device opens through it, and the target is
eDP-1 at 1920x1080@60.

---

## Milestone 18: the tty backend, phase two — setting a mode, 2026-08-08

`cusk --modeset-test [--seconds=N]`. Sets a real mode on the first connected
output, fills it with HadalOS blue, and puts the display back.

The first step that can strand a screen, so the safety design matters more than
the mode-setting.

### The watchdog is armed before master is taken

A thread sleeps, restores the saved CRTC, drops master and hard-exits — and it
is started **before** `acquire_master_lock`, so the mode-set itself is inside
its window. Arming it afterwards would leave the one operation most likely to
hang as the one operation it could not rescue.

It exits with `process::exit` rather than unwinding, because by the time it
fires the main thread is by definition not answering, and unwinding would wait
on the thing that is stuck.

### Restoring is not a courtesy

The previous CRTC — mode, framebuffer and position — is read before anything is
touched and put back on every path out: the normal one, the watchdog, and
before the buffer is destroyed. A compositor that takes master, sets a mode and
exits leaves the display scanning out whatever was last there, with no text
console to type into.

The target CRTC is the one already driving that connector, via its encoder,
rather than any free one — so the saved state and the restore refer to the same
hardware.

### A dumb buffer, not GBM

Phase two only has to prove a mode can be set and something appears. A DRM dumb
buffer is allocated, mapped, and filled with a colour, which skips GBM and EGL
entirely. Those arrive with the render loop, where they are actually needed.

### It refuses rather than fights

Run inside the desktop it stops at session acquisition and says so, never
reaching DRM master:

```
  could not join a session: Failed to open session: Function not implemented
  This needs its own VT — see --probe-drm.
```

### Confirmed from a VT, 2026-08-08

The screen turned HadalOS blue and returned to the console cleanly. The
riskiest step in the whole backend, and it behaved.

---

## Milestone 19: the tty backend, phase three — a keyboard, 2026-08-08

`--modeset-test` now opens libinput on the session's seat, and **Escape ends it
early**.

That is two things at once. It proves input reaches the compositor from a VT,
and it replaces "wait for the watchdog" with a way out that a person can
choose. The watchdog is still the guarantee; this is the door beside it.

### Decisions

- **libinput is opened through the session**, like the DRM device. It needs
  `/dev/input/event*`, and logind hands those to whoever holds session control.
  Opening them directly works as root and nowhere else, and a compositor that
  needs root to read a keyboard is not one anyone will run.
- **Opened before master is taken.** A keyboard that cannot be reached then
  fails while the console is still readable, rather than behind a blue screen.
- **A missing keyboard is not fatal**, it is reported — `the watchdog is the
  only way out` — and the test still runs.
- **Polled at 16ms rather than slept.** A single sleep cannot be interrupted,
  so Escape would have had nothing to interrupt.
- **The key report is printed after the mode is restored.** Anything written
  while the blue screen is up goes to a console nobody can read, which is the
  same reasoning as restoring before destroying the buffer.
- **The keys seen are reported, not just "input works".** Those are different
  claims and only the second is worth making.

### Confirmed from a VT, 2026-08-08

Escape ended the test early and the key codes were reported. Input reaches the
compositor from a tty.

---

## Milestone 20: the GPU path, 2026-08-08

`--probe-render`. The unknown standing in front of phase four, removed before
it could surface in the middle of a refactor.

Everything on the tty so far has used a **dumb buffer** — CPU memory the
display controller scans out — which says nothing about whether the GPU path
works there. The render loop needs GBM to allocate, EGL on the DRM node for a
context, and a `GlesRenderer` on top. Any of those failing during the
restructure of `main.rs` would be expensive to diagnose; failing here costs
twenty lines.

It uses the **render node**, so it needs neither a session nor DRM master and
runs from inside a desktop. Scanout is the part that needs the card node, and
scanout is already proven by the mode-set test.

```
  /dev/dri/renderD128 — GBM, EGL and GlesRenderer all work, and the pixels are right
```

**It reads the pixels back rather than trusting the return values.** A context
on the wrong device can succeed at every call and render nothing; "the calls
returned Ok" and "the picture is right" are different claims and only the
second is worth making. The colour has three distinct channels, so a channel
swap shows up as a wrong answer rather than as the same number twice.

---

## Milestone 21: the render loop comes out of `main`, 2026-08-08

Phase four, step one. `draw_frame` and `FrameContext`, with **winit still the
only driver**.

A second backend cannot be added while drawing a frame is 500 lines welded into
`main`'s loop, threading ten separate locals through it. So the seam is cut
first, with nothing on the other side of it yet — the riskiest part of adding
DRM, done where any breakage shows up immediately in the nested compositor
rather than on a tty with no way to read a log.

### The seam

`draw_frame` is about *what* is on screen. Obtaining a framebuffer, presenting
it and pumping input stay in the driver, and those are precisely the parts that
differ between nested and tty. `FrameContext` gathers the state that outlives a
frame — shader programs, uploaded textures, the backdrop cache, the font —
because a second driver would otherwise have to thread the same ten locals.

`transform` is now a parameter rather than a constant. It is the **driver's**:
winit hands back a framebuffer that is already flipped and DRM does not, and
`Transform::Flipped180` was hardcoded in the middle of the drawing code where
the second backend could not reach it.

### Verified as a no-op

The point of a refactor with one driver is that nothing should change. With
wallpaper, blur, window blur, opacity, corners, panel, title and three windows:
**no errors, no warnings**, and 139 tests pass. One incidental confirmation —
the missing-wallpaper warning appeared exactly once rather than 1020 times, so
the refused-key fix from milestone 12 survived the move.

### The DRM driver cannot use `DrmCompositor`

Found while planning the next step. `DrmCompositor::render_frame` takes a slice
of `RenderElement`s, but `draw_frame` draws **into a framebuffer** — solid
rectangles, texture blits, two custom shaders. Expressing that as render
elements would mean wrapping every one of those in an element type, which is a
rewrite of the drawing code and defeats the seam this milestone just cut.

So the driver takes the manual path: allocate through GBM, export as a dmabuf,
`renderer.bind` it, call `draw_frame`, and hand the same buffer to the display
controller. More code than `DrmCompositor`, and the only version that lets both
backends share one drawing routine.

---

## Milestone 22: the two halves join up, 2026-08-08

`--probe-scanout`. The last unproven link before the driver.

`--modeset-test` scans out a **dumb buffer** — CPU memory. `--probe-render`
renders with the **GPU** into an offscreen texture nobody displays. Neither says
the two join, and joining them is the whole of the DRM driver:

```text
  GbmAllocator -> GbmBuffer -> Dmabuf -> renderer.bind() -> draw
                            -> add_planar_framebuffer -> set_crtc
```

One buffer, seen by the GPU as a render target and by the display controller as
a scanout source. If the format, modifier or flags are wrong, one of the two
rejects it.

- **`GbmBufferFlags::SCANOUT` is the flag that matters.** Without it allocation
  still succeeds and `add_planar_framebuffer` refuses the buffer later, which
  reads as a mode-setting failure rather than an allocation one.
- **Allocated on the card node, not the render node.** The buffer has to be
  scannable by *this* display controller, and one allocated against another
  device may be neither shareable nor scanout-capable.
- **Two GBM devices on two dups of the same fd**, because `EGLDisplay` consumes
  one and the allocator needs the other. Same hardware either way, which is
  what makes the buffer usable by both.
- **Single-buffered on purpose.** Double buffering and page flips are the
  driver's problem; adding them here would test something this does not claim.
- **The framebuffer flags are derived from the buffer, not hardcoded.** See
  below — this was wrong first time and it panicked.
- Teal rather than blue, so it is distinguishable from the mode-set test on
  screen without reading the log.

### The flags and the buffer have to agree

First run on a VT panicked inside drm:

```
assertion failed: (has_modifier && modifier.is_some()) || (!has_modifier && modifier.is_none())
```

`add_planar_framebuffer` **asserts** that `FbCmd2Flags::MODIFIERS` is set
exactly when the buffer carries a real modifier. The allocation asked for
`Modifier::Linear`, so the buffer had one, and `FbCmd2Flags::empty()` was
passed anyway — a panic rather than an error, because drm treats the mismatch
as a programming mistake, which it was.

The flag is now derived from the buffer with the **same predicate the assertion
uses**, so the two cannot drift. That predicate has a trap in it worth naming:
`Invalid` is `Some`, so a naive `is_some()` sets the flag and then fails the
assertion, because drm filters `Invalid` out before comparing. A driver can
return `Invalid` for a buffer allocated with an explicit modifier, so this is
not hypothetical.

`framebuffer_flags` is a pure function with three tests, and `--probe-render`
now reports what the driver actually returns — checkable from a desktop rather
than costing a trip to a VT:

```
a Linear allocation reports Some(Linear), so framebuffer flags = FbCmd2Flags(MODIFIERS)
```

---

## Milestone 24: one compositor, two drivers, 2026-08-09

`build_compositor` and `Compositor`. The last groundwork before the DRM loop.

The Wayland side — display, globals, seat, socket — was built inline in
`main`, so a second driver could not reach it. Each backend constructing its
own would be two places for a global to be registered and one of them to be
forgotten, and a missing global shows up as a client that starts and draws
nothing rather than as an error.

**The dmabuf global is deliberately not built here.** It needs the renderer's
format list, and only the driver has a renderer. It is the one global each
backend registers for itself, which is stated in the function rather than left
for someone to wonder about.

`mod_key` moved in with it: it is resolved from the config plus the `CUSK_MOD`
override, and that resolution belongs with the seat it arms rather than beside
the backend that happens to run first.

### Verified as a no-op

Wallpaper, blur, window blur, opacity, corners, panel, title, dmabuf, font,
three windows: **no errors, no warnings**, 142 tests.

One thing this run caught about the *tests* rather than the code: an earlier
verification had silently exercised nothing, because the scratchpad had been
cleaned and cusk had regenerated a default config — no wallpaper set, so no
backdrop, no blur and no warning either. "No warnings" from a run that did
nothing looks exactly like "no warnings" from a run that did everything. The
re-run checks for the `wallpaper ready` line specifically.

---

## Milestone 25: cusk on a tty, 2026-08-09

`cusk --tty [--seconds=N]`. The compositor, on a virtual terminal, drawing the
real scene and accepting real clients.

The winit loop's counterpart, and deliberately small because milestones 21 and
24 did the work: obtain a framebuffer, pump input, call `draw_frame`, present,
dispatch clients. Everything about *what* is drawn is shared.

### Decisions

- **Two buffers.** A frame is never drawn into the buffer the display is
  reading, or every frame is visible while it is still being assembled.
- **`set_crtc`, not a page flip.** A flip is asynchronous and its completion
  arrives as a DRM event that must be read before the next can be queued.
  Doing that needs an event loop this driver does not have yet. `set_crtc`
  tears, which is visible and honest; a flip queued twice without draining
  returns `EBUSY` and the screen silently stops updating.
- **Everything fallible happens before the display is taken.** Device, buffers,
  libinput and the compositor are all built first, so a failure lands on a
  readable console.
- **The watchdog is armed before master**, as everywhere else here.
- **`Transform::Normal`.** winit hands back an inverted framebuffer; DRM does
  not. That is the parameter milestone 21 introduced, earning itself.
- **Keycodes are offset by eight.** libinput reports evdev codes and xkb wants
  them shifted; feeding the raw code moves every key one row and reads as a
  broken layout rather than an offset.
- **Time-boxed.** VT switching and session pause/resume are not handled, so a
  compositor running indefinitely would hold DRM master across a switch away
  and leave the other VT blank. Escape ends it early.

### Two failures on the first VT run, one assumption

As an ordinary user: `could not become DRM master: Permission denied`. The
session *was* acquired — it printed `cusk on seat0, 1920x1080` — so the failure
was the explicit `SET_MASTER` call.

**That call was the mistake.** With logind, `TakeDevice` on an active session
already returns a master-capable fd; logind does the granting, and a process
asking for master itself needs root. `EACCES` there usually means "you are
already master and did not need to ask". Most compositors on logind never make
the call. It is now attempted and ignored, with `set_crtc` — which reports its
own error — as the real test.

Under `sudo` it failed differently: `XDG_RUNTIME_DIR is not set or invalid`,
because sudo strips it and the Wayland socket cannot be created without it. So
`sudo` was never a workaround, only a different failure, and the error now says
so rather than naming a variable and leaving the cause to be guessed.

Worth stating plainly, because the whole design assumes it: **the tty backend
does not need root.** Every phase up to this one was built to make that true.

### First tty run: pointer, and a silent config

The compositor came up on the real screen — workspace pills and cursor drawn at
1920x1080. Two things were wrong.

**The cursor did not move.** Expected: pointer input was the stated gap, so the
cursor was drawn at a `pointer_location` that started at the origin and never
changed. Now wired: motion is accumulated across a drain rather than dispatched
per event — a touchpad emits dozens of deltas between frames, and each one
would otherwise cost a hit test and an enter/leave pass for a pointer that ends
up somewhere once. Clicks focus and route exactly as they do under winit.

`clamp_pointer` keeps it on screen, and that is not defensive: a pointer that
can leave has no desktop edge to catch it and no other compositor to reset it,
so it is the difference between a usable session and one killed from another
VT. It stops one pixel short of the far edge, because a pointer exactly at
`width` is outside every window and the rightmost column would never respond.

**The wallpaper did not appear, and that was a silent failure of mine.** The
config had a duplicate `window-blur` key and a repeated `[commands]` header —
both TOML errors — so it failed to parse and *every* setting fell back to its
default. The panel appeared because 28 is the default; the wallpaper did not
because empty is.

The winit path warns about that. The `--tty` path did
`Config::load(..).unwrap_or_default()`, which **discarded the error entirely**.
A file that will not parse is precisely the case where the user most needs to
be told, and it was the one case that said nothing. It now prints the parse
error with its line number and says every setting is at its default until it is
fixed.

The settings GUI was suspected and cleared: a test that edits the generated file
the way the GUI does — including writing the same key twice — confirms the
result still parses with no duplicate keys or repeated headers. The duplicates
were hand-edited.

### Verified on a VT, 2026-08-09

Cursor tracks the touchpad, wallpaper renders, pills and panel draw. cusk runs
on a virtual terminal, without root.

---

## Milestone 26: VT switching, 2026-08-09

The time box is gone. `--seconds=0` runs until Escape.

**The notifier was being dropped.** `LibSeatSession::new` returns a session *and*
a notifier, and every phase so far bound the second to `_notifier` — so cusk
never learned that the user had switched away. logind revokes device access on
a VT switch and restores it on switching back; a compositor that keeps drawing
through one is writing to a revoked fd, failing every frame, and reporting it to
a console nobody is looking at.

### While paused, nothing happens

No drawing, and **no input reading** — libinput is suspended. Continuing to read
would steal keystrokes from whoever is actually using the machine, which is a
worse failure than a blank screen and much harder to attribute.

### Resume is nearly free, by accident

Milestone 25 chose `set_crtc` over a page flip because flips need an event loop
to drain completions. That decision pays here: `present` sets the CRTC every
frame, so **the first frame after a resume restores the mode** with no special
case. The only thing resume has to do is tell libinput to reopen its devices.

### calloop, but only for this

The notifier is a calloop event source, and the driver is a plain loop. Rather
than restructure everything, a calloop loop holding just the notifier is
dispatched with a **zero timeout** once per frame — a poll inside the render
loop rather than the loop itself. Moving the whole driver onto calloop is worth
doing when DRM page-flip completions need draining, and not before.

### Switching *away* is the compositor's job too

The first unbounded run could not leave: Ctrl+Alt+F1 did nothing. Holding
session control means **logind has disabled the kernel's own VT switching**, so
a compositor that does not implement the chord traps the user on its terminal —
which is a worse trap than the time box it had just removed.

Bound now, and checked before the key reaches the compositor, then not
forwarded: a client receiving the F-key as well would act on it while the
screen is being handed away.

Read as **raw evdev codes**, not xkb keysyms. The keysym route depends on the
layout including `srvr_ctrl(fkey2vt)`; when it does not, `XF86Switch_VT` never
arrives and VT switching silently does not work, which is the hardest kind of
failure to attribute. Switching terminals is a physical-key operation, so a
physical key is the right thing to read.

The trap worth a test: **F11 and F12 are not adjacent to F1..F10 in evdev**
(87 and 88, against 59..68), so one subtraction gets them wrong and the symptom
is landing on the wrong terminal.

### Switching away killed it, and that was a race

The switch worked; cusk then exited with `could not restore previous mode:
permission denied`.

logind revokes device access **the instant the switch begins**, but
`PauseSession` only arrives on the next notifier dispatch. One `present` runs in
that window, `set_crtc` returns `EACCES`, and it was propagated straight out of
the loop as a fatal error.

Two fixes, both about not collapsing a distinction:

- `present` returns the raw `io::Error` instead of a string, so the caller can
  tell *this is not our display right now* from *this is broken*. `EACCES` and
  `ENOTCONN` are treated as a pause — the notifier confirms it a moment later,
  and `just_resumed` still fires on the way back. Any other error is still
  fatal, or a genuinely broken device would look like a VT switch and cusk
  would spin forever pretending to be paused.
- Restoring the mode on exit no longer complains when the device is revoked.
  Another VT owns the display, so there is no mode of ours to put back, and
  "could not restore" there is alarming and wrong.

### The time box was load-bearing until now

An unbounded run could previously hold the display forever, which is why every
phase carried `--seconds` and a watchdog. With a VT switch releasing the screen
there is always a way out, so `--seconds=0` is now safe — and the watchdog is
skipped in that mode rather than firing on a session someone is using.

---

## Milestone 27: one binding table, 2026-08-09

The tty driver shipped **with none of the compositor bindings**. It forwarded
every key straight to the client: no Super+Return, no launcher, no workspace
switching, no tiling toggles. A session with a wallpaper, a panel, a cursor —
and no way to open a terminal.

Not a subtle bug, and the same shape as the two before it. The whole table
lived inside the winit event handler, so the second driver could not reach it,
exactly as `draw_frame` and `build_compositor` could not be reached before they
were lifted out.

- **`binding_for(sym, shift)`** is a pure function, so both drivers agree by
  construction rather than by diligence. Three tests, including one asserting
  that *every binding the banner advertises actually resolves* — a table this
  long is where an entry goes missing, and the symptom is one dead key among a
  dozen working ones.
- **`apply_binding`** is a method for the same reason `draw_frame` is a
  function: two drivers, one behaviour. The socket name and the configured
  programs are arguments rather than fields, because they belong to the session
  rather than to the compositor.
- Unbound keys must forward. Claiming them would make ordinary typing vanish
  whenever the modifier happened to be down, which is tested too.

### Verified

winit unchanged: wallpaper, blur, opacity, corners, panel, title, three
windows, no errors and no warnings. 122 tests.

That check had to be run twice. The first attempt reported clean — and had
silently exercised nothing, because the scratchpad wallpaper had been cleaned
away again. **A run that does nothing produces the same output as a run that
does everything**, so the check now counts the `wallpaper ready` line rather
than trusting the absence of warnings. Second time this session.

---

## Milestone 28: the rest of the driver parity, 2026-08-09

Floating windows could be clicked on the tty and not dragged. The Super+drag
and Super+right-drag logic lived inside the winit button handler — **the third
time in three milestones** that behaviour welded into one driver turned out to
be invisible to the other, after `draw_frame`, `build_compositor` and the
binding table.

`Cusk::press` now owns it, and the order in it is the substance: the panel is
tested first because it owns its strip outright, focus is taken before any
modifier handling so a Super+drag also focuses, and a consumed press is not
forwarded or Super+drag selects text in the terminal it is dragging.

**Hot reload was winit-only too**, which meant the settings editor could be open
on one backend and inert on the other — in the one feature whose entire point is
that an edit lands immediately. The tty driver now watches the file as well.

### The pattern, named

Three milestones, three instances, one shape: *anything welded into a driver is
invisible to the other, and the failure is silence rather than an error*. Not a
missing feature that errors, not a wrong result — a key that does nothing, a
window that will not move, a config that will not reload.

An audit for the rest turned up one more, in a different category: **neither
driver handles scroll**. That is a missing feature rather than a parity gap, so
it is recorded here rather than fixed under this heading.

---

## Milestone 29: who draws the titlebar, 2026-08-09

`zxdg_decoration_manager_v1`. §3 said floating and tiling are two policies over
one window set; this is where that becomes visible on screen.

- **Floating** — a window is its own object, so it keeps its titlebar. That bar
  is what a client offers to drag, which is the other half of what floating
  mode means.
- **Tiling** — position is computed, so the bar is a lie. It cannot be dragged
  anywhere meaningful, it eats a row of every tile, and the thing it names is
  already in the panel. Clients are told `ServerSide` and cusk draws nothing,
  which is how the bar disappears.

Toggling tiling re-sends the mode to every window, because leaving the bars
behind when tiling turns on is exactly the half-applied state that call
prevents.

**A client that insists is not overruled.** The protocol is a negotiation and
some toolkits cannot turn their decorations off; forcing the mode would leave a
window rendering wrongly rather than one with a spare titlebar.

### The drag, not yet explained

Dragging a client's own titlebar does not move the window. `move_request` is
wired and calls the same grab a Super+drag uses, and that path was verified
working in milestone 2 — so either the client is not asking, or the grab is not
taking, and those have completely different causes.

Rather than guess, `move_request` now logs at info, like `start_move` beside
it. One line distinguishes them:

- neither line — the client never sent `xdg_toplevel.move`
- `client requested a move` alone — the request arrived and the grab refused
- both — the grab started and something else is wrong

Three wrong guesses preceded the milestone 2 fix, and instrumenting took one
round and cost less than any of them. Same choice here.

---

## Milestone 30: scroll, 2026-08-09

Neither backend handled it, so scrolling in any terminal did nothing. Not a
parity gap — a feature missing from both, found by auditing for parity.

### Three sources, and the differences are the work

- **Wheel** carries discrete steps as well as a smooth value. Sending only the
  smooth one makes every wheel behave like a trackpad, and "scroll one notch"
  stops meaning anything.
- **Finger** ends a gesture with a **zero on the axis that stopped**. That zero
  is information: it ends kinetic scrolling. Dropped as "an empty event", a
  touchpad flick keeps coasting in any application with momentum, which reads
  as the scroll being stuck.
- **Continuous** — a trackpoint — has neither steps nor lifts.

So an empty event must be dropped *except* for a finger, where empty is the
message. Both halves are tested, along with each axis stopping independently:
stopping both when only one ended makes a diagonal flick finish sideways.

`axis_frame` is shared, because the mapping carries enough judgement to be
worth having once rather than twice — which is the lesson of the three parity
milestones applied before the divergence rather than after it.

### Next

Hotplug, and page-flip timing to replace the tearing `set_crtc`. Also
outstanding: why a client's own titlebar does not drag, which the `move_request`
log line will answer on the next tty run. on eDP-1, rendering a single colour, with a hard timeout that
   restores the VT — the first thing that can strand a screen, so it should be
   unable to strand it for more than a few seconds.
2. libinput, so there is a keyboard.
3. The render loop, abstracted over winit and DRM.
4. udev hotplug and VT switching.
