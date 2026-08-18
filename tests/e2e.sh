#!/usr/bin/env bash
# A courier between a real beb and a real depot, over a real sshd.
#
#   BEB_COURIER_BIN=target/release/beb-courier \
#   BEB_DEPOT_BIN=../beb-depot/target/release/beb-depot \
#   BEB_BIN=../beb/target/release/beb bash tests/e2e.sh
#
# Nothing here is stubbed, because a courier is entirely made of the
# seams between other programs: beb's outbox, the depot's wire, and
# sshd's environment. What is left after removing those is a directory
# read and a subprocess, and neither is what breaks.
#
# Skips rather than fails where a piece is missing; that is a property of
# the machine, not of the courier.
set -u

C=${BEB_COURIER_BIN:-beb-courier}
D=${BEB_DEPOT_BIN:-beb-depot}
BEB=${BEB_BIN:-beb}
for v in C D BEB; do
    p=${!v}
    case "$p" in /*) ;; */*) eval "$v=\$PWD/\$p" ;; esac
done
command -v "$BEB" >/dev/null 2>&1 || { echo "skip - no beb"; exit 0; }
command -v "$D" >/dev/null 2>&1 || { echo "skip - no beb-depot"; exit 0; }
SSHD=""
for c in /usr/sbin/sshd /usr/local/sbin/sshd; do test -x "$c" && { SSHD=$c; break; }; done
test -n "$SSHD" || { echo "skip - no sshd"; exit 0; }

n=0
ok() { n=$((n + 1)); echo "ok $n - $1"; }
die() { echo "not ok - $1"; exit 1; }

W=$(mktemp -d)
PID=""
cleanup() { test -n "$PID" && kill "$PID" 2>/dev/null; rm -rf "$W"; }
trap cleanup EXIT

# --- the depot machine ---------------------------------------------------

export BEB_DEPOT_ROOT=$W/srv BEB_DEPOT_AUTHORIZED_KEYS=$W/authorized_keys
ssh-keygen -q -t ed25519 -N '' -f "$W/hostkey" || die "host key"
PORT=0
for _ in 1 2 3 4 5; do
    p=$((20000 + RANDOM % 20000))
    cat > "$W/sshd_config" <<EOF
Port $p
ListenAddress 127.0.0.1
HostKey $W/hostkey
AuthorizedKeysFile $W/authorized_keys
PasswordAuthentication no
UsePAM no
StrictModes no
PidFile $W/sshd.pid
EOF
    "$SSHD" -f "$W/sshd_config" -E "$W/sshd.log" 2>/dev/null
    sleep 1
    if [ -f "$W/sshd.pid" ]; then PORT=$p; PID=$(cat "$W/sshd.pid"); break; fi
done
test "$PORT" -ne 0 || { echo "skip - could not start an sshd"; exit 0; }

# The depot's host key, accepted once, as an operator would. It goes in
# the courier's own ssh_config, never in the user's ~/.ssh: ssh expands
# ~ from the password database, so HOME cannot move it out of the way
# and a test that wrote there would be editing real files.
mkdir -p "$W/courier"
printf '[127.0.0.1]:%s %s\n' "$PORT" "$(cut -d" " -f1,2 "$W/hostkey.pub")" \
    > "$W/known_hosts"
printf 'UserKnownHostsFile %s\n' "$W/known_hosts" > "$W/courier/ssh_config"

# --- bob's machine: an identity, and a courier ---------------------------

mkdir -p "$W/bob" "$W/alice"
export BEB_COURIER_ROOT=$W/courier
bob() { env XDG_DATA_HOME=$W/bob/data XDG_CONFIG_HOME=$W/bob/cfg BEB_IDENTITY=$W/bob "$BEB" "$@"; }
alice() { env XDG_DATA_HOME=$W/alice/data XDG_CONFIG_HOME=$W/alice/cfg BEB_IDENTITY=$W/alice "$BEB" "$@"; }
courier() { env XDG_DATA_HOME=$W/bob/data BEB_BIN=$BEB "$C" "$@"; }

(cd "$W/bob" && env XDG_DATA_HOME=$W/bob/data XDG_CONFIG_HOME=$W/bob/cfg "$BEB" init bob) >/dev/null 2>&1
(cd "$W/alice" && env XDG_DATA_HOME=$W/alice/data XDG_CONFIG_HOME=$W/alice/cfg "$BEB" init alice) >/dev/null 2>&1
bob contacts 2>/dev/null >> "$W/alice/cfg/beb/known_signers"
alice contacts 2>/dev/null >> "$W/bob/cfg/beb/known_signers"

