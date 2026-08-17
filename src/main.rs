//! beb-courier: moves beb's mail between machines, and knows nothing
//! about mail.
//!
//! It never parses an envelope, never verifies a signature, and never
//! computes a path into a mailbox. What is waiting to leave is a
//! directory whose filenames carry the address; what arrives goes to
//! `beb drop` and nowhere else.

use std::fs::{self, File, OpenOptions};
use std::io::{self, BufRead, BufReader, Read, Write};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitCode, Stdio};
use std::time::Duration;

const DIR_MODE: u32 = 0o700;
const FILE_MODE: u32 = 0o600;

/// A frame this program will not carry, matching the depot's own
/// ceiling. It exists here only so a broken depot cannot make a courier
/// allocate without bound; the depot refuses the same size on arrival.
const FRAME_MAX: u64 = 64 * 1024 * 1024;

/// How long `listen` waits before reconnecting, and the ceiling it
/// backs off to.
///
/// A depot that is down is a depot somebody is fixing, and a courier
/// that retries every second for an hour is a courier in that person's
/// log. Doubling from a second to a minute reconnects promptly after a
/// blip and quietly after an outage.
const BACKOFF_MIN: Duration = Duration::from_secs(1);
const BACKOFF_MAX: Duration = Duration::from_secs(60);

/// How often `listen` looks in the outbox.
///
/// A poll, and cheap enough not to argue about: reading an empty outbox
/// measured 0.018ms against 1277ms for the one ssh handshake it exists
/// to trigger, so looking four times a second costs about seventy
/// thousandths of the work it saves waiting for. A kernel watch would
/// notice instantly and cost a platform's worth of code that beb already
/// carries and this does not.
///
/// Nothing connects on an empty outbox: `push` reads the directory and
/// returns without opening anything when there is nothing in it.
const OUTBOX_POLL: Duration = Duration::from_millis(250);

struct Fail {
    code: u8,
    msg: String,
}

fn refused(msg: impl Into<String>) -> Fail {
    Fail { code: 3, msg: msg.into() }
}

fn nothing(msg: impl Into<String>) -> Fail {
    Fail { code: 2, msg: msg.into() }
}

impl From<String> for Fail {
    fn from(msg: String) -> Fail {
        Fail { code: 1, msg }
    }
}

impl From<&str> for Fail {
    fn from(msg: &str) -> Fail {
        Fail { code: 1, msg: msg.to_string() }
    }
}

const USAGE: &str = "\
beb-courier carries beb's mail to and from a depot.

  beb-courier init DEPOT
      mint this machine's courier key and name the depot it uses
  beb-courier whoami
      this machine's key and the queues it reads for, for the operator
  beb-courier sync
      push what is waiting to leave, pull what is waiting to arrive
  beb-courier listen
      carry both ways until stopped: arrivals land as they happen,
      and what is waiting to leave goes as it is written
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

BEB_BIN names the beb to hand mail to, and defaults to what is on PATH.";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let r = match args.first().map(String::as_str) {
        Some("init") => cmd_init(&args[1..]),
        Some("whoami") => cmd_whoami(&args[1..]),
        Some("sync") => cmd_sync(&args[1..]),
        Some("listen") => cmd_listen(&args[1..]),
        Some("unit") => cmd_unit(&args[1..]),
        Some("--help") | Some("-h") | None => {
            println!("{USAGE}");
            return ExitCode::SUCCESS;
        }
        Some("--version") => {
            println!("beb-courier {}", env!("CARGO_PKG_VERSION"));
            return ExitCode::SUCCESS;
        }
        Some(other) => {
            Err(format!("no such command \"{other}\"; beb-courier --help lists them").into())
        }
    };
    match r {
        Ok(()) => ExitCode::SUCCESS,
        Err(f) => {
            note(&f.msg);
            ExitCode::from(f.code)
        }
    }
}

/// Everything said about a result, never the result itself.
fn note(msg: &str) {
    let _ = io::stdout().flush();
    let mut err = io::stderr().lock();
    for line in msg.lines() {
        let _ = writeln!(err, "beb-courier: {line}");
    }
}

