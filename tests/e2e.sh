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

courier init "not-a-url" >/dev/null 2>"$W/err" && die "a bare hostname was accepted"
grep -q 'ssh://' "$W/err" || die "depot refusal: $(cat "$W/err")"
courier init "ssh://127.0.0.1:$PORT" >/dev/null 2>"$W/err" || die "init: $(cat "$W/err")"
test -f "$BEB_COURIER_ROOT/id_ed25519.pub" || die "no key was minted"
grep -q "ssh://127.0.0.1:$PORT" "$BEB_COURIER_ROOT/depot" || die "the depot was not recorded"
ok "init mints one key for the machine and writes down the depot"

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

courier init "ssh://elsewhere" >/dev/null 2>"$W/err" && die "init overwrote an existing key"
grep -q 'already has a courier key' "$W/err" || die "second init: $(cat "$W/err")"
ok "a second init is refused, because a new key is one the depot never heard of"

# --- the handover --------------------------------------------------------

courier whoami >"$W/handover" 2>"$W/err" || die "whoami: $(cat "$W/err")"
head -1 "$W/handover" | grep -q '^ssh-ed25519 ' || die "no key on line 1: $(cat "$W/handover")"
test "$(tail -n +2 "$W/handover" | wc -l | tr -d ' ')" -eq 1 || die "wrong recipient count"
BOBQ=$(tail -n +2 "$W/handover")
test -d "$W/bob/data/beb/$BOBQ" || die "the queue named is not a mailbox here"
grep -q '1 key, 1 queues' "$W/err" || die "whoami summary: $(cat "$W/err")"
ok "whoami prints the key and the queues this machine reads for, and nothing else"

"$D" authorize "$W/handover" >/dev/null 2>"$W/err" || die "authorize: $(cat "$W/err")"
grep -q "may now collect for $BOBQ" "$W/err" || die "authorize by handover: $(cat "$W/err")"
ok "the depot takes that file whole, so neither side pastes a queue name"

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
printf 'a frame for a queue nobody granted' > "$W/bob/data/beb/outbox/000000000000000009-$STRANGER"
courier sync >/dev/null 2>"$W/err"; rc=$?
test "$rc" -eq 3 || die "a refused push exited $rc, wanted 3 (refused)"
grep -q 'stays here' "$W/err" || die "no word of the refusal: $(cat "$W/err")"
grep -q 'would not take' "$W/err" || die "no summary of the refusal: $(cat "$W/err")"
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

# --- listen --------------------------------------------------------------

# The wake: a frame dropped at the depot while listen is blocked must
# arrive without anything asking again.
alice pack bob --subject wake --body "while you were waiting" >"$W/frame" 2>/dev/null ||
    die "pack for the listen test"
test -s "$W/frame" || die "the packed frame is empty"
python3 - "$C" "$D" "$BEB" "$W" "$BOBQ" <<'PY' || die "listen"
import os, subprocess, sys, time
c, d, beb, w, q = sys.argv[1:]
env = dict(os.environ, XDG_DATA_HOME=w + "/bob/data", BEB_BIN=beb)
p = subprocess.Popen([c, "listen"], env=env,
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
ok "listen holds a connection open, and a frame dropped meanwhile lands unasked"

# The other half, and the one that was missing: mail written while listen
# is running has to leave without anybody running sync. It did not, on a
# real machine, for as long as nobody looked in the outbox.
python3 - "$C" "$D" "$BEB" "$W" "$BOBQ" <<'PY2' || die "listen ships the outbox"
import os, subprocess, sys, time
c, d, beb, w, q = sys.argv[1:]
env = dict(os.environ, XDG_DATA_HOME=w + "/bob/data", BEB_BIN=beb)
p = subprocess.Popen([c, "listen"], env=env,
                     stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
try:
    time.sleep(1.5)
    outbox = os.path.join(w, "bob/data/beb/outbox")
    os.makedirs(outbox, exist_ok=True)
    name = "000000000000000042-" + q
    with open(os.path.join(outbox, name), "wb") as f:
        f.write(b"written while listen was running")
    for _ in range(150):                       # up to 15s
        if not os.path.exists(os.path.join(outbox, name)):
            break
        time.sleep(0.1)
    else:
        raise AssertionError("listen never shipped it; the outbox still holds " + name)
finally:
    p.kill()
PY2
ok "and ships what is written to the outbox meanwhile, with nobody running sync"

echo "all $n tests passed"