"$C" whoami >"$W/out" 2>"$W/err" && die "whoami worked with no key"
grep -q 'beb-courier init' "$W/err" || die "whoami with no key: $(cat "$W/err")"
ok "a courier with no key says how to make one"

courier init "ssh://127.0.0.1:$PORT" >/dev/null 2>"$W/err" && die "init took a place"
grep -q 'takes nothing' "$W/err" || die "init refusal: $(cat "$W/err")"
courier init >/dev/null 2>"$W/err" || die "init: $(cat "$W/err")"
test -f "$BEB_COURIER_ROOT/id_ed25519.pub" || die "no key was minted"
ok "init mints one key for the machine, and where mail goes is not its business"

courier sync >/dev/null 2>"$W/err" && die "a courier with no routes carried something"
grep -q 'route add ssh://' "$W/err" || die "no routes: $(cat "$W/err")"
courier route add "not-a-url" >/dev/null 2>"$W/err" && die "a bare hostname was accepted"
grep -q 'ssh://' "$W/err" || die "place refusal: $(cat "$W/err")"
courier route add "ssh://127.0.0.1:$PORT" >/dev/null 2>"$W/err" || die "route add: $(cat "$W/err")"
grep -q "^ssh://127.0.0.1:$PORT" "$BEB_COURIER_ROOT/routes" || die "the place was not recorded"
# A place alone, and the first field is an address on every other line,
# so nothing in that column has to be told apart by its shape.
ok "route add with no address names where everything else goes, which is what a depot is"

# The comment is how an operator tells one authorized_keys line from the
# next, and it came out "beb-courier@unknown" on both machines of the
# first deployment.
comment=$(awk '{print $3}' "$BEB_COURIER_ROOT/id_ed25519.pub")
case "$comment" in
    beb-courier@?*) ;;
    *) die "the key comment is \"$comment\"" ;;
esac
if command -v hostname >/dev/null 2>&1 && [ -n "$(hostname 2>/dev/null)" ]; then
    test "$comment" != "beb-courier@unknown" ||
        die "the key is labelled unknown on a machine that knows its name"
fi
ok "the key is labelled with this machine's name, so a line names a courier"

courier init >/dev/null 2>"$W/err" && die "init overwrote an existing key"
grep -q 'already has a courier key' "$W/err" || die "second init: $(cat "$W/err")"
ok "a second init is refused, because a new key is one nowhere has heard of"

courier route add "ssh://elsewhere" >/dev/null 2>"$W/err" && die "the rest was routed twice"
grep -q 'route rm' "$W/err" || die "a second default: $(cat "$W/err")"
ok "and a route is never silently repointed; rm says so out loud first"

# --- the handover --------------------------------------------------------

courier whoami >"$W/handover" 2>"$W/err" || die "whoami: $(cat "$W/err")"
head -1 "$W/handover" | grep -q '^ssh-ed25519 ' || die "no key on line 1: $(cat "$W/handover")"
test "$(tail -n +2 "$W/handover" | wc -l | tr -d ' ')" -eq 1 || die "wrong address count"
BOBADDR=$(tail -n +2 "$W/handover")
test -d "$W/bob/data/beb/$BOBADDR" || die "the address named is not a mailbox here"
grep -q '1 key, 1 addresses' "$W/err" || die "whoami summary: $(cat "$W/err")"
ok "whoami prints the key and the addresses this machine reads for, and nothing else"

# Two acts, on the depot's two axes: the key decides who connects, the
# queue is granted separately. bob's own courier would register it, and
# does later in this file; here it is the operator, because what is
# being set up is the transport rather than the grant.
"$D" authorize "$W/handover" >/dev/null 2>"$W/err" || die "authorize: $(cat "$W/err")"
grep -q 'collects for nothing yet' "$W/err" || die "authorize granted something: $(cat "$W/err")"
CFP0=$(ssh-keygen -lf "$BEB_COURIER_ROOT/id_ed25519.pub" | awk '{print $2}')
"$D" grant "$CFP0" "$BOBADDR" >/dev/null 2>"$W/err" || die "grant: $(cat "$W/err")"
grep -q "may now collect for $BOBADDR" "$W/err" || die "grant ack: $(cat "$W/err")"
ok "the depot takes the key from that file, and a queue is the other verb"

