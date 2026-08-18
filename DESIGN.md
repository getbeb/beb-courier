# beb-courier

Moves beb's mail between machines, and knows nothing about mail.

## Why it exists

beb owns no network. It signs a message, puts it in a mailbox if the
recipient reads here, and puts it in an outbox if they do not. What
happens to the outbox is somebody else's problem, and this is that
somebody.

## What it moves

Frames. Opaque ones. A courier never parses an envelope and never
verifies a signature: what is waiting to leave is a directory, and the
name of each file says both things a courier needs to know.

    ~/.local/share/beb/outbox/000000000000000001-d811f21767d40b…756e
                              └─ order ─────────┘ └─ who it is for ┘

So the outbound half is four lines and no beb process:

    for f in "$SPOOL"/outbox/0*; do
        n=${f##*/}; to=${n#*-}
        ship "$f" "$(route "$to")" && rm -f "$f"
    done

There were two beb verbs here until the outbox learned to name its
recipients -- one to hand a frame over, one to remove it afterwards --
and both existed only because the address lived inside the frame,
which is the one place a courier must not look.

The rule, and it is the only one that matters: **a courier reads the
outbox and calls `drop`, and touches nothing else.** It does not
compute a path into a mailbox, does not decide who lives here, does
not open an envelope. Both times a transport reached past that line it
drifted -- once on how a mailbox is named, once on what counts as
living here -- and both times it broke silently the next time beb
changed.

This document used to say a courier never asks beb anything, which was
a slogan and not the rule, since it already calls `drop` and already
reads the mailbox directory names beside the outbox. The rule is
**a courier hands beb bytes and never asks it questions.** `drop` is a
hand-off: bytes in, beb's own key and beb's own rules do the work, an
exit code out. Anything that ever joins it has to be the same shape.
What a courier must not do is ask beb who lives here, where a mailbox
is, or what an address means. Those it reads from the spool's shape, or
it is told them, and being told is why the handover exists.

The distinction earns its keep the moment beb can sign for a caller.
Handing beb something to sign is `drop` again. Asking beb which
identity `BEB_IDENTITY` currently names is a question about beb's
world, and a courier that starts asking those is a courier that has an
opinion to be wrong about later.

The seam is a place going out and a verb coming in, and that asymmetry
is the point rather than an inconsistency. Outbound has no rules: flat,
complete frames, already signed and addressed. Inbound has all of them
-- residency, deduplication, the counter, the cursor -- and a courier
must not know any.

`route` in that sketch was a constant for as long as there was one
place to carry to. It is a table now, and the next two sections are
what is in it.

## Where mail goes

The filename says who a frame is for. A table says where that
recipient is:

    ssh://depot.internal
    d811f21767d40b61…0fe756e ssh://bob.lan   # bob

Keyed by address, because the address is what beb wrote into the
outbox and a name is not. A courier that looked names up would have to
read `known_signers`, which is beb's roster and would be the third
time a transport reached across this seam; the first two broke
silently the next time beb changed. What follows `#` is for whoever
opens the file and is never matched on.

Two levels of specificity and no more: an exact route wins, and the
line with no address takes the rest.

**The default is spelled by leaving the address off**, and not by a
word standing where addresses stand. A word there would have nothing
but its shape to be told apart by, and shape is not something a name
has to respect: nothing stops a name being sixty-four hex characters.
A place always begins `ssh://` and an address never can, so the two
columns are disjoint by construction rather than by luck. It also has
to survive a shell, and a token that must be quoted to mean itself is
a token that will one day be a filename instead.

A depot is not a second concept here. It is the route that names no
address, which is what it always was -- one place, for everything --
and it stopped needing a file of its own the moment there was a table
to hold it.

**An exact route that fails does not fall back.** The frame stays in
the outbox and the pass says which frame and which host. The other
order delivers the mail by the other path and nobody learns the link
is dead, which is how the long-lived verb spent its first version
moving one direction while nothing reported a fault.

`route rm` takes an address, or a place and everything pointed at it.
Two forms because the line with no address has no address to be named
by, and because the case an operator actually has is a machine that
died rather than one recipient who moved.

## Two kinds of place, one wire

A route names a place and does not record what answers there. It
cannot: sshd discards the command a client sends and runs the forced
command from `authorized_keys`, so the far side decides what it is on
every connection, and a courier that wrote down an opinion would be
writing down a guess.

    command="'/usr/local/bin/beb-depot' serve --root '/srv/beb' SHA256:abc…",restrict
    command="'/usr/local/bin/beb' drop",restrict

The first shelves the frame for a courier that will come asking. The
second installs it, because `beb drop` resolves no identity, takes the
recipient from the frame, and refuses a mailbox that is not there. So
the receiving half of a direct link is not code in this program. It is
a line in a file, and beb had already written it.

Two things are absent from the second on purpose. **No fingerprint:** a
depot bakes the caller's in because it has to decide what that caller
may collect, and `beb drop` decides nothing about the caller. **No
root:** a depot is passed one because a forced command inherits none of
the operator's environment, while sshd does set `HOME`, which is where
beb's spool comes from. A machine holding its spool under
`XDG_DATA_HOME` is the exception, and `authorize` writes the `env`
prefix for it, resolved here, the way it resolves the beb.

## Two directions, and they are not alike

**Outbound needs nothing running, in the sense that it needs no
connection held open.** The outbox only fills because an agent sent
something, so `sync` can push it at whatever moment the caller finds
convenient -- for an agent harness, a turn boundary.

What that argument got wrong was "so the sender is already there". The
sender is there; the sender is an agent, and no agent calls `sync`.
The first machine to run `carry` as a service demonstrated it: mail
was written, the courier was running, and nothing moved until a person
looked in the outbox. So `carry` goes both ways, and `sync` remains
for the caller who wants one pass and an exit code.

**Inbound needs something running**, and this is the whole reason a
daemon exists at all. A depot cannot open a connection to a client, so
a client that wants to be *woken* has to hold one open and let the far
side block inside it. That is `carry`.

It was called `listen` until 0.5.0, and the name was wrong twice over.
Nothing here listens: this dials outwards and the far side blocks
inside the connection, which is the constraint the whole design turns
on. And it named the half that pulls, which is the half that was the
only half for three versions while mail sat in an outbox with nothing
reporting a fault. `carry` is the program's own word and covers both
directions. `wait` was refused because `beb wait` is a different shape
in the program sitting next to this one: it returns.

Without `carry` mail still arrives, at the next `sync`. What is lost
is the wake: an idle session finds its mail when it next looks rather
than the moment it lands. Which is why `sync` pulls as well as pushes,
and why `carry` is a thing you add rather than a thing you need.

**Only a place that shelves has anything to be collected from**, so
the inbound half asks the address-less route and no other. A peer holds
nothing: it was written into directly, and the wake it gives is better
than a pickup, since the sender's ssh and the recipient's `beb wait`
are the same moment. A machine whose routes are all exact therefore
runs one half of `carry`, which is the correct amount, and mail still
lands there the instant somebody sends it.

**Collection is singular, and not because a machine may have one
depot.** It is because a courier cannot derive where mail for it is
left: that is decided by the tables of whoever sends to it, and no
place can be asked whether it shelves, since a peer's forced command
would read the question as a frame. So it has to be told, and the
address-less route tells it by convention.

Those are two different facts sharing one line, and they come apart in
a case that is not exotic: send only to peers, collect through somebody
else's depot, and that depot has to be named the default in order to be
collected from, after which every address with no route of its own goes
there quietly instead of being refused.

When that has to be unloaded it is a mark on that line, and not a
second file. Two files would hold one hostname twice, and two places
holding one fact is the shape of both outages this project has had: a
unit naming one beb while the shell named another, and a depot's root
in the environment disagreeing with the one in the forced command. The
mark is also something `register` would write rather than an operator,
since "I collect there" is the record of an act and `register` is that
act. It is not built, and with one collection point the mark would have
one legal value, which is a knob.

## Verbs

    beb-courier init          mint this machine's courier key
    beb-courier whoami        everything the far side needs from this
                              machine, and nothing else
    beb-courier route         every route, and whether the outbox
                              matches them
    beb-courier route add     where a recipient's mail goes; an
                              address or a handover file, and with none
                              of either, where everything else goes
    beb-courier route rm      stop routing one address, or one place
    beb-courier authorize     let that courier drop here
    beb-courier register      say an identity reads here, in its own
                              signature; the address comes from stdin
    beb-courier unregister    give up this machine's claim on one address
    beb-courier sync          push the outbox, drain what shelves for
                              here, once
    beb-courier carry         the same, until stopped; the only
                              long-lived verb
    beb-courier unit          print the unit that keeps carry standing

`init` takes no argument. It mints a key and nothing else, because
where mail goes is a table now and a table is added to rather than
founded.

## Config

Topology, not knobs. Where the recipients are, and how to reach them,
in `$BEB_COURIER_ROOT`:

    id_ed25519    this machine's courier key
    routes        one line per place; an address and where it goes,
                  or a place alone for everything else
    ssh_config    optional, and used only if it is there

A scheme on every place, so that a second way of reaching one has
somewhere to be named without anything being renamed. What answers
there is indifferent to which scheme carried the bytes.

`depot`, one line naming one place, is what `routes` grew out of. A
root still holding one is refused and told the single line to write,
rather than read as though it were the address-less line: no machine
should carry mail by a path its own `route` output does not show.

`ssh_config` is the rest of the topology rather than a knob. ssh
expands `~` from the password database and not from HOME, so a courier
running as a daemon user has no other way to be told a place's host
key -- and the alternative, passing StrictHostKeyChecking=no, would
throw away the one thing ssh is doing between here and there.
Absent means "use this account's ordinary ssh setup", which is right
when a courier runs as a person.

## Custody

    outbound   read the outbox -> ship -> the place accepts -> unlink
    inbound    the depot streams -> beb drop -> ack -> the depot deletes

Nothing is removed until the other end has it, in either direction, so
a courier that dies mid-transfer loses nothing: the file is still where
it was, and the next attempt finds it.

Nothing here counts attempts or records what is in flight. Retrying is
the courier's business only in the sense that it is the one still
holding the thing; exactly-once is not its business at all, because
the beb on the receiving end deduplicates whatever it is handed.

Which also means two couriers for one identity are safe: both pull the
same frame, both hand it to beb, and the second is told it was already
delivered. Coordination between them is an optimisation, not a
correctness requirement.

## When a place is down

Until there were two places, "this pass failed" and "that host is
down" were the same sentence, and the backoff `carry` already had was
per destination by accident. A second route makes continuing past a
dead host mandatory, since bob asleep must not hold pve's mail, and
continuing returns success from the pass and resets that backoff to a
quarter second.

So the wait is held per destination, and a destination inside its wait
is skipped rather than dialled. Skipping is the load-bearing half:
`push` is one sequential loop, so a pass that merely tried and failed
would sit through one host's connect timeout before reaching the
others, and one sleeping laptop would stall the whole outbox behind it.

This is not a retry count and nothing is recorded as in flight. The
wait says when to look again; it never says to stop, because the frame
is still here and the recipient still exists.

The table is read at the top of each pass and never held. A daemon
carrying the copy it read at start is the fault where a unit and a
shell name different bebs, which is the outage that actually happened
here, and a small file costs nothing against the handshake it is
deciding about.

## What whoami prints

Setting up a depot means moving two facts from a client machine to a
machine it was built to be unable to reach: which key will be calling,
and which addresses it collects for. Both live here, so one command hands
over both and the operator pastes the result:

    $ beb-courier whoami > laptop.handover
    beb-courier: 1 key, 2 addresses this machine reads for
    beb-courier: give this to whoever runs the depot: beb-depot authorize <this file>

    $ cat laptop.handover
    ssh-ed25519 AAAA… beb-courier@laptop
    bb68ed0016fd16b5b04cd295b0433c3a54e15f34dcf898ca248dfb34dfa446f0
    25157d6074b94409be182632c8860c65d91ebb6a6d35a6561847975a095de02e

The key first, the addresses after, prose on stderr so the file is only
ever the handover. `beb-depot authorize` reads exactly that shape, so
the operator names nothing at all.

It prints the *key*, not the `authorized_keys` line, and the difference
is not cosmetic. That line has to name the depot's own binary and its
own root, neither of which this machine knows or should guess -- a
courier that guessed would write a line that looked right and pointed
nowhere. The depot builds it, from this file, because the depot is the
only side that knows where it lives. An earlier draft of this document
had `whoami` printing the whole line; implementing it is what showed
that it cannot.

The recipient list is not a thing this program knows. It is a directory
read: beb names each mailbox for the identity's key, so the spool holds
exactly the addresses a depot shelves under, and the only work is dropping
`outbox`, which sits beside them. Sending to a stranger never invents a
mailbox and a mailbox appears at `beb init`, so what is left is
precisely who reads here, at the moment somebody first asks.

Which is why this lives here rather than in beb. The fact is beb's; the
*use* is a depot's, and beb knows nothing about depots.

## Setting up a link

`beb-depot authorize` takes one handover and uses both halves of it:
the key, to let that courier connect, and the addresses, to know what it
may collect. A peer hands nothing back, so nobody there needs both
halves, and the two halves land on different machines:

    alice$ beb-courier route add bob.handover ssh://bob.lan
    bob$   beb-courier authorize alice.handover

`route add` reads the addresses and ignores the key. `authorize` reads
the key and ignores them. Both sides run both verbs against the
file the other one sent, and then the link carries both ways.

Four commands where a depot takes one, and that is the honest price of
having no hub: a depot is a machine both sides already agreed on, and
two peers have to agree to each other's face.

They stay two verbs rather than one `peer` that does both. They write
to different files with different blast radius, one of them being
`~/.ssh/authorized_keys`, and a one-way link is a real topology: an
agent that only ever reports needs `route add` here and `authorize`
there, and nothing else.

`route add` also takes a bare address, and a place on its own for the
line that names no address, which has no handover to read.

## Being allowed to collect

A depot will not hand mail to a courier that has not been granted the
recipient, and granting is the operator's, at the depot, in one
`beb-depot authorize` against the file `whoami` printed. So a courier
has nothing to do here beyond handing that file over.

That was the whole of it until `register`, and what made it wear was
frequency: the key crosses once, but the list of who reads here goes
stale on every `beb init`, and re-declaring it meant a round trip to a
machine this one was built to be unable to reach. The bootstrap was
being paid again for every identity.

**A handover crosses because it must; after that the two machines can
talk.** So the identity says so itself, over the connection there
already is:

    BEB_IDENTITY=~/newthing beb whoami | beb-courier register

The address arrives on stdin because a courier does not ask beb who it
is. This composes the one sentence the depot has to believe -- that
address authorises this fingerprint -- and hands it to `beb sign` to be
signed, which is `drop` again in the other direction: bytes in, beb's
key does the work, bytes out.

Three things make it safe, and the third is the one that matters. The
signature verifies against the key in the claim, with no trust store
anywhere: the depot builds a one-line `allowed_signers` from the thing
it is checking, the way beb verifies an envelope. The fingerprint in
the claim must be the one sshd says is calling, so an intercepted claim
is useless to anybody else. And **the queue granted is derived from the
signed key rather than named beside it**, so a courier cannot present a
valid claim and ask for a different identity's mail.

The namespace is its own, `beb-collect`, which is what stops an
envelope's signature being presented as a claim on a queue.

`unregister` needs no signature at all, and the asymmetry is the point.
Adding a claim asserts something about an identity; dropping one asserts
nothing, since sshd already said who is calling and a courier can only
ever remove its own line. The worst it can do is stop its own mail.

It names the address rather than working out what is missing. A machine
with `XDG_DATA_HOME` unset reads for nothing at all, and a courier that
gave up whatever it could not see would revoke a live identity the first
time an environment went wrong. Grants are added and dropped by saying
so, never by inference.

What none of this reaches is the machine that never comes back, which
cannot call in to give anything up. That grant is the operator's to
remove, with `beb-depot revoke`, at the depot.

A peer has no grant table to add to, and needs none. `beb drop`
installs a frame for a mailbox that is here and refuses one that is
not, so the whole of the permission is the `authorized_keys` line, and
sshd enforces it before this program is reached. There is nothing for
a check on the far side to ask that admission does not already answer.

## What a routing table must never become

Two rules, cheap to hold now and expensive to recover later.

**A route is told, never learned.** Every famous routing failure is a
route believed from somebody else: the hijack, the leak, the
withdrawal that took a company off the internet for a day. This table
is hand-written and local, so there is nothing to believe and no third
party who can move somebody's mail. Distributing routes the way the
roster wants to be distributed would import that entire catalogue in
one commit.

**A courier is an edge, never a transit.** `beb drop` installs or
refuses and never re-emits, so a frame has no second hop and needs no
hop count to bound it. The moment anything forwards, the first loop
runs until a disk fills.

What the table cannot do is misdeliver, which is the hazard it would
have if this were a network. An address is an identity and not a
position, so a frame sent to the wrong host is refused there rather
than delivered to whoever happens to be standing at that address. It
is still handed to that host as bytes, which is what ssh host
verification is for, and the reason `StrictHostKeyChecking=no` stays
refused.

The one hazard with no answer here is narrower than it first looks. A
depot refuses a drop for a recipient nobody may collect for, so a route
to the wrong depot fails loudly and the frame stays. What no side can
see is a grant whose owner stopped coming: the depot accepts, custody
transfers, the outbox empties, and the frames wait in a directory with
no reader. A peer route cannot do this, since `beb drop` either
installs or refuses. `beb-depot held` is where it shows, and that is a
machine the sender may not operate.

## What it never does

Own a queue format, keep a shelf, count retries, resolve a name, read a
message, or ask beb a question. The wait it holds per destination is
not a count: it says when to look again and never says to give up.

It reads two things under beb's spool and that is the whole of the
contract between them: the outbox, a flat directory of `<id>-<address>`
where each file is a complete frame, and the mailbox directory names
sitting beside it, which are the addresses this machine reads for. Both
are the spool's shape rather than its contents. If anything beyond that
starts to look necessary, the answer is upstream.
