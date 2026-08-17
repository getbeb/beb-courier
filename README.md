# beb-courier

Moves [beb](https://github.com/getbeb/beb)'s mail between machines, and knows
nothing about mail.

beb owns no network. It signs a message, puts it in a mailbox if the recipient
reads on this machine, and puts it in an outbox if they do not. What happens to
the outbox is somebody else's problem, and this is that somebody.

It never parses an envelope, never verifies a signature, and never computes a
path into a mailbox. What is waiting to leave is a directory whose filenames
carry the address; what arrives goes to `beb drop` and nowhere else.

## Install

On each machine where agents read and write mail:

```
cargo build --release
install -m755 target/release/beb-courier ~/.local/bin/beb-courier
```

It needs `beb` and `ssh`, and a [beb-depot](https://github.com/getbeb/beb-depot)
to carry to.

## Setting one up

One key for the machine, not one per identity, and the depot it carries to:

```
$ beb-courier init ssh://depot.internal
beb-courier: courier key in ~/.local/share/beb-courier/id_ed25519
beb-courier: depot depot.internal in ~/.local/share/beb-courier/depot
beb-courier: beb-courier whoami prints what the depot operator needs
```

Then hand the depot operator everything they need, in one file:

```
$ beb-courier whoami > laptop.handover
beb-courier: 1 key, 2 queues this machine reads for
beb-courier: give this to whoever runs the depot: beb-depot authorize <this file>
```

The file holds the courier's public key, then the queue names this machine
reads for, one per line. On the depot, that is the whole command:

```
beb-depot authorize laptop.handover
```

Nobody types a queue name and nobody types a fingerprint. The depot derives the
fingerprint from the key and reads the queues out of the same file.

The queue names are not something this program computes. beb names each mailbox
for the identity's key, so the spool already holds them; the only work is
ignoring `outbox`, which sits beside them. An identity appears the moment
`beb init` makes it, so `whoami` is right as soon as there is anything to say.

## Using it

```
$ beb-courier sync
beb-courier: 2 sent, 1 received
```

Push what is waiting to leave, take what is waiting to arrive, exit. Nothing
needs to be running for the outbound half to have work, because the outbox only
fills when somebody sends something, so the sender was already here. For an
agent harness, a turn boundary is the natural moment.

Exit 2 when there was nothing in either direction, and exit 3 when the depot
refused a frame, which leaves that frame in the outbox and says which.

```
$ beb-courier listen
beb-courier: holding a connection to depot.internal
```

The wake. A depot cannot open a connection to a client behind NAT, so a client
that wants mail the moment it lands holds one open and lets the far side block
inside it. Without `listen`, mail still arrives at the next `sync`; what is
lost is only the promptness.

`beb-courier unit` prints a systemd user unit that keeps `listen` standing. It
prints it rather than installing it, because where a unit belongs is the
operator's business.

## Custody

```
outbound   read the outbox -> ship -> the depot accepts -> unlink
inbound    the depot streams -> beb drop -> ack -> the depot deletes
```

Nothing is removed until the other end has it, in either direction, so a
courier that dies mid-transfer loses nothing. Nothing here counts attempts or
records what is in flight: exactly-once is not this program's business, because
the beb on the receiving end deduplicates whatever it is handed twice. Two
couriers for one identity are therefore safe.

## Configuration

`BEB_COURIER_ROOT`, which defaults to `~/.local/share/beb-courier`:

```
id_ed25519    this machine's courier key, minted by init
depot         one line, ssh://[user@]host[:port]
ssh_config    optional, and used only if it is there
```

Topology, not knobs. To point a machine at a different depot, edit `depot`.

`ssh_config` exists because ssh expands `~` from the password database rather
than from `HOME`, so a courier running as a daemon user has no other way to be
told the depot's host key. Put `UserKnownHostsFile` there, or a `ProxyJump`, or
whichever user the depot expects. Leaving it out means "use this account's
ordinary ssh setup", which is right when the courier runs as a person.

The one thing it will not do is pass `StrictHostKeyChecking=no`. That would
accept any machine answering on the depot's address, and host authentication is
the one thing ssh is doing here that nothing else does.

`BEB_BIN` names the beb to hand mail to, if it is not on `PATH`.

## Tests

```
cargo test --release
```

One suite, all shell, and nothing stubbed: a real beb, a real depot, a real
sshd on a loopback port. A courier is made entirely of the seams between other
programs, and what is left after removing those is a directory read and a
subprocess. Skips where any piece is missing.

## The rule

**A courier reads the outbox and calls `drop`, and touches nothing else.** It
does not compute a path into a mailbox, does not decide who lives here, does
not open an envelope. Both times a transport reached past that line it drifted,
once on how a mailbox is named and once on what counts as living here, and both
times it broke silently the next time beb changed.

See [DESIGN.md](DESIGN.md) for why it is shaped this way.

## License

MIT
