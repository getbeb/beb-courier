# beb-courier

Carries [beb](https://github.com/getbeb/beb)'s mail between machines.

beb owns no network. What it cannot deliver waits in an outbox, and the
filename says who it is for, so a courier moves it without opening an
envelope.

```console
$ beb-courier sync
beb-courier: 2 sent, 1 received
```

## Install

On every machine where your agents read and write mail:

```sh
curl -fsSL https://getbeb.dev/courier.sh | sh
```

Or from source with cargo (Rust 1.75+):

```sh
cargo install --git https://github.com/getbeb/beb-courier
```

It needs `beb` and `ssh` on PATH, and somewhere to carry to.

## Quick start

One courier per machine, not one per identity. Both sides run `init` and
hand over what `whoami` prints:

```sh
beb-courier init
beb-courier whoami > alice.handover
```

Then, with the file the other side gave you:

```sh
# on alice
beb-courier route add bob.handover ssh://bob.lan   # where bob's mail goes
beb-courier authorize bob.handover                 # let bob's courier drop here

# on bob, the same with the other file
beb-courier route add alice.handover ssh://alice.lan
beb-courier authorize alice.handover
```

That is the whole link. `route add` reads the addresses in the handover
and `authorize` reads the key, so nobody types a fingerprint or an
address.

Where you have ssh to the other side, no file has to exist at all:

```sh
beb-courier whoami | ssh bob.lan beb-courier authorize -
```

Mail moves on `sync`, one pass and an exit code, which is the shape for a
timer or an agent's turn boundary:

```console
$ beb-courier sync
beb-courier: 1 sent, 0 received

$ beb-courier carry
beb-courier: carrying outbound only, since nothing shelves for you
beb-courier: mail still lands the moment a peer drops it in
```

## Automatic delivery and collection

`beb-courier unit` prints the file this platform's supervisor reads to
keep `carry` running:

```sh
# linux
beb-courier unit > ~/.config/systemd/user/beb-courier.service
systemctl --user enable --now beb-courier

# macos
beb-courier unit > ~/Library/LaunchAgents/dev.getbeb.courier.plist
launchctl bootstrap gui/$(id -u) ~/Library/LaunchAgents/dev.getbeb.courier.plist
```

## Through a depot

When you and the other side cannot reach each other, name one place you
both can. That is what a [beb-depot](https://github.com/getbeb/beb-depot)
is:

```sh
beb-courier init
beb-courier route add ssh://depot.internal   # everything not routed elsewhere
beb-courier whoami > laptop.handover
```

```sh
# on the depot
beb-depot authorize laptop.handover
```

That lets this machine connect. A depot hands mail only to a courier it
has been told collects for an address, so each identity says so itself:

```console
$ BEB_IDENTITY=~/newthing beb whoami | beb-courier register
beb-depot: SHA256:abc... may now collect for a0c0fd70..., by its own signature
beb-courier: registered at depot.internal; mail for it arrives here now
```

The depot verifies the claim against the key inside it, so a new
identity needs nobody there. `beb-courier unregister ADDRESS` undoes it,
and `beb-depot revoke` does the same from the depot.

Then mail moves:

```console
$ beb-courier sync
beb-courier: 1 sent, 1 received
```

## Routes

```console
$ beb-courier route
ssh://depot.internal
d811f21767d40b61...0fe756e ssh://bob.lan   # beb-courier@bob
beb-courier: 2 routes; nothing waiting to leave
```

An exact route wins over the address-less one, and a route that fails
does not fall back: the frame waits in the outbox and `sync` names it and
the host. Delivering it the other way would hide a dead link behind mail
that still arrives.

## Is it working

```console
$ beb-courier status
beb-courier: 0.5.0 at ~/.local/bin/beb-courier, root ~/.local/share/beb-courier
beb-courier: 3 addresses, 0 waiting to leave, beb is beb 0.11.0
beb-courier: 2 routes, collecting from depot.internal
beb-courier: supervised by ~/Library/LaunchAgents/dev.getbeb.courier.plist
beb-courier: depot.internal answers and knows this key
beb-courier: bob.lan answers and knows this key
beb-courier: depot.internal grants 3
```

A grant and a mailbox are two facts on two machines, and this is the only
thing that reads both. Either can go stale in silence: mail for an
identity nobody granted is refused where you cannot hear it, and a grant
left behind by a deleted identity keeps a queue alive that nothing will
collect.

```console
$ beb-courier status
beb-courier: 65807a70 is granted there and reads nowhere here
beb-courier: beb-courier unregister 65807a70a1b2c3d4e5f60718293a4b5c6d7e8f90a1b2c3d4e5f6071829304152
beb-courier: one thing does not agree
```

Every place is probed with an intent it refuses, which proves it is
reachable and knows this key without moving any mail. Exit 3 when
something disagrees, and it says which:

```console
$ beb-courier status
beb-courier: ~/.config/systemd/user/beb-courier.service does not name
             /home/me/.local/bin/beb, so the service and this shell
             disagree about beb
beb-courier: one thing does not agree
```

## Commands

```console
$ beb-courier
beb-courier 0.5.0 carries beb's mail to wherever each recipient is.

  beb-courier init
      mint this machine's courier key
  beb-courier whoami
      this machine's key and the addresses it reads for, for the far side

  beb-courier route
      every route, and whether the outbox matches them
  beb-courier route add [ADDRESS] PLACE
      where a recipient's mail goes; ADDRESS is an address or the
      handover that machine printed, and left off means everything else
  beb-courier route rm ADDRESS | PLACE
      stop routing one address, or everything going to one place

  beb-courier authorize KEYFILE
      let that courier drop mail here, over ssh
  beb-courier register
      say a new identity reads here, in its own signature; the address
      comes from stdin, so: beb whoami | beb-courier register
  beb-courier unregister ADDRESS
      give up this machine's claim on one address

  beb-courier sync
      push what is waiting to leave, pull what is waiting to arrive
  beb-courier carry
      the same, until stopped: arrivals land as they happen, and what
      is waiting to leave goes as it is written
  beb-courier unit
      the supervisor file this platform reads
  beb-courier status
      whether this machine can still carry, and what it carries for

  beb-courier --help
  beb-courier --version

Exit: 0 did it, 1 change the command, 2 nothing to do, 3 refused.

A PLACE is ssh://[user@]host[:port]. A route is an address and a place,
or a place alone for everything else, and an exact route wins. A KEYFILE
is a courier's public key, or the handover carrying it, or "-" for stdin.

Collection is from the address-less route and nowhere else, since only a
place that shelves has anything to hand back. A peer holds nothing: it is
written into directly, and that is the wake.

BEB_COURIER_ROOT holds the key, the routes, and an ssh_config if this
machine needs one. It defaults to ~/.local/share/beb-courier:

  id_ed25519    this courier's key, minted by init
  routes        one line each: an address and a place, or a place alone
  ssh_config    optional, and used only if it is there

BEB_BIN names the beb to hand mail to, and defaults to what is on PATH.
```

`ssh_config` is there because ssh expands `~` from the password
database and not from `HOME`, so a courier running as a daemon user has
no other way to be told a place's host key. Put `UserKnownHostsFile`
in it, or a `ProxyJump`. Leaving it out means "use this account's
ordinary ssh setup", which is right when the courier runs as a person.

It will not pass `StrictHostKeyChecking=no`. That accepts any machine
answering on the depot's address, and host authentication is the one
thing ssh does here that nothing else does.

## Design

A courier reads the outbox, hands each frame to `beb drop`, and never
opens an envelope. [DESIGN.md](DESIGN.md) has the custody rules, the seam
with beb, what a routing table must never become, and what a courier
refuses to know.

## License

MIT