fn home() -> Result<PathBuf, String> {
    std::env::var_os("HOME")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .ok_or_else(|| "HOME is not set".to_string())
}

fn root() -> Result<PathBuf, String> {
    if let Some(r) = std::env::var_os("BEB_COURIER_ROOT").filter(|v| !v.is_empty()) {
        return Ok(PathBuf::from(r));
    }
    Ok(home()?.join(".local/share/beb-courier"))
}

/// beb's spool, by beb's own rule. Read rather than asked for: a
/// courier that had its own idea of where the spool is would be a
/// second opinion about a thing beb decides.
fn spool() -> Result<PathBuf, String> {
    if let Some(x) = std::env::var_os("XDG_DATA_HOME").filter(|v| !v.is_empty()) {
        return Ok(PathBuf::from(x).join("beb"));
    }
    Ok(home()?.join(".local/share/beb"))
}

fn private_dir_all(p: &Path) -> io::Result<()> {
    fs::create_dir_all(p)?;
    fs::set_permissions(p, fs::Permissions::from_mode(DIR_MODE))
}

fn key_path(root: &Path) -> PathBuf {
    root.join("id_ed25519")
}

fn depot_path(root: &Path) -> PathBuf {
    root.join("depot")
}

/// An ssh config of the courier's own, used when it exists.
///
/// Not a knob but the rest of the topology. ssh expands `~` from the
/// password database rather than from HOME, so a courier running as a
/// daemon user has no way to be told the depot's host key except this
/// one: a file beside its key, holding UserKnownHostsFile, or a
/// ProxyJump, or whichever user the depot expects.
///
/// Absent means "use this account's ordinary ssh setup", which is right
/// for a courier that runs as a person.
fn ssh_config_path(root: &Path) -> PathBuf {
    root.join("ssh_config")
}

// ---- what this machine is ----------------------------------------------

/// A depot as a place to reach, not a URL library.
///
/// One scheme today. The scheme is written down anyway so that a second
/// way of reaching a depot has somewhere to be named without anything
/// being renamed, which is the whole reason it is not a bare hostname.
#[derive(Clone)]
struct Depot {
    host: String,
    port: Option<String>,
}

fn parse_depot(s: &str) -> Result<Depot, Fail> {
    let rest = s.strip_prefix("ssh://").ok_or_else(|| {
        refused(format!(
            "\"{s}\" is not a depot; it looks like ssh://[user@]host[:port]"
        ))
    })?;
    if rest.is_empty() || rest.contains('/') {
        return Err(refused(format!(
            "\"{s}\" is not a depot; it looks like ssh://[user@]host[:port], with no path"
        )));
    }
    // A colon after the last ']' or after the host is a port. Bracketed
    // IPv6 is left to ssh, which understands it and this does not.
    match rest.rsplit_once(':') {
        Some((h, p)) if !h.is_empty() && p.chars().all(|c| c.is_ascii_digit()) && !p.is_empty() => {
            Ok(Depot { host: h.to_string(), port: Some(p.to_string()) })
        }
        _ => Ok(Depot { host: rest.to_string(), port: None }),
    }
}

fn read_depot(root: &Path) -> Result<Depot, Fail> {
    let p = depot_path(root);
    let text = fs::read_to_string(&p).map_err(|_| {
        Fail::from(format!(
            "no depot named in {}; make one with: beb-courier init ssh://host",
            root.display()
        ))
    })?;
    parse_depot(text.trim())
}

