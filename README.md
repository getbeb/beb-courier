# beb-courier

Carries [beb](https://github.com/getbeb/beb)'s mail between machines.

beb owns no network. What it cannot deliver goes to an outbox, and the
filename says who it is for. This ships that, collects what a
[depot](https://github.com/getbeb/beb-depot) is holding, and hands each
frame to `beb drop`.

```console
$ beb-courier sync
beb-courier: 2 sent, 1 received
```

## Install

From source with cargo (Rust 1.75+):

```sh
cargo install --git https://github.com/getbeb/beb-courier
```

It needs `beb` and `ssh` on PATH, and a depot to carry to.

## Quick start

One courier per machine, not one per identity.

```sh
beb-courier init ssh://depot.internal
```

That mints a key in `~/.local/share/beb-courier` and writes down the
depot. Now hand the operator both things they need, in one file:

```console
$ beb-courier whoami > laptop.handover
beb-courier: 1 key, 2 queues this machine reads for
beb-courier: give this to whoever runs the depot: beb-depot authorize <this file>
```

The file is this machine's public key, then the queue names it reads
for. On the depot, that is the whole command:

```sh
beb-depot authorize laptop.handover
```

Nobody types a fingerprint and nobody types a queue name. Then mail
moves:

```console
$ beb-courier sync
beb-courier: 1 sent, 1 received
```

`sync` finishes: push what is waiting, take what is waiting, exit. It
is the shape for a timer or an agent's turn boundary. Exit 2 when there
was nothing either way, 3 when the depot refused a frame, which leaves
that frame in the outbox and says which.

For mail that lands the moment it is sent, hold a connection open:

```sh
beb-courier listen
```

A depot cannot dial a client behind NAT, so the client dials out and
lets the far side block inside the connection. Without `listen` mail
still arrives at the next `sync`; what is lost is only the promptness.
`beb-courier unit` prints a systemd user unit that keeps it standing.

## Commands

```console
$ beb-courier
beb-courier carries beb's mail to and from a depot.

  beb-courier init DEPOT
      mint this machine's courier key and name the depot it uses
  beb-courier whoami
      this machine's key and the queues it reads for, for the operator
  beb-courier sync
      push what is waiting to leave, pull what is waiting to arrive
  beb-courier listen
      hold a connection open so arrivals land as they happen
  beb-courier unit
      a systemd unit that keeps listen standing

  beb-courier --help
  beb-courier --version

Exit: 0 did it, 1 change the command, 2 nothing to do, 3 refused.

A DEPOT is ssh://[user@]host[:port] -- a scheme, so that a second way of
reaching one has somewhere to be named later.

BEB_COURIER_ROOT holds the key, the depot name, and an ssh_config if
this machine needs one. It defaults to ~/.local/share/beb-courier:

  id_ed25519    this courier's key, minted by init
  depot         one line, the depot it carries to
  ssh_config    optional, and used only if it is there

BEB_BIN names the beb to hand mail to, and defaults to what is on PATH.
```

`ssh_config` is there because ssh expands `~` from the password
database and not from `HOME`, so a courier running as a daemon user has
no other way to be told the depot's host key. Put `UserKnownHostsFile`
in it, or a `ProxyJump`. Leaving it out means "use this account's
ordinary ssh setup", which is right when the courier runs as a person.

It will not pass `StrictHostKeyChecking=no`. That accepts any machine
answering on the depot's address, and host authentication is the one
thing ssh does here that nothing else does.

## Design

Nothing is removed until the other end has it, in either direction, so
a courier that dies mid-transfer loses nothing. It counts no attempts
and records nothing in flight: exactly-once is not its business,
because the beb on the receiving end deduplicates whatever it is handed
twice. Two couriers for one identity are therefore safe.

The rule is that a courier reads the outbox and calls `drop`, and
touches nothing else. It computes no path into a mailbox, decides who
lives where, or opens an envelope. Both times a transport reached past
that line it drifted, and both times it broke silently the next time
beb changed.

[DESIGN.md](DESIGN.md) has the custody rules, the seam with beb, and
what a courier refuses to know.

## License

MIT
