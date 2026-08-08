# Bundled artwork

`menu_icon.png` is a **copy** of the source artwork, which lives outside this
repository at `HadalOS_Graphics/Icons/menu_icon.png`.

It is copied in rather than referenced, because `include_bytes!` resolves at
compile time and an absolute path outside the repo makes the crate build only
on one machine.

The cost of that choice is the usual one: this copy does not follow the
original. Re-copy it when the icon is redesigned.
