# Pincer

Pincer is a Wayland clipboard manager inspired by Vim's register model.
Pincer is written in Rust, hence the name: crabs use their pincers to hold things, and pincer will hold as many clippings as you need it to (almost).

Pincer consists of a daemon that does the actual clipboard management, and a command-line program for controlling it.
The daemon maintains ten numeric registers, `"0` through `"9`, and 26 named registers, `"a` through `"z`, and a, possibly unset, register pointer.
The numeric registers are a first-in-first-out queue for your clipboard:
Whenever you copy something, it goes in the `"0` register, pushing any previous clippings to the next higher register.
The named registers are an associative array.
If the register pointer is set to `"a`, say, the next clipping will also be stored in the `"a` register.
By setting the register pointer, you can choose which register your next paste will read from.
Thus, `pincer register set a` means that your next copy will be like `"ay` in Vim, and your next paste will be like `"ap`.

Of course, opening a terminal and typing 22 characters is not convenient to select a register is not convenient.
Pincer is meant for keyboard oriented desktop environment such as sway, where you can
create a key binding for `pincer register set`, which will then grab the next alphanumeric key.
You can query the daemon for the selected register with `pincer register active`, and display it on your status line.
See example configuration files for sway and i3blocks.

You can also query the daemon for the current contents of each register.
This integrates well with rofi, so that you can filter by contents, select the register with the clipping you want, and paste it.