fn cmd_init(args: &[String]) -> Result<(), Fail> {
    let depot = match args {
        [d] => parse_depot(d)?,
        _ => return Err("init takes the depot to use: beb-courier init ssh://host".into()),
    };
    let root = root()?;
    let key = key_path(&root);
    // Refused rather than overwritten: a new key is a key the depot has
    // never been told about, and replacing one silently would leave this
    // machine unable to collect with no sign of why.
    if key.exists() {
        return Err(refused(format!(
            "{} already has a courier key; to point it elsewhere, edit {}",
            root.display(),
            depot_path(&root).display()
        )));
    }
    private_dir_all(&root).map_err(|e| format!("cannot create {}: {e}", root.display()))?;

    let label = format!("beb-courier@{}", hostname());
    let out = Command::new("ssh-keygen")
        .args(["-q", "-t", "ed25519", "-N", "", "-C", &label, "-f"])
        .arg(&key)
        .output()
        .map_err(|e| Fail::from(format!("cannot run ssh-keygen: {e}")))?;
    if !out.status.success() {
        return Err(format!(
            "ssh-keygen: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        )
        .into());
    }

    let mut f = private_file(&depot_path(&root))
        .map_err(|e| format!("cannot write the depot name: {e}"))?;
    writeln!(f, "ssh://{}", host_of(&depot)).map_err(|e| format!("cannot write: {e}"))?;
    f.sync_all().map_err(|e| format!("cannot sync: {e}"))?;

    note(&format!("courier key in {}", key.display()));
    note(&format!("depot {} in {}", host_of(&depot), depot_path(&root).display()));
    note("beb-courier whoami prints what the depot operator needs");
    Ok(())
}

/// The comment on the courier key, which is how an operator tells one
/// line of authorized_keys from the next.
///
/// Asked of `hostname` rather than of the environment: HOSTNAME is a
/// shell variable, not an exported one, so reading it named every
/// courier "unknown" -- which is what the first two deployed keys are
/// still called, since a comment is written once at init.
fn hostname() -> String {
    Command::new("hostname")
        .output()
        .ok()
        .filter(|o| o.status.success())
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty() && !s.contains(char::is_whitespace))
        .unwrap_or_else(|| "unknown".into())
}

fn host_of(d: &Depot) -> String {
    match &d.port {
        Some(p) => format!("{}:{}", d.host, p),
        None => d.host.clone(),
    }
}

fn private_file(p: &Path) -> io::Result<File> {
    OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(FILE_MODE)
        .open(p)
}

/// The queues this machine reads for.
///
/// A directory read, not a computation. beb names each mailbox for the
/// identity's key, so the spool already holds exactly the queue names a
/// depot wants, and the only work is dropping `outbox`, which sits
/// beside them. Sending to a stranger never invents a mailbox and a
/// mailbox appears at `beb init`, so what is left is precisely who
/// reads here, at the moment somebody first asks.
fn recipients() -> Result<Vec<String>, Fail> {
    let dir = spool()?;
    let mut out: Vec<String> = match fs::read_dir(&dir) {
        Ok(rd) => rd
            .filter_map(|e| e.ok())
            .filter_map(|e| e.file_name().to_str().map(str::to_string))
            .filter(|n| is_queue(n))
            .collect(),
        Err(_) => Vec::new(),
    };
    out.sort();
    Ok(out)
}

fn is_queue(s: &str) -> bool {
    s.len() == 64 && s.bytes().all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
}

/// Everything a depot operator needs from this machine, and nothing
/// else: the key that will be calling, and the queues it collects for.
///
/// One document because the two facts have to travel together to a
/// machine this one was built to be unable to reach, and because the
/// depot can take them together -- `beb-depot authorize` reads exactly
/// this shape. The key first, the queues after, prose on stderr so the
/// file is only ever the handover.
fn cmd_whoami(args: &[String]) -> Result<(), Fail> {
    if !args.is_empty() {
        return Err("whoami takes nothing: beb-courier whoami".into());
    }
    let root = root()?;
    let pubkey = key_path(&root).with_extension("pub");
    let key = fs::read_to_string(&pubkey).map_err(|_| {
        Fail::from(format!(
            "no courier key in {}; make one with: beb-courier init ssh://host",
            root.display()
        ))
    })?;
    let key = key.trim();
    let mine = recipients()?;

    let mut out = io::stdout().lock();
    writeln!(out, "{key}").map_err(|e| format!("cannot write: {e}"))?;
    for to in &mine {
        writeln!(out, "{to}").map_err(|e| format!("cannot write: {e}"))?;
    }
    drop(out);

    match mine.len() {
        0 => note(&format!(
            "1 key, and no identity reads on this machine yet, so there is nothing to collect\n\
             beb init NAME makes one, then run this again"
        )),
        n => note(&format!("1 key, {n} queues this machine reads for")),
    }
    note("give this to whoever runs the depot: beb-depot authorize <this file>");
    Ok(())
}