# --- status ---------------------------------------------------------------
#
# The one outage here was the unit and the shell disagreeing about which
# beb to use, and each looked healthy alone. The depot is probed with an
# intent it refuses: a refusal proves the host is reachable, the key
# authenticates, and sshd ran the forced command -- and it moves no mail.

# HOME moves for these: status looks for the supervisor file where this
# platform keeps them, and that is the operator's real launchd agent or
# systemd unit. A test must not read those, let alone judge them.
mkdir -p "$W/home"
sstatus() { env HOME=$W/home XDG_DATA_HOME=$W/bob/data BEB_BIN=$BEB "$C" status "$@"; }

sstatus >/dev/null 2>"$W/err"; rc=$?
test "$rc" -eq 0 || die "status on a healthy courier exited $rc: $(cat "$W/err" | tail -3)"
grep -q 'answers and knows this key' "$W/err" || die "status did not probe the depot: $(cat "$W/err")"
grep -q '1 addresses' "$W/err" || die "status counts no addresses: $(cat "$W/err")"
grep -qE 'beb is beb [0-9]' "$W/err" || die "status does not name the beb it would use: $(cat "$W/err")"
ok "status reports the courier, and a refused intent proves the depot answers"

env HOME=$W/home XDG_DATA_HOME=$W/bob/data BEB_BIN=/nonexistent/beb "$C" status >/dev/null 2>"$W/err" &&
    die "status passed a beb that cannot run"
grep -q 'does not answer --version' "$W/err" || die "missing beb unreported: $(cat "$W/err")"
ok "a beb that cannot answer is caught, since nothing could be delivered"

ROUTES_KEEP=$(cat "$BEB_COURIER_ROOT/routes")
echo "ssh://127.0.0.1:1" > "$BEB_COURIER_ROOT/routes"
sstatus >/dev/null 2>"$W/err" && die "status passed an unreachable place"
grep -q 'cannot reach' "$W/err" || die "unreachable place unreported: $(cat "$W/err")"
printf '%s\n' "$ROUTES_KEEP" > "$BEB_COURIER_ROOT/routes"
ok "a place that cannot be reached is caught, and says so as itself"

# A courier upgraded past the table meets its own depot file, and the
# refusal has to carry the whole of the fix: this is the only thing four
# deployed machines will see.
printf 'ssh://192.168.100.200\n' > "$BEB_COURIER_ROOT/depot"
courier sync >/dev/null 2>"$W/err" && die "a leftover depot file was carried by"
grep -q 'nothing reads that file now' "$W/err" || die "depot file: $(cat "$W/err")"
grep -q 'route add ssh://192.168.100.200' "$W/err" || die "the fix is not named: $(cat "$W/err")"
# The fix has to be one a courier in this state can actually run, and
# `route add` reads the same table, so removing comes first or the
# refusal names a command that is refused.
courier route add ssh://192.168.100.200 >/dev/null 2>"$W/err2" &&
    die "route add worked with the old file still there"
grep -q 'nothing reads that file now' "$W/err2" || die "route add: $(cat "$W/err2")"
grep -q "rm $BEB_COURIER_ROOT/depot" "$W/err" || die "removing it is not named first: $(cat "$W/err")"
rm -f "$BEB_COURIER_ROOT/depot"
ok "a depot file from before the table is refused, and names the line to write"

# --- outbound ------------------------------------------------------------

courier sync >/dev/null 2>"$W/err"; rc=$?
test "$rc" -eq 2 || die "an idle sync exited $rc, wanted 2 (nothing to do)"
grep -q 'nothing to send and nothing waiting' "$W/err" || die "idle sync: $(cat "$W/err")"
ok "a sync with nothing to do says so, and is not a failure"

alice send bob --subject across --body "carried" >/dev/null 2>&1
# alice's spool is a different machine's; move the frame to bob's outbox
# the way a second machine's courier never would, so that one courier
# here can be seen doing both halves.
mkdir -p "$W/bob/data/beb/outbox"
mv "$W"/alice/data/beb/outbox/0* "$W/bob/data/beb/outbox/"
# bob reads on this machine, so one sync does both halves: out to the
# depot, and straight back down again.
courier sync >/dev/null 2>"$W/err" || die "sync: $(cat "$W/err")"
test -z "$(ls -A "$W/bob/data/beb/outbox")" || die "the outbox still holds a shipped frame"
grep -q '1 sent, 1 received' "$W/err" || die "sync summary: $(cat "$W/err")"
ok "sync ships what is waiting and drops its copy only once the depot has it"

