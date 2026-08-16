# beb-courier

Moves beb's mail between machines, and knows nothing about mail.

## Why it exists

beb owns no network. It signs a message, puts it in a mailbox if the
recipient reads here, and puts it in an outbox if they do not. What
happens to the outbox is somebody else's problem, and this is that
somebody.

## What it moves

Frames. Opaque ones. A courier never parses an envelope, never
verifies a signature, and never opens the spool: it asks beb for the
next thing to send and beb answers with the bytes *and* the address
they are for.

    $ beb pickup
    beb: outbound 1 for backend; 550 bytes; beb rm 1 once it has landed
    beb: to ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAA...
    <the frame, on stdout>

That second line is why nothing here needs a parser. The one before it
is why nothing here needs to know where a mailbox lives.

The rule, and it is the only one that matters: **a courier may only
touch beb's plumbing.** If it needs something the plumbing does not
expose, that is a missing verb in beb, not a path to open under the
spool. Both times a transport reached into beb's storage instead, it
drifted -- once on how a mailbox is named, once on what counts as
living here -- and both times it broke silently the next time beb
changed.

## Two directions, and they are not alike

**Outbound needs nothing running.** The outbox only fills because an
agent sent something, so the sender is already there. `sync` pushes
what is waiting, at whatever moment the caller finds convenient -- for
an agent harness, a turn boundary.

**Inbound needs something running**, and this is the whole reason a
daemon exists at all. A depot cannot open a connection to a client, so
a client that wants to be *woken* has to hold one open and let the far
side block inside it. That is `listen`.

Without `listen` mail still arrives, at the next `sync`. What is lost
is the wake: an idle session finds its mail when it next looks rather
than the moment it lands. Which is why `sync` pulls as well as pushes,
and why `listen` is a thing you add rather than a thing you need.

## Verbs

    beb-courier init          mint this machine's courier key
    beb-courier whoami        the authorized_keys line for a depot
    beb-courier register      claim queues at the depot, signed by
                              each identity that lives here
    beb-courier sync          push the outbox, pull the inbox, once
    beb-courier listen        hold a connection so arrivals wake a
                              session; the only long-lived verb
    beb-courier unit          print the unit that keeps listen standing

## Config

Topology, not knobs. Which depot, and how to reach it:

    depot = ssh://depot.internal

A scheme, so that a second way of reaching a depot has somewhere to be
named without anything being renamed. The depot itself is indifferent
to which one carried the bytes.

## Custody

    outbound   beb pickup -> ship -> the depot accepts -> beb rm <id>
    inbound    the depot streams -> beb drop -> ack -> the depot deletes

`pickup` does not remove and the depot does not delete until told, so
a courier that dies mid-transfer loses nothing: the frame is still
where it was, and the next attempt finds it.

Nothing here counts attempts or records what is in flight. Retrying is
the courier's business only in the sense that it is the one still
holding the thing; exactly-once is not its business at all, because
the beb on the receiving end deduplicates whatever it is handed.

Which also means two couriers for one identity are safe: both pull the
same frame, both hand it to beb, and the second is told it was already
delivered. Coordination between them is an optimisation, not a
correctness requirement.

## Registration

A depot will not hand mail to a courier that has not claimed the
recipient, and a claim is signed by the identity itself -- so a
courier cannot claim a queue it has no key for. The claim binds the
identity key, the courier key, the depot it is addressed to, the
operation, a nonce and an expiry, because a claim that binds less than
that is a claim that can be replayed somewhere else, later.

Adding an identity to a machine means one more claim. It does not mean
touching the depot's `authorized_keys`, which names the courier and
nothing else.

## What it never does

Own a queue format, keep a shelf, count retries, compute a path under
beb's spool, or read a message. If any of those start to look
necessary, the answer is upstream.