// ---- the two directions -------------------------------------------------

fn cmd_sync(args: &[String]) -> Result<(), Fail> {
    if !args.is_empty() {
        return Err("sync takes nothing: beb-courier sync".into());
    }
    let root = root()?;
    let depot = read_depot(&root)?;
    let (pushed, refused_here) = push(&root, &depot)?;
    let pulled = collect(&root, &depot, "drain")?;
    if refused_here > 0 {
        // Everything that could move has moved; this is what did not.
        // Reported as a refusal rather than folded into a count, because
        // a sync on a timer that quietly leaves the same frames behind
        // every run is a sync nobody looks at again.
        return Err(refused(format!(
            "{pushed} sent, {pulled} received, {refused_here} the depot would not take"
        )));
    }
    if pushed == 0 && pulled == 0 {
        return Err(nothing("nothing to send and nothing waiting"));
    }
    note(&format!("{pushed} sent, {pulled} received"));
    Ok(())
}

/// Outbound. Nothing has to be running for this to have work: the
/// outbox only fills because somebody sent something, so the sender was
/// already here.
///
/// The filename is the whole routing table. Opening the frame to find
/// out where it goes is the one thing a courier must never do, and the
/// reason it does not have to is that beb writes the address into the
/// name.
fn push(root: &Path, depot: &Depot) -> Result<(usize, usize), Fail> {
    let dir = spool()?.join("outbox");
    let mut waiting: Vec<(String, PathBuf)> = match fs::read_dir(&dir) {
        Ok(rd) => rd
            .filter_map(|e| e.ok())
            .filter_map(|e| {
                let name = e.file_name().to_str()?.to_string();
                let (_, to) = name.split_once('-')?;
                is_queue(to).then(|| (name.clone(), e.path()))
            })
            .collect(),
        Err(_) => Vec::new(),
    };
    // Oldest first, because the ids are ordered and a reader would
    // rather see a conversation in the order it was written.
    waiting.sort_by(|a, b| a.0.cmp(&b.0));

    let (mut sent, mut refused_here) = (0, 0);
    for (name, path) in waiting {
        let to = name.split_once('-').map(|(_, t)| t).unwrap_or_default();
        let f = File::open(&path).map_err(|e| format!("cannot open {}: {e}", path.display()))?;
        let out = ssh(depot, root)
            .arg(format!("drop {to}"))
            .stdin(Stdio::from(f))
            .output()
            .map_err(|e| Fail::from(format!("cannot run ssh: {e}")))?;
        if out.status.success() {
            // Only now, and never before: the depot has it, so this copy
            // is the redundant one. The other order loses mail.
            fs::remove_file(&path).map_err(|e| format!("cannot remove {}: {e}", path.display()))?;
            sent += 1;
            continue;
        }
        let why = String::from_utf8_lossy(&out.stderr);
        let why = why.trim();
        // 255 is ssh itself: the depot was not reached, so every other
        // frame would fail the same way and the run stops. Anything else
        // came from the depot and is about this one frame.
        if out.status.code() == Some(255) {
            return Err(Fail {
                code: 1,
                msg: format!(
                    "cannot reach {}: {why}\n\
                     ssh has to know that host already; accept its key once, or put \
                     UserKnownHostsFile in {}",
                    host_of(depot),
                    ssh_config_path(root).display()
                ),
            });
        }
        note(&format!("{} stays here: {why}", short(&name)));
        refused_here += 1;
    }
    Ok((sent, refused_here))
}

fn short(name: &str) -> String {
    match name.split_once('-') {
        Some((id, to)) => format!("{id}-{}", &to[..8.min(to.len())]),
        None => name.to_string(),
    }
}