"$D" held >/dev/null 2>&1; rc=$?
test "$rc" -eq 2 || die "the depot still holds a frame it was acked for"
ok "and collects, hands each frame to beb, and acks so the depot lets go"

# --- what the depot will not take ----------------------------------------

STRANGER=7c1e59a4b8d03f2615ae9c47d2b8f0193a5dc86e1f0b729e4d38c5a06e91b2f8
printf 'a frame for an address nobody granted' > "$W/bob/data/beb/outbox/000000000000000009-$STRANGER"
courier sync >/dev/null 2>"$W/err"; rc=$?
test "$rc" -eq 3 || die "a refused push exited $rc, wanted 3 (refused)"
grep -q 'stays here' "$W/err" || die "no word of the refusal: $(cat "$W/err")"
grep -q 'would not take' "$W/err" || die "no summary of the refusal: $(cat "$W/err")"
grep -q '0 sent' "$W/err" || die "the summary does not count what did move: $(cat "$W/err")"
test -f "$W/bob/data/beb/outbox/000000000000000009-$STRANGER" ||
    die "a refused frame was deleted anyway"
rm -f "$W/bob/data/beb/outbox/000000000000000009-$STRANGER"
ok "a frame the depot refuses stays in the outbox, and the run says so"

out=$(bob read 2>&1) || die "read: $out"
echo "$out" | grep -q "carried" || die "the body did not survive: $out"
echo "$out" | grep -q "alice" || die "the sender did not resolve: $out"
ok "bob reads it, signed by alice, having crossed a depot and two machines"

bob read >/dev/null 2>&1; rc=$?
test "$rc" -eq 2 || die "a second read exited $rc, wanted 2"
ok "and once only"

# --- carry --------------------------------------------------------------

# The wake: a frame dropped at the depot while carry is blocked must
# arrive without anything asking again.
alice pack bob --subject wake --body "while you were waiting" >"$W/frame" 2>/dev/null ||
    die "pack for the carry test"
test -s "$W/frame" || die "the packed frame is empty"
python3 - "$C" "$D" "$BEB" "$W" "$BOBADDR" <<'PY' || die "carry"
import os, subprocess, sys, time
c, d, beb, w, q = sys.argv[1:]
env = dict(os.environ, XDG_DATA_HOME=w + "/bob/data", BEB_BIN=beb)
p = subprocess.Popen([c, "carry"], env=env,
                     stdout=subprocess.DEVNULL, stderr=subprocess.PIPE)
try:
    time.sleep(1.5)                       # let it block inside the depot
    frame = open(w + "/frame", "rb").read()
    r = subprocess.run([d, "serve", "SHA256:zzzz"], input=frame,
                       env=dict(os.environ, SSH_ORIGINAL_COMMAND="drop " + q),
                       capture_output=True)
    assert r.returncode == 0, r.stderr.decode()
    box = os.path.join(w, "bob/data/beb", q, "msg")
    for _ in range(100):
        if len(os.listdir(box)) == 2:
            break
        time.sleep(0.1)
    else:
        raise AssertionError("the frame never arrived: %s" % os.listdir(box))
finally:
    p.kill()
PY
ok "carry holds a connection open, and a frame dropped meanwhile lands unasked"

# The other half, and the one that was missing: mail written while carry
# is running has to leave without anybody running sync. It did not, on a
# real machine, for as long as nobody looked in the outbox.
# A real frame, not a filler one: it goes to the depot and waits there,
# and anything left at a depot is collected by the next drain and handed
# to beb, which knows the difference.
alice pack bob --subject outbound --body "written while carry was running" \
    >"$W/frame2" 2>/dev/null || die "pack for the outbound carry test"