/// Inbound, which is why a daemon exists at all. A depot cannot open a
/// connection to a client behind NAT, so a client that wants to be woken
/// holds one open and lets the far side block inside it.
///
/// `drain` takes what is there and returns; `pickup` does the same and
/// then waits. One function because they differ in nothing else.
fn collect(root: &Path, depot: &Depot, intent: &str) -> Result<usize, Fail> {
    let mut child = ssh(depot, root)
        .arg(intent)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .map_err(|e| Fail::from(format!("cannot run ssh: {e}")))?;
    let r = drain_connection(&mut child);
    let _ = child.kill();
    let _ = child.wait();
    r
}

fn drain_connection(child: &mut Child) -> Result<usize, Fail> {
    let mut out = BufReader::new(child.stdout.take().expect("stdout piped"));
    let mut ack = child.stdin.take().expect("stdin piped");
    let mut got = 0usize;
    loop {
        let mut header = String::new();
        let n = out
            .read_line(&mut header)
            .map_err(|e| Fail::from(format!("cannot read from the depot: {e}")))?;
        if n == 0 {
            return Ok(got); // the depot finished, or hung up
        }
        let (to, id, len) = parse_header(&header)?;
        if len > FRAME_MAX {
            return Err(refused(format!("the depot offered {len} bytes, over the ceiling")));
        }
        let mut frame = vec![0u8; len as usize];
        out.read_exact(&mut frame)
            .map_err(|e| Fail::from(format!("the depot cut off mid-frame: {e}")))?;

        hand_to_beb(&frame, &to)?;
        // Acked only once beb has it, so a courier that dies here loses
        // nothing: the depot still holds the frame and offers it again,
        // and beb deduplicates whatever it is handed twice.
        writeln!(ack, "ack {id}").map_err(|e| format!("cannot acknowledge: {e}"))?;
        ack.flush().map_err(|e| format!("cannot acknowledge: {e}"))?;
        got += 1;
    }
}

fn parse_header(line: &str) -> Result<(String, u64, u64), Fail> {
    let mut f = line.split_whitespace();
    match (f.next(), f.next(), f.next(), f.next()) {
        (Some(to), Some(id), Some(len), None) => {
            let id = id.parse().map_err(|_| Fail::from(format!("bad id \"{id}\"")))?;
            let len = len.parse().map_err(|_| Fail::from(format!("bad length \"{len}\"")))?;
            Ok((to.to_string(), id, len))
        }
        _ => Err(format!("the depot said \"{}\", which is not a frame header", line.trim()).into()),
    }
}

/// The one verb a courier calls. Not a path into a mailbox, not a
/// residency check, not a signature: every rule about what may be
/// installed lives on the other side of this call.
fn hand_to_beb(frame: &[u8], to: &str) -> Result<(), Fail> {
    let beb = std::env::var("BEB_BIN").unwrap_or_else(|_| "beb".into());
    let mut child = Command::new(&beb)
        .arg("drop")
        .stdin(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| {
            Fail::from(format!(
                "cannot run {beb}: {e}\nBEB_BIN names it if it is not on PATH"
            ))
        })?;
    child
        .stdin
        .take()
        .expect("stdin piped")
        .write_all(frame)
        .map_err(|e| format!("cannot write to beb: {e}"))?;
    let out = child
        .wait_with_output()
        .map_err(|e| Fail::from(format!("cannot wait for beb: {e}")))?;
    if out.status.success() {
        note(String::from_utf8_lossy(&out.stderr).trim());
        return Ok(());
    }
    // Not acked, so the depot keeps it. A frame for somebody who does
    // not read here means the depot was told this courier collects for
    // them and it does not, which is an operator's mistake and not
    // something to paper over by deleting mail.
    Err(refused(format!(
        "beb would not take a frame for {}: {}\n\
         either that identity should read here, or the depot should stop offering it",
        &to[..8.min(to.len())],
        String::from_utf8_lossy(&out.stderr).trim()
    )))
}

/// Both directions until stopped.
///
/// Inbound is why a long-lived verb exists at all: a depot cannot dial a
/// client behind NAT, so the client holds a connection open and lets the
/// far side block inside it.
///
/// Outbound was left to `sync` on the argument that the outbox only
/// fills because somebody sent something, so the sender is already there
/// to push it. That is true of a person at a shell and false of an agent:
/// on the first machine to run this as a service, `beb send` said "a
/// carrier takes it from there", the carrier was running, and the mail
/// sat in the outbox until somebody ran `sync` by hand. Nothing reported
/// a fault, because nothing had faulted.
///
/// So the two halves run together, in two threads, because one blocks
/// inside a connection the depot owns and the other must not wait on it.
fn cmd_listen(args: &[String]) -> Result<(), Fail> {
    if !args.is_empty() {
        return Err("listen takes nothing: beb-courier listen".into());
    }
    let root = root()?;
    let depot = read_depot(&root)?;
    note(&format!("carrying both ways with {}", host_of(&depot)));

    let (r, d) = (root.clone(), depot.clone());
    std::thread::spawn(move || outbound(&r, &d));

    let mut wait = BACKOFF_MIN;
    loop {
        match collect(&root, &depot, "pickup") {
            // A blocking pickup that returned means the connection
            // ended, not that there is nothing to do. Reconnect.
            Ok(n) => {
                if n > 0 {
                    wait = BACKOFF_MIN; // it worked, so start over patient
                }
            }
            // A refusal is about this depot's answer and will not change
            // by asking again in a second.
            Err(f) if f.code == 3 => return Err(f),
            Err(f) => note(&f.msg),
        }
        std::thread::sleep(wait);
        wait = (wait * 2).min(BACKOFF_MAX);
    }
}

/// The outbound half of `listen`: ship what is waiting, as it appears.
///
/// Backs off for the same two reasons the inbound half does, and they
/// are not the same reason. A depot that cannot be reached will not be
/// reachable a quarter second later. A frame the depot refuses will be
/// refused every time it is offered, and at four looks a second that is
/// the same line four times a second forever, so a refusal quiets down
/// to once a minute rather than being retried at speed.
fn outbound(root: &Path, depot: &Depot) -> ! {
    let mut wait = BACKOFF_MIN;
    loop {
        match push(root, depot) {
            Ok((sent, refused_here)) => {
                if sent > 0 {
                    note(&format!("{sent} sent"));
                }
                if refused_here > 0 {
                    std::thread::sleep(wait);
                    wait = (wait * 2).min(BACKOFF_MAX);
                    continue;
                }
                wait = BACKOFF_MIN;
            }
            Err(f) => {
                note(&f.msg);
                std::thread::sleep(wait);
                wait = (wait * 2).min(BACKOFF_MAX);
                continue;
            }
        }
        std::thread::sleep(OUTBOX_POLL);
    }
}

fn ssh(depot: &Depot, root: &Path) -> Command {
    let mut c = Command::new("ssh");
    let cfg = ssh_config_path(root);
    if cfg.is_file() {
        c.arg("-F").arg(&cfg);
    }
    c.arg("-o")
        .arg("BatchMode=yes")
        .arg("-o")
        .arg("IdentitiesOnly=yes")
        .arg("-i")
        .arg(key_path(root));
    if let Some(p) = &depot.port {
        c.arg("-p").arg(p);
    }
    c.arg(&depot.host);
    c
}

fn cmd_unit(args: &[String]) -> Result<(), Fail> {
    if !args.is_empty() {
        return Err("unit takes nothing: beb-courier unit".into());
    }
    let exe = std::env::current_exe()
        .map_err(|e| Fail::from(format!("cannot find my own path: {e}")))?;
    let root = root()?;
    let mut out = io::stdout().lock();
    // Printed, never installed. Where a unit belongs and whether it is
    // wanted is the operator's, and a program that writes into
    // /etc is a program that has to be trusted with more than it needs.
    write!(
        out,
        "\
[Unit]
Description=beb-courier: hold a connection to the depot
After=network-online.target

[Service]
ExecStart={exe} listen
Environment=BEB_COURIER_ROOT={root}
Restart=always
RestartSec=5

[Install]
WantedBy=default.target
",
        exe = exe.display(),
        root = root.display()
    )
    .map_err(|e| format!("cannot write: {e}"))?;
    drop(out);
    note("write that to ~/.config/systemd/user/beb-courier.service");
    note("then: systemctl --user enable --now beb-courier");
    Ok(())
}