python3 - "$C" "$D" "$BEB" "$W" "$BOBADDR" <<'PY2' || die "carry ships the outbox"
import os, subprocess, sys, time
c, d, beb, w, q = sys.argv[1:]
env = dict(os.environ, XDG_DATA_HOME=w + "/bob/data", BEB_BIN=beb)
p = subprocess.Popen([c, "carry"], env=env,
                     stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
try:
    time.sleep(1.5)
    outbox = os.path.join(w, "bob/data/beb/outbox")
    os.makedirs(outbox, exist_ok=True)
    name = "000000000000000042-" + q
    with open(os.path.join(outbox, name), "wb") as f:
        f.write(open(w + "/frame2", "rb").read())
    for _ in range(150):                       # up to 15s
        if not os.path.exists(os.path.join(outbox, name)):
            break
        time.sleep(0.1)
    else:
        raise AssertionError("carry never shipped it; the outbox still holds " + name)
finally:
    p.kill()
PY2
ok "and ships what is written to the outbox meanwhile, with nobody running sync"

# --- the table ------------------------------------------------------------

# A frame with nowhere to go is the one thing in this program that could
# be silent, so it is said, kept, and named with the line that fixes it.
mkdir -p "$W/noroute"
printf '%s ssh://nowhere.invalid\n' "$STRANGER" > "$W/noroute/routes"
noroute() { env XDG_DATA_HOME=$W/bob/data BEB_COURIER_ROOT=$W/noroute BEB_BIN=$BEB "$C" "$@"; }
printf 'a frame with nowhere to go' > "$W/bob/data/beb/outbox/000000000000000011-$BOBADDR"
noroute sync >/dev/null 2>"$W/err"; rc=$?
test "$rc" -eq 3 || die "a frame with no route exited $rc, wanted 3 (refused)"
grep -q "no route for" "$W/err" || die "no word of it: $(cat "$W/err")"
grep -q "route add $BOBADDR" "$W/err" || die "the fix is not named: $(cat "$W/err")"
test -f "$W/bob/data/beb/outbox/000000000000000011-$BOBADDR" || die "it was deleted anyway"
noroute route >/dev/null 2>"$W/err" || die "route: $(cat "$W/err")"
grep -q '1 waiting to leave match none' "$W/err" || die "route does not say: $(cat "$W/err")"
rm -f "$W/bob/data/beb/outbox/000000000000000011-$BOBADDR"
ok "a frame with no route stays, says so, and route reports it against the table"

# Two forms, because the line with no address has no address to be named
# by, and because a host that has died is the case an operator has. They
# cannot be confused: a place begins ssh:// and an address is hex.
mkdir -p "$W/rm"
printf 'ssh://gone.example\n%s ssh://gone.example\n%s ssh://kept.example\n' \
    "$BOBADDR" "$STRANGER" > "$W/rm/routes"
rmc() { env XDG_DATA_HOME=$W/bob/data BEB_COURIER_ROOT=$W/rm BEB_BIN=$BEB "$C" "$@"; }
rmc route rm ssh://gone.example >/dev/null 2>"$W/err" || die "rm by place: $(cat "$W/err")"
grep -q '2 routes to gone.example removed' "$W/err" || die "rm summary: $(cat "$W/err")"
test "$(wc -l < "$W/rm/routes" | tr -d ' ')" -eq 1 || die "the wrong lines went"
rmc route rm "$STRANGER" >/dev/null 2>"$W/err" || die "rm by address: $(cat "$W/err")"
rmc route >/dev/null 2>"$W/err"; rc=$?
test "$rc" -eq 2 || die "an emptied table exited $rc, wanted 2"
ok "route rm takes an address or a place, told apart by the scheme and never by shape"

# Two places, one of them asleep. Until there were two, "this pass
# failed" and "that host is down" were the same sentence: the dead one is
# tried first here, and everything else still has to leave.
mkdir -p "$W/two"
cp "$BEB_COURIER_ROOT/id_ed25519" "$BEB_COURIER_ROOT/id_ed25519.pub" "$W/two/"
cp "$BEB_COURIER_ROOT/ssh_config" "$W/two/"
printf '%s ssh://127.0.0.1:1\nssh://127.0.0.1:%s\n' "$STRANGER" "$PORT" > "$W/two/routes"
two() { env XDG_DATA_HOME=$W/bob/data BEB_COURIER_ROOT=$W/two BEB_BIN=$BEB "$C" "$@"; }
alice pack bob --subject past --body "the sleeping host did not hold this" \
    >"$W/bob/data/beb/outbox/000000000000000032-$BOBADDR" 2>/dev/null || die "pack"
printf 'for a host that is not listening' > "$W/bob/data/beb/outbox/000000000000000031-$STRANGER"
two sync >/dev/null 2>"$W/err"; rc=$?
test "$rc" -eq 1 || die "a pass with one dead place exited $rc, wanted 1: $(cat "$W/err")"
grep -q '1 sent' "$W/err" || die "the live place did not get its frame: $(cat "$W/err")"
grep -q 'could not reach 127.0.0.1:1' "$W/err" || die "the dead place unreported: $(cat "$W/err")"
test -f "$W/bob/data/beb/outbox/000000000000000031-$STRANGER" || die "the held frame was deleted"
test ! -f "$W/bob/data/beb/outbox/000000000000000032-$BOBADDR" || die "the live frame was held back too"
rm -f "$W/bob/data/beb/outbox/000000000000000031-$STRANGER"
courier sync >/dev/null 2>&1
while bob read >/dev/null 2>&1; do :; done   # caught up, so the next read is the peer's
ok "a place that is asleep holds back its own mail and nobody else's"

# --- registering a new identity, with nobody at the depot ------------------
#
# The handover exists because two facts have to cross to a machine this
# one cannot reach. That is a bootstrap, and it was being paid again on
# every `beb init`. Once the key is authorized the two machines can talk,
# so the identity says so itself, signed, over the connection there
# already is.

mkdir -p "$W/newthing"
(cd "$W/newthing" && env XDG_DATA_HOME=$W/bob/data XDG_CONFIG_HOME=$W/bob/cfg \
    "$BEB" init newthing) >/dev/null 2>&1 || die "a second identity on bob's machine"
NEW=$(env XDG_DATA_HOME=$W/bob/data BEB_IDENTITY=$W/newthing "$BEB" whoami 2>/dev/null)
NEWADDR=$(printf '%s' "$NEW" |
    python3 -c 'import base64,sys; print(base64.b64decode(sys.stdin.read().split()[1])[19:51].hex())')
grep -q "$NEWADDR" "$W/srv/allowed" && die "the new identity was already granted"

# A drop for it is refused while nobody collects, which is the loud half
# of the same fact. A real frame, because it has to survive the round
# trip afterwards and beb knows the difference.
bob pack newthing --subject welcome --body "for the newly registered" \
    > "$W/bob/data/beb/outbox/000000000000000051-$NEWADDR" 2>/dev/null ||
    die "pack for the new identity"
courier sync >/dev/null 2>"$W/err"; rc=$?
test "$rc" -eq 3 || die "a drop for an unregistered identity exited $rc, wanted 3"
grep -q 'stays here' "$W/err" || die "no word of it: $(cat "$W/err")"

echo "$NEW" | env XDG_DATA_HOME=$W/bob/data BEB_IDENTITY=$W/newthing BEB_BIN=$BEB \
    "$C" register >/dev/null 2>"$W/err" || die "register: $(cat "$W/err")"
grep -q 'by its own signature' "$W/err" || die "the depot did not verify it: $(cat "$W/err")"
grep -q "$NEWADDR" "$W/srv/allowed" || die "no grant was written: $(cat "$W/srv/allowed")"
ok "register grants a new identity by its own signature, with nobody at the depot"

# And the same frame now moves, which is the whole point of the grant.
courier sync >/dev/null 2>"$W/err" || die "sync after register: $(cat "$W/err")"
test ! -f "$W/bob/data/beb/outbox/000000000000000051-$NEWADDR" ||
    die "the frame still waits after the grant"
ok "and the frame that was refused a moment ago leaves"

# A claim is not a bearer token: it authorises one fingerprint, and sshd
# decides which one is calling.
mkdir -p "$W/thief"
cp "$BEB_COURIER_ROOT/ssh_config" "$W/thief/"
env BEB_COURIER_ROOT=$W/thief XDG_DATA_HOME=$W/bob/data "$C" init >/dev/null 2>&1
env BEB_COURIER_ROOT=$W/thief XDG_DATA_HOME=$W/bob/data "$C" route add \
    "ssh://127.0.0.1:$PORT" >/dev/null 2>&1
echo "$NEW" | env XDG_DATA_HOME=$W/bob/data BEB_IDENTITY=$W/newthing BEB_BIN=$BEB \
    BEB_COURIER_ROOT=$W/thief "$C" register >/dev/null 2>"$W/err" &&
    die "an unauthorized courier registered somebody else's identity"
ok "a courier the depot has not let in cannot register anybody, signature or not"

# Giving up a grant needs no signature, since a courier can only ever
# take away its own line. It names the queue rather than the key: the
# depot relates the two only where it must, which is verifying a claim.
env XDG_DATA_HOME=$W/bob/data BEB_BIN=$BEB "$C" unregister "$NEWADDR" \
    >/dev/null 2>"$W/err" || die "unregister: $(cat "$W/err")"
grep -q "$NEWADDR" "$W/srv/allowed" && die "the grant is still there"
ok "unregister gives up this machine's own grant, and asserts nothing"

env XDG_DATA_HOME=$W/bob/data BEB_BIN=$BEB "$C" unregister "$NEWADDR" \
    >/dev/null 2>"$W/err"; rc=$?
test "$rc" -eq 2 || die "unregistering twice exited $rc, wanted 2 (nothing to do)"
ok "and twice is nothing to do rather than a failure"

# The two facts nothing else reads together: a grant here and a mailbox
# there. Each is silent on its own, which is why status is the verb that
# has to notice.
sstatus >/dev/null 2>"$W/err"; rc=$?
test "$rc" -eq 3 || die "status passed an identity nobody grants, exiting $rc"
grep -q 'reads here and is granted nowhere' "$W/err" ||
    die "the unregistered identity is unreported: $(cat "$W/err")"
grep -q 'beb whoami | beb-courier register' "$W/err" ||
    die "status does not name the line that fixes it: $(cat "$W/err")"
ok "status finds an identity that reads here and is granted nowhere, and names the fix"

echo "$NEW" | env XDG_DATA_HOME=$W/bob/data BEB_IDENTITY=$W/newthing BEB_BIN=$BEB \
    "$C" register >/dev/null 2>&1 || die "register for the drift test"
rm -rf "$W/newthing" "$W/bob/data/beb/$NEWADDR"
sstatus >/dev/null 2>"$W/err"; rc=$?
test "$rc" -eq 3 || die "status passed a grant nothing collects on, exiting $rc"
grep -q 'is granted there and reads nowhere here' "$W/err" ||
    die "the stale grant is unreported: $(cat "$W/err")"
grep -q "beb-courier unregister $NEWADDR" "$W/err" ||
    die "status does not name the address to give up: $(cat "$W/err")"
ok "and a grant left behind by a deleted identity, naming the address it can no longer print"

env XDG_DATA_HOME=$W/bob/data BEB_BIN=$BEB "$C" unregister "$NEWADDR" >/dev/null 2>&1
sstatus >/dev/null 2>"$W/err" || die "status still disagrees: $(cat "$W/err")"
ok "and goes quiet once the two agree again"

# The operator's half, for the machine that cannot call in.
CFP=$(ssh-keygen -lf "$BEB_COURIER_ROOT/id_ed25519.pub" | awk '{print $2}')
"$D" grant "$CFP" "$NEWADDR" >/dev/null 2>&1 || die "grant"
"$D" revoke "$CFP" "$NEWADDR" >/dev/null 2>"$W/err" || die "revoke: $(cat "$W/err")"
grep -q 'drops for it are refused' "$W/err" || die "revoke summary: $(cat "$W/err")"
ok "revoke takes a grant back at the depot, for a courier that never comes back"

# --- a direct link, with no depot in it ------------------------------------
#
# The receiving half of a peer link is not code in this program: it is a
# forced command running `beb drop`, which resolves no identity and takes
# the recipient from the frame. So this is the same sshd, a second key,
# and a line beb had already written.

mkdir -p "$W/peer"
peer() { env XDG_DATA_HOME=$W/alice/data BEB_COURIER_ROOT=$W/peer BEB_BIN=$BEB "$C" "$@"; }
cp "$BEB_COURIER_ROOT/ssh_config" "$W/peer/"
peer init >/dev/null 2>"$W/err" || die "peer init: $(cat "$W/err")"
peer whoami >"$W/alice.handover" 2>/dev/null || die "alice's handover"

# On bob's machine, with bob's spool: the line has to carry it, since a
# forced command inherits none of the operator's environment.
env XDG_DATA_HOME=$W/bob/data BEB_BIN=$BEB BEB_COURIER_AUTHORIZED_KEYS=$W/authorized_keys \
    "$C" authorize "$W/alice.handover" >"$W/line" 2>"$W/err" || die "authorize: $(cat "$W/err")"
grep -q "drop\",restrict" "$W/line" || die "not a drop line: $(cat "$W/line")"
grep -q 'SHA256:' "$W/line" && die "a peer line carries a fingerprint it has no question for"
grep -qF "$W/bob/data" "$W/line" || die "the line does not carry bob's spool: $(cat "$W/line")"
ok "authorize writes the one line sshd needs, and nothing it has no question for"

# And on alice's, the other half of the same handover pair.
peer route add "$W/handover" "ssh://127.0.0.1:$PORT" >/dev/null 2>"$W/err" ||
    die "route add by handover: $(cat "$W/err")"
grep -q "^$BOBADDR .*ssh://127.0.0.1:$PORT" "$W/peer/routes" ||
    die "the addresses in the handover were not routed: $(cat "$W/peer/routes")"

alice send bob --subject direct --body "no depot carried this" >/dev/null 2>&1
peer sync >/dev/null 2>"$W/err"; rc=$?
test "$rc" -eq 0 || die "a peer sync exited $rc: $(cat "$W/err")"
grep -q '1 sent, 0 received' "$W/err" ||
    die "a peer holds nothing, so nothing is collected from it: $(cat "$W/err")"
# The frames alone: beb keeps its counter in there too, and a courier
# reads past it by the same rule that names each frame `<id>-<address>`.
test -z "$(ls "$W/alice/data/beb/outbox"/0* 2>/dev/null)" ||
    die "the frame stayed in alice's outbox"
out=$(bob read 2>&1) || die "read: $out"
echo "$out" | grep -q "no depot carried this" || die "the body did not survive: $out"
ok "alice ships straight into bob's beb, over ssh, with no depot in the path"

env HOME=$W/home XDG_DATA_HOME=$W/alice/data BEB_COURIER_ROOT=$W/peer BEB_BIN=$BEB \
    "$C" status >/dev/null 2>"$W/err"; rc=$?
test "$rc" -eq 0 || die "status on a peer-only courier exited $rc: $(cat "$W/err")"
grep -q 'answers and knows this key' "$W/err" || die "the peer was not probed: $(cat "$W/err")"
grep -q 'collecting from nowhere' "$W/err" ||
    die "status does not say nothing shelves here: $(cat "$W/err")"
ok "status probes a peer the same way, and says that nothing collects for this machine"

# A unit gets systemd's PATH, not the operator's. On the first machine to
# run one, "beb" resolved to a different install four versions behind,
# which refused every frame; the same command in a login shell worked.
courier unit >"$W/unit" 2>/dev/null || die "unit"
# Whichever supervisor this platform has, it has to name beb absolutely:
# neither systemd nor launchd inherits the operator's PATH.
grep -qF "$BEB" "$W/unit" || die "the file names a different beb than the one in use: $(cat "$W/unit")"
grep -q 'carry' "$W/unit" || die "the file does not run carry: $(cat "$W/unit")"
case "$(uname -s)" in
    Darwin)
        grep -q '<key>Label</key>' "$W/unit" || die "macOS got something that is not a plist"
        grep -q 'dev.getbeb.courier' "$W/unit" || die "the plist has no label"
        grep -q '\[Service\]' "$W/unit" && die "macOS got a systemd unit, which nothing there reads"
        ;;
    *)
        grep -q "^Environment=BEB_BIN=/" "$W/unit" ||
            die "the unit does not name beb by absolute path: $(grep Environment "$W/unit")"
        grep -q '<plist' "$W/unit" && die "linux got a plist"
        ;;
esac
ok "unit prints the supervisor file this platform actually reads, naming beb absolutely"

# beb decides admission before it reads a body, so a refusal breaks the
# pipe mid-write. What the operator needs then is beb's sentence, not the
# pipe's -- and the child must still be reaped.
printf 'not a frame at all' >"$W/junk"
out=$(BEB_IDENTITY=$W/bob XDG_DATA_HOME=$W/bob/data "$BEB" drop <"$W/junk" 2>&1); rc=$?
test "$rc" -ne 0 || die "beb accepted junk, so this cannot be tested"
echo "$out" | grep -q '^beb:' || die "beb said nothing about junk: $out"
ok "beb refuses a malformed frame with a sentence, which is what must survive the broken pipe"

echo "all $n tests passed"
