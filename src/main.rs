//! beb-courier: moves beb's mail between machines, and knows nothing
//! about mail.
//!
//! It never parses an envelope, never verifies a signature, and never
//! computes a path into a mailbox. What is waiting to leave is a
//! directory whose filenames carry the address; what arrives goes to
//! `beb drop` and nowhere else.

use std::collections::HashMap;
use std::fs::{self, File, OpenOptions};
use std::io::{self, BufRead, BufReader, Read, Write};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitCode, Stdio};
use std::time::{Duration, Instant};

const DIR_MODE: u32 = 0o700;
const FILE_MODE: u32 = 0o600;

/// A frame this program will not carry, matching the depot's own
/// ceiling. It exists here only so a broken depot cannot make a courier
/// allocate without bound; the depot refuses the same size on arrival.
const FRAME_MAX: u64 = 64 * 1024 * 1024;

/// How long to leave something alone before asking again, and the
/// ceiling it backs off to.
///
/// A place that is down is a place somebody is fixing, and a courier
/// that retries every second for an hour is a courier in that person's
/// log. Doubling from a second to a minute reconnects promptly after a
/// blip and quietly after an outage.
const BACKOFF_MIN: Duration = Duration::from_secs(1);
const BACKOFF_MAX: Duration = Duration::from_secs(60);

/// How often `carry` looks in the outbox.
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
beb-courier {version} carries beb's mail to wherever each recipient is.

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
  beb-courier authorize FILE
      let the courier in that handover drop mail here, over ssh
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
or a place alone for everything else, and an exact route wins.

Collection is from the address-less route and nowhere else, since only a
place that shelves has anything to hand back. A peer holds nothing: it is
written into directly, and that is the wake.

BEB_COURIER_ROOT holds the key, the routes, and an ssh_config if this
machine needs one. It defaults to ~/.local/share/beb-courier:

  id_ed25519    this courier's key, minted by init
  routes        one line each: an address and a place, or a place alone
  ssh_config    optional, and used only if it is there

BEB_BIN names the beb to hand mail to, and defaults to what is on PATH.";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let r = match args.first().map(String::as_str) {
        Some("init") => cmd_init(&args[1..]),
        Some("whoami") => cmd_whoami(&args[1..]),
        Some("route") => cmd_route(&args[1..]),
        Some("authorize") => cmd_authorize(&args[1..]),
        Some("sync") => cmd_sync(&args[1..]),
        Some("carry") => cmd_carry(&args[1..]),
        Some("unit") => cmd_unit(&args[1..]),
        Some("status") => cmd_status(&args[1..]),
        Some("--help") | Some("-h") | None => {
            println!("{}", USAGE.replace("{version}", env!("CARGO_PKG_VERSION")));
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

fn routes_path(root: &Path) -> PathBuf {
    root.join("routes")
}

/// What `routes` grew out of: one line naming one place, for as long as
/// there was only ever one. Read by nothing now, and refused rather than
/// treated as an implicit `*`, so that no machine carries mail by a path
/// its own `route` output does not show.
fn depot_path(root: &Path) -> PathBuf {
    root.join("depot")
}

/// An ssh config of the courier's own, used when it exists.
///
/// Not a knob but the rest of the topology. ssh expands `~` from the
/// password database rather than from HOME, so a courier running as a
/// daemon user has no way to be told a place's host key except this
/// one: a file beside its key, holding UserKnownHostsFile, or a
/// ProxyJump, or whichever user the far side expects.
///
/// Absent means "use this account's ordinary ssh setup", which is right
/// for a courier that runs as a person.
fn ssh_config_path(root: &Path) -> PathBuf {
    root.join("ssh_config")
}

// ---- where mail goes ----------------------------------------------------

/// A place to reach, not a URL library.
///
/// One scheme today. The scheme is written down anyway so that a second
/// way of reaching a place has somewhere to be named without anything
/// being renamed, which is the whole reason it is not a bare hostname.
#[derive(Clone, PartialEq, Eq)]
struct Place {
    host: String,
    port: Option<String>,
}

/// One line of the table: an address and where it goes, or a place with
/// no address, which is where everything else goes.
///
/// Two levels and no more, and the default is spelled by leaving the
/// address off rather than by a word standing in its place. A word there
/// would sit in the column addresses sit in, with nothing but its shape
/// to tell them apart, and shape is not something a name has to respect.
/// A place always begins `ssh://` and an address never can, so the two
/// cannot be confused for each other by construction.
///
/// `note` is whatever followed the place, kept verbatim so that `route`
/// can print the file back in its own format.
struct Route {
    addr: Option<String>,
    place: Place,
    note: String,
}

/// A route as its line begins, for prose about one.
fn shown(addr: &Option<String>) -> String {
    match addr {
        None => "everything else".to_string(),
        Some(a) => a.clone(),
    }
}

fn parse_place(s: &str) -> Result<Place, Fail> {
    let rest = s.strip_prefix("ssh://").ok_or_else(|| {
        refused(format!(
            "\"{s}\" is not a place; it looks like ssh://[user@]host[:port]"
        ))
    })?;
    if rest.is_empty() || rest.contains('/') {
        return Err(refused(format!(
            "\"{s}\" is not a place; it looks like ssh://[user@]host[:port], with no path"
        )));
    }
    // A colon after the last ']' or after the host is a port. Bracketed
    // IPv6 is left to ssh, which understands it and this does not.
    match rest.rsplit_once(':') {
        Some((h, p)) if !h.is_empty() && p.chars().all(|c| c.is_ascii_digit()) && !p.is_empty() => {
            Ok(Place { host: h.to_string(), port: Some(p.to_string()) })
        }
        _ => Ok(Place { host: rest.to_string(), port: None }),
    }
}

/// The table, read where it is needed and never held.
///
/// A daemon carrying the copy it read at start is the fault where a unit
/// and a shell name different bebs: `route add` would change what
/// `route` prints and not what `carry` does, and the two would disagree
/// with nothing having failed. The file is small and the handshake it
/// decides about is not.
///
/// Absent is empty rather than an error, because the caller knows better
/// than this does whether having no routes is worth saying.
fn read_routes(root: &Path) -> Result<Vec<Route>, Fail> {
    let old = depot_path(root);
    if old.is_file() {
        let named = fs::read_to_string(&old).unwrap_or_default().trim().to_string();
        let named = if named.is_empty() { "ssh://host".to_string() } else { named };
        // In the order that works: every verb refuses while that file is
        // there, `route add` included, since it is the same table being
        // read. The place it named is in the message, so removing it
        // first loses nothing.
        return Err(refused(format!(
            "{} names a place, and nothing reads that file now\n\
             rm {}\n\
             beb-courier route add {named}",
            old.display(),
            old.display()
        )));
    }
    let p = routes_path(root);
    let text = match fs::read_to_string(&p) {
        Ok(t) => t,
        Err(_) => return Ok(Vec::new()),
    };
    let mut out: Vec<Route> = Vec::new();
    for (i, l) in text.lines().enumerate() {
        let l = l.trim();
        if l.is_empty() || l.starts_with('#') {
            continue;
        }
        let where_ = format!("line {} of {}", i + 1, p.display());
        let mut f = l.split_whitespace();
        let first = f.next().unwrap_or_default();
        let (addr, place) = if first.starts_with("ssh://") {
            (None, first)
        } else if is_address(first) {
            match f.next() {
                Some(s) => (Some(first.to_string()), s),
                None => {
                    return Err(refused(format!(
                        "{where_} names an address and no place; a line is \
                         [ADDRESS] ssh://host"
                    )))
                }
            }
        } else {
            return Err(refused(format!(
                "{where_} begins \"{}\", which is not an address; a line is \
                 [ADDRESS] ssh://host, and no address means everything else",
                trim_to(first, 16)
            )));
        };
        let place = parse_place(place)?;
        // Refused rather than resolved by taking the first. `route add`
        // will not make one, so a second line for one address arrived by
        // hand, and guessing which of the two an operator meant is how a
        // table starts lying about where mail goes.
        if out.iter().any(|r| r.addr == addr) {
            return Err(refused(format!(
                "{where_} routes {} a second time; one address, one place",
                shown(&addr)
            )));
        }
        out.push(Route {
            addr,
            place,
            note: f.collect::<Vec<_>>().join(" "),
        });
    }
    Ok(out)
}

/// Exact first, then the address-less line. The whole of the lookup.
fn resolve<'a>(routes: &'a [Route], to: &str) -> Option<&'a Place> {
    routes
        .iter()
        .find(|r| r.addr.as_deref() == Some(to))
        .or_else(|| routes.iter().find(|r| r.addr.is_none()))
        .map(|r| &r.place)
}

/// The place this machine collects from, if any.
///
/// Collection is singular, and not because a machine may have one depot.
/// It is because a courier cannot derive where mail for it is left: that
/// is decided by the tables of whoever sends to it, and no place can be
/// asked whether it shelves, since a peer's forced command would read
/// the question as a frame. So it has to be told, and the default route
/// tells it by convention. The two are not the same fact, and when they
/// have to come apart it is a mark on this line rather than a second
/// file, so the host keeps being named once.
///
/// A machine with only exact routes collects from nowhere, correctly,
/// because it is written into instead.
fn default_route(routes: &[Route]) -> Option<&Place> {
    routes.iter().find(|r| r.addr.is_none()).map(|r| &r.place)
}

fn cmd_init(args: &[String]) -> Result<(), Fail> {
    if !args.is_empty() {
        return Err("init takes nothing: beb-courier init\n\
                    where mail goes is a table, added to with beb-courier route add"
            .into());
    }
    let root = root()?;
    let key = key_path(&root);
    // Refused rather than overwritten: a new key is a key nowhere has
    // been told about, and replacing one silently would leave this
    // machine unable to collect with no sign of why.
    if key.exists() {
        return Err(refused(format!(
            "{} already has a courier key; where its mail goes is {}",
            root.display(),
            routes_path(&root).display()
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

    note(&format!("courier key {label} in {}", key.display()));
    note("nowhere to carry to yet: beb-courier route add ssh://host");
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

fn show(p: &Place) -> String {
    match &p.port {
        Some(port) => format!("{}:{}", p.host, port),
        None => p.host.clone(),
    }
}

fn plural<'a>(n: usize, one: &'a str, many: &'a str) -> &'a str {
    if n == 1 {
        one
    } else {
        many
    }
}

/// Enough of a long thing to recognise it, for prose about a mistake.
fn trim_to(s: &str, n: usize) -> String {
    match s.char_indices().nth(n) {
        Some((i, _)) => format!("{}…", &s[..i]),
        None => s.to_string(),
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

/// The addresses this machine reads for.
///
/// A directory read, not a computation. beb names each mailbox for the
/// identity's key, so the spool already holds exactly the addresses a
/// depot wants to shelve under, and the only work is dropping `outbox`,
/// which sits beside them. Sending to a stranger never invents a mailbox and a
/// mailbox appears at `beb init`, so what is left is precisely who
/// reads here, at the moment somebody first asks.
fn recipients() -> Result<Vec<String>, Fail> {
    let dir = spool()?;
    let mut out: Vec<String> = match fs::read_dir(&dir) {
        Ok(rd) => rd
            .filter_map(|e| e.ok())
            .filter_map(|e| e.file_name().to_str().map(str::to_string))
            .filter(|n| is_address(n))
            .collect(),
        Err(_) => Vec::new(),
    };
    out.sort();
    Ok(out)
}

fn is_address(s: &str) -> bool {
    s.len() == 64 && s.bytes().all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
}

/// Everything a depot operator needs from this machine, and nothing
/// else: the key that will be calling, and the addresses it collects for.
///
/// One document because the two facts have to travel together to a
/// machine this one was built to be unable to reach, and because the
/// depot can take them together -- `beb-depot authorize` reads exactly
/// this shape. The key first, the addresses after, prose on stderr so the
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
        n => note(&format!("1 key, {n} addresses this machine reads for")),
    }
    note("give this to whoever runs the depot: beb-depot authorize <this file>");
    note("or to a peer, who reads it with: beb-courier route add / beb-courier authorize");
    Ok(())
}

/// The two facts a handover carries, and each side of a direct link
/// needs one of them.
///
/// A depot uses both together: the key, to let that courier connect, and
/// the addresses, to know what it may collect. A peer hands nothing
/// back, so `route add` reads the addresses and ignores the key while
/// `authorize` reads the key and ignores them, on two different machines.
struct Handover {
    key: String,
    /// The comment on the key line, which is the machine that minted it.
    label: String,
    addresses: Vec<String>,
}

fn read_handover(p: &Path) -> Result<Handover, Fail> {
    // "-" is the descriptor already open, not the path /dev/stdin, for
    // the same reason the depot reads it that way: a pipe survives a
    // change of uid and re-opening the path does not.
    let dash = p == Path::new("-");
    let name = if dash { "the handover on stdin".to_string() } else { p.display().to_string() };
    let text = if dash {
        let mut s = String::new();
        io::stdin()
            .read_to_string(&mut s)
            .map_err(|e| Fail::from(format!("cannot read {name}: {e}")))?;
        s
    } else {
        fs::read_to_string(p).map_err(|e| Fail::from(format!("cannot read {name}: {e}")))?
    };
    if text.contains("PRIVATE KEY") {
        return Err(refused(format!(
            "{name} is a private key; a handover carries the public half, and is what \
             beb-courier whoami prints"
        )));
    }
    let (mut key, mut label, mut addresses) = (None, String::new(), Vec::new());
    for l in text.lines().map(str::trim).filter(|l| !l.is_empty() && !l.starts_with('#')) {
        if is_address(l) {
            addresses.push(l.to_string());
            continue;
        }
        let mut f = l.split_whitespace();
        match (f.next(), f.next()) {
            (Some(t), Some(_)) if t.starts_with("ssh-") || t.starts_with("ecdsa-") || t.starts_with("sk-") => {
                if key.is_none() {
                    label = f.collect::<Vec<_>>().join(" ");
                    key = Some(l.to_string());
                }
            }
            _ => {
                return Err(refused(format!(
                    "{name} has a line that is neither a key nor an address:\n  {}",
                    trim_to(l, 40)
                )))
            }
        }
    }
    match key {
        Some(key) => Ok(Handover { key, label, addresses }),
        None => Err(refused(format!(
            "{name} has no public key on it; beb-courier whoami prints one, first"
        ))),
    }
}

// ---- the table ----------------------------------------------------------

fn cmd_route(args: &[String]) -> Result<(), Fail> {
    match args.first().map(String::as_str) {
        None => route_list(),
        Some("add") => route_add(&args[1..]),
        Some("rm") => route_rm(&args[1..]),
        Some(other) => Err(format!(
            "no such route command \"{other}\"; it is add, rm, or nothing at all to list them"
        )
        .into()),
    }
}

/// The table, in the file's own format, so a line is copied rather than
/// transcribed -- and then the one thing the file cannot say: whether
/// anything waiting to leave matches none of it.
fn route_list() -> Result<(), Fail> {
    let root = root()?;
    let routes = read_routes(&root)?;
    if routes.is_empty() {
        return Err(nothing(format!(
            "no routes in {}; name one with: beb-courier route add ssh://host",
            routes_path(&root).display()
        )));
    }
    let mut out = io::stdout().lock();
    for r in &routes {
        let line = match &r.addr {
            Some(a) => format!("{a} ssh://{}", show(&r.place)),
            None => format!("ssh://{}", show(&r.place)),
        };
        if r.note.is_empty() {
            writeln!(out, "{line}")
        } else {
            writeln!(out, "{line} {}", r.note)
        }
        .map_err(|e| format!("cannot write: {e}"))?;
    }
    drop(out);

    let stranded = stranded(&routes);
    let waiting = outbox_addresses().len();
    note(&format!(
        "{} {}; {}",
        routes.len(),
        plural(routes.len(), "route", "routes"),
        match (waiting, stranded.len()) {
            (0, _) => "nothing waiting to leave".to_string(),
            (_, 0) => "everything waiting to leave has one".to_string(),
            (_, n) => format!("{n} waiting to leave match none"),
        }
    ));
    for a in &stranded {
        note(&format!("no route for {}: beb-courier route add {a} ssh://host", trim_to(a, 8)));
    }
    Ok(())
}

/// Addresses sitting in the outbox that the table does not cover.
///
/// The reason a list verb exists at all: a frame with nowhere to go is
/// otherwise silent, and reading the table alone cannot tell you.
fn stranded(routes: &[Route]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for to in outbox_addresses() {
        if resolve(routes, &to).is_none() && !out.contains(&to) {
            out.push(to);
        }
    }
    out
}

/// Read the way `push` reads it: a frame is `<id>-<address>`, and the
/// counter and the lock beside it are not mail.
fn outbox_addresses() -> Vec<String> {
    let dir = match spool() {
        Ok(s) => s.join("outbox"),
        Err(_) => return Vec::new(),
    };
    match fs::read_dir(dir) {
        Ok(rd) => rd
            .filter_map(|e| e.ok())
            .filter_map(|e| e.file_name().to_str().map(str::to_string))
            .filter_map(|n| n.split_once('-').map(|(_, to)| to.to_string()))
            .filter(|to| is_address(to))
            .collect(),
        Err(_) => Vec::new(),
    }
}

fn route_add(args: &[String]) -> Result<(), Fail> {
    let (what, place) = match args {
        // A place and nothing else is the line with no address: where
        // everything not routed elsewhere goes.
        [p] => (None, parse_place(p)?),
        [w, p] => (Some(w.as_str()), parse_place(p)?),
        _ => {
            return Err("route add takes where, and optionally what: \
                        beb-courier route add [ADDRESS] ssh://host\n\
                        ADDRESS is an address or the handover file that machine printed; \
                        left off, it is everything else"
                .into())
        }
    };
    let root = root()?;
    let existing = read_routes(&root)?;

    // One of three things, and a typo is none of them, so it is named
    // rather than written down as a route to nowhere.
    let adding: Vec<(Option<String>, String)> = match what {
        None => vec![(None, String::new())],
        Some(w) if is_address(w) => vec![(Some(w.to_string()), String::new())],
        Some(w) if Path::new(w).exists() || w == "-" => {
            let h = read_handover(Path::new(w))?;
            if h.addresses.is_empty() {
                return Err(refused(format!(
                    "{w} names no addresses, so there is nothing there to route to\n\
                     that machine has no identity reading on it yet: beb init NAME, \
                     then whoami again"
                )));
            }
            h.addresses.into_iter().map(|q| (Some(q), h.label.clone())).collect()
        }
        Some(w) => {
            return Err(refused(format!(
                "\"{}\" is not an address and not a file that is here\n\
                 for everything else, name the place alone: beb-courier route add ssh://host",
                trim_to(w, 24)
            )))
        }
    };

    // Nothing is written until every part of it can be: a half-added
    // handover is a machine some of whose identities are routed and some
    // of whose are not, which looks like a network fault rather than a
    // command that stopped halfway.
    for (m, _) in &adding {
        if let Some(r) = existing.iter().find(|r| &r.addr == m) {
            return Err(refused(format!(
                "{} already goes to {}; beb-courier route rm {} first",
                trim_to(&shown(m), 16),
                show(&r.place),
                match m {
                    Some(a) => a.clone(),
                    None => format!("ssh://{}", show(&r.place)),
                }
            )));
        }
    }

    private_dir_all(&root).map_err(|e| format!("cannot create {}: {e}", root.display()))?;
    let p = routes_path(&root);
    let mut f = OpenOptions::new()
        .create(true)
        .append(true)
        .mode(FILE_MODE)
        .open(&p)
        .map_err(|e| Fail::from(format!("cannot open {}: {e}", p.display())))?;
    for (m, label) in &adding {
        let line = match m {
            Some(a) => format!("{a} ssh://{}", show(&place)),
            None => format!("ssh://{}", show(&place)),
        };
        let line = if label.is_empty() { line } else { format!("{line} # {label}") };
        writeln!(f, "{line}").map_err(|e| format!("cannot write {}: {e}", p.display()))?;
    }
    f.sync_all().map_err(|e| format!("cannot sync: {e}"))?;

    if adding.iter().any(|(m, _)| m.is_none()) {
        note(&format!("everything not routed elsewhere goes to {}", show(&place)));
    } else {
        note(&format!(
            "{} {} to {}, in {}",
            adding.len(),
            plural(adding.len(), "route", "routes"),
            show(&place),
            p.display()
        ));
    }
    note(&format!(
        "{} must let this key drop: beb-courier whoami prints the handover it needs",
        show(&place)
    ));
    Ok(())
}

/// One address, or a place and everything going to it.
///
/// Two forms because the line with no address has no address to be named
/// by, and because a machine that has died is the case an operator
/// actually has: stop routing to that host, whatever was pointed at it.
/// They cannot be confused for each other, since a place begins `ssh://`
/// and an address is hex.
fn route_rm(args: &[String]) -> Result<(), Fail> {
    let what = match args {
        [w] => w.as_str(),
        _ => {
            return Err("route rm takes one address, or a place and everything going to it: \
                        beb-courier route rm ADDRESS | ssh://host"
                .into())
        }
    };
    let by_place = match what.strip_prefix("ssh://") {
        Some(_) => Some(show(&parse_place(what)?)),
        None => None,
    };
    let root = root()?;
    let p = routes_path(&root);
    let text = fs::read_to_string(&p)
        .map_err(|_| Fail::from(format!("no routes in {}", root.display())))?;
    let (mut kept, mut gone) = (String::new(), 0);
    for l in text.lines() {
        let hit = match &by_place {
            Some(host) => line_place(l).is_some_and(|h| &h == host),
            None => line_address(l) == Some(what),
        };
        if hit {
            gone += 1;
            continue;
        }
        kept.push_str(l);
        kept.push('\n');
    }
    if gone == 0 {
        return Err(nothing(match &by_place {
            Some(host) => format!("nothing is routed to {host}"),
            None => format!("{} is not routed anywhere", trim_to(what, 16)),
        }));
    }
    // Written beside and renamed over, because a routes file half
    // rewritten is a machine that has forgotten where some of its mail
    // goes, and this is the only verb that rewrites rather than appends.
    let tmp = p.with_extension("writing");
    let mut f = private_file(&tmp).map_err(|e| format!("cannot write {}: {e}", tmp.display()))?;
    f.write_all(kept.as_bytes()).map_err(|e| format!("cannot write: {e}"))?;
    f.sync_all().map_err(|e| format!("cannot sync: {e}"))?;
    drop(f);
    fs::rename(&tmp, &p).map_err(|e| format!("cannot replace {}: {e}", p.display()))?;

    let left = read_routes(&root)?;
    match (&by_place, default_route(&left)) {
        (Some(host), _) => note(&format!(
            "{gone} {} to {host} removed",
            plural(gone, "route", "routes")
        )),
        (None, Some(place)) => note(&format!(
            "{} is not routed on its own now, so it goes where everything else does: {}",
            trim_to(what, 8),
            show(place)
        )),
        (None, None) => note(&format!("{} is not routed now", trim_to(what, 8))),
    }
    Ok(())
}

/// The address a line begins with, if it begins with one. A line naming
/// only a place has none, which is what makes it the default.
fn line_address(l: &str) -> Option<&str> {
    let first = l.trim().split_whitespace().next()?;
    is_address(first).then_some(first)
}

/// The place a line names, wherever it sits on that line.
fn line_place(l: &str) -> Option<String> {
    let mut f = l.trim().split_whitespace();
    let first = f.next()?;
    let s = if first.starts_with("ssh://") { first } else { f.next()? };
    parse_place(s).ok().map(|p| show(&p))
}

// ---- letting a peer in --------------------------------------------------

/// The other half of a direct link, and the half this machine owns.
///
/// The line it writes is short because `beb drop` decides nothing about
/// the caller: it installs a frame for a mailbox that is here and
/// refuses one that is not. A depot bakes the caller's fingerprint into
/// its own line because it has to decide what that caller may collect,
/// and there is no such question here, so a fingerprint would be a check
/// with nothing behind it.
fn cmd_authorize(args: &[String]) -> Result<(), Fail> {
    let file = match args {
        [f] => Path::new(f.as_str()),
        _ => {
            return Err("authorize takes the handover that machine printed: \
                        beb-courier authorize FILE"
                .into())
        }
    };
    let h = read_handover(file)?;
    let beb = shell_word(&beb_path())?;
    // A forced command inherits none of the operator's environment. sshd
    // does set HOME, which is where beb's spool comes from, so the usual
    // line needs nothing -- but a machine keeping its spool under
    // XDG_DATA_HOME would otherwise deliver into an empty one, and an
    // empty spool and the wrong spool look identical from inside.
    let cmd = match std::env::var("XDG_DATA_HOME").ok().filter(|v| !v.is_empty()) {
        Some(x) => format!("env XDG_DATA_HOME={} {beb} drop", shell_word(&x)?),
        None => format!("{beb} drop"),
    };
    let line = format!("command=\"{cmd}\",restrict {}", h.key);

    let ak = authorized_keys_path()?;
    let existing = fs::read_to_string(&ak).unwrap_or_default();
    let blob = h.key.split_whitespace().nth(1).unwrap_or_default();
    for (i, l) in existing.lines().enumerate() {
        if !l.contains(blob) {
            continue;
        }
        if l.trim() == line {
            return Err(nothing(format!(
                "{} already lets that key drop here, with this command",
                ak.display()
            )));
        }
        return Err(refused(format!(
            "line {} of {} already has this key, under a different command:\n  {}\n\
             that line decides what the key may do; fix or remove it first",
            i + 1,
            ak.display(),
            l.trim()
        )));
    }
    append_line(&ak, &line)?;

    // stdout carries the line, so it can be piped to a machine that
    // keeps its authorized_keys somewhere this cannot reach.
    println!("{line}");
    let who = if h.label.is_empty() { "that courier".to_string() } else { h.label.clone() };
    note(&format!("{who} may drop here, in {}", ak.display()));
    note("to send there as well: beb-courier route add <their handover> ssh://<their host>");
    Ok(())
}

fn authorized_keys_path() -> Result<PathBuf, String> {
    if let Some(p) = std::env::var_os("BEB_COURIER_AUTHORIZED_KEYS").filter(|v| !v.is_empty()) {
        return Ok(PathBuf::from(p));
    }
    Ok(home()?.join(".ssh/authorized_keys"))
}

/// One argument of the forced command, safe for the shell sshd hands it
/// to. `command="..."` is not the end of the quoting: sshd runs the
/// string through the login shell, so a path with a space in it becomes
/// two arguments unless something says otherwise.
fn shell_word(s: &str) -> Result<String, Fail> {
    if s.contains('\'') || s.contains('"') || s.contains('\\') || s.contains('\n') {
        return Err(refused(format!(
            "{s} cannot go in a forced command; a quote or a backslash in the path \
             would end the line early. move it somewhere plainer"
        )));
    }
    Ok(format!("'{s}'"))
}

/// Appended, never rewritten. authorized_keys is a file sshd depends on
/// and other things edit; replacing it wholesale to add one line risks
/// losing whatever arrived in between, and this verb's whole purpose is
/// to be the safe way to touch it.
fn append_line(p: &Path, line: &str) -> Result<(), Fail> {
    // Created if missing, and never chmodded if not: the parent here is
    // somebody else's directory, unlike every other directory this
    // program makes.
    if let Some(d) = p.parent() {
        if !d.as_os_str().is_empty() && !d.is_dir() {
            private_dir_all(d).map_err(|e| Fail::from(format!("cannot create {}: {e}", d.display())))?;
        }
    }
    let mut f = OpenOptions::new()
        .create(true)
        .append(true)
        .mode(FILE_MODE)
        .open(p)
        .map_err(|e| Fail::from(format!("cannot open {}: {e}", p.display())))?;
    // A file not ending in a newline would otherwise gain a line that is
    // the tail of the previous one, and sshd would read neither.
    let tail = fs::read_to_string(p).unwrap_or_default();
    let lead = if tail.is_empty() || tail.ends_with('\n') { "" } else { "\n" };
    write!(f, "{lead}{line}\n").map_err(|e| format!("cannot write {}: {e}", p.display()))?;
    f.sync_all().map_err(|e| format!("cannot sync: {e}"))?;
    Ok(())
}

// ---- the two directions -------------------------------------------------

fn cmd_sync(args: &[String]) -> Result<(), Fail> {
    if !args.is_empty() {
        return Err("sync takes nothing: beb-courier sync".into());
    }
    let root = root()?;
    let routes = read_routes(&root)?;
    if routes.is_empty() {
        return Err(refused(format!(
            "nowhere to carry to: {} has no routes\n\
             beb-courier route add ssh://host names one for everything",
            root.display()
        )));
    }
    // A fresh table of waits, because one pass cannot hold anything off
    // twice. `carry` keeps its own across passes, which is where a
    // holddown means something.
    let mut held = Held::default();
    let pass = push(&root, &routes, &mut held)?;
    let pulled = match default_route(&routes) {
        Some(place) => collect(&root, place, "drain")?,
        None => 0,
    };

    // Everything that could move has moved; this is what did not.
    // Reported as trouble rather than folded into a count, because a sync
    // on a timer that quietly leaves the same frames behind every run is
    // a sync nobody looks at again.
    let mut trouble: Vec<String> = Vec::new();
    if pass.refused_here > 0 {
        trouble.push(format!("{} the far side would not take", pass.refused_here));
    }
    if !pass.unroutable.is_empty() {
        trouble.push(format!("{} with nowhere to go", pass.unroutable.len()));
    }
    if !pass.unreachable.is_empty() {
        trouble.push(format!("could not reach {}", pass.unreachable.join(", ")));
    }
    if !trouble.is_empty() {
        // 3 when this program declined to move something, 1 when the
        // network did: a refusal is an answer and an unreachable host is
        // not, and a timer reading exit codes should be able to tell.
        let code = if pass.refused_here > 0 || !pass.unroutable.is_empty() { 3 } else { 1 };
        return Err(Fail {
            code,
            msg: format!("{} sent, {pulled} received, {}", pass.sent, trouble.join("; ")),
        });
    }
    if pass.sent == 0 && pulled == 0 {
        return Err(nothing("nothing to send and nothing waiting"));
    }
    note(&format!("{} sent, {pulled} received", pass.sent));
    Ok(())
}

/// How long to leave a place alone, per place.
///
/// Until there were two places, "this pass failed" and "that host is
/// down" were the same sentence, and one backoff served both. A second
/// route makes continuing past a dead host mandatory -- bob asleep must
/// not hold pve's mail -- and continuing returns success from the pass,
/// which resets that backoff to a quarter second.
///
/// Skipping is the load-bearing half, not the waiting: `push` is one
/// sequential loop, so a pass that merely tried and failed would sit
/// through one host's connect timeout before reaching the others, and
/// one sleeping laptop would stall the whole outbox behind it.
///
/// Not a count of attempts, and nothing here gives up. The wait says
/// when to look again; the frame is still in the outbox either way.
#[derive(Default)]
struct Held(HashMap<String, (Instant, Duration)>);

impl Held {
    fn waiting(&self, place: &str) -> bool {
        self.0.get(place).is_some_and(|(until, _)| Instant::now() < *until)
    }
    fn down(&mut self, place: &str) {
        let wait = match self.0.get(place) {
            Some((_, w)) => (*w * 2).min(BACKOFF_MAX),
            None => BACKOFF_MIN,
        };
        self.0.insert(place.to_string(), (Instant::now() + wait, wait));
    }
    fn up(&mut self, place: &str) {
        self.0.remove(place);
    }
}

/// What one pass over the outbox did, and what it left behind.
#[derive(Default)]
struct Pass {
    sent: usize,
    refused_here: usize,
    unroutable: Vec<String>,
    unreachable: Vec<String>,
}

/// Outbound. Nothing has to be running for this to have work: the
/// outbox only fills because somebody sent something, so the sender was
/// already here.
///
/// The filename says who each frame is for, and the table says where
/// that recipient is. Opening the frame to find out where it goes is the
/// one thing a courier must never do, and the reason it does not have to
/// is that beb writes the address into the name.
fn push(root: &Path, routes: &[Route], held: &mut Held) -> Result<Pass, Fail> {
    let dir = spool()?.join("outbox");
    let mut waiting: Vec<(String, PathBuf)> = match fs::read_dir(&dir) {
        Ok(rd) => rd
            .filter_map(|e| e.ok())
            .filter_map(|e| {
                let name = e.file_name().to_str()?.to_string();
                let (_, to) = name.split_once('-')?;
                is_address(to).then(|| (name.clone(), e.path()))
            })
            .collect(),
        Err(_) => Vec::new(),
    };
    // Oldest first, because the ids are ordered and a reader would
    // rather see a conversation in the order it was written.
    waiting.sort_by(|a, b| a.0.cmp(&b.0));

    let mut pass = Pass::default();
    for (name, path) in waiting {
        let to = name.split_once('-').map(|(_, t)| t).unwrap_or_default();
        let place = match resolve(routes, to) {
            Some(p) => p,
            None => {
                // Said once per address per pass, and never guessed at.
                // A frame with nowhere to go is otherwise the only thing
                // in this program that is silent.
                if !pass.unroutable.iter().any(|a| a == to) {
                    pass.unroutable.push(to.to_string());
                    note(&format!(
                        "{} stays here: no route for {}, and nothing takes the rest\n\
                         name one with: beb-courier route add {to} ssh://host",
                        short(&name),
                        trim_to(to, 8)
                    ));
                }
                continue;
            }
        };
        let where_ = show(place);
        // Held off, and already reported when it went down. Every frame
        // for this place waits with it rather than being dialled at.
        if held.waiting(&where_) {
            continue;
        }
        let f = File::open(&path).map_err(|e| format!("cannot open {}: {e}", path.display()))?;
        let out = ssh(place, root)
            .arg(format!("drop {to}"))
            .stdin(Stdio::from(f))
            .output()
            .map_err(|e| Fail::from(format!("cannot run ssh: {e}")))?;
        if out.status.success() {
            // Only now, and never before: the far side has it, so this
            // copy is the redundant one. The other order loses mail.
            fs::remove_file(&path).map_err(|e| format!("cannot remove {}: {e}", path.display()))?;
            held.up(&where_);
            pass.sent += 1;
            continue;
        }
        let why = String::from_utf8_lossy(&out.stderr);
        let why = why.trim();
        // 255 is ssh itself: the place was not reached, so every other
        // frame going there would fail the same way. It is held off and
        // the pass carries on, because another place is another machine
        // and has nothing to do with this one being asleep.
        if out.status.code() == Some(255) {
            held.down(&where_);
            if !pass.unreachable.contains(&where_) {
                pass.unreachable.push(where_.clone());
            }
            note(&format!(
                "{} and the rest for {where_} stay here: {why}\n\
                 ssh has to know that host already; accept its key once, or put \
                 UserKnownHostsFile in {}",
                short(&name),
                ssh_config_path(root).display()
            ));
            continue;
        }
        note(&format!("{} stays here: {why}", short(&name)));
        pass.refused_here += 1;
    }
    Ok(pass)
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
fn collect(root: &Path, depot: &Place, intent: &str) -> Result<usize, Fail> {
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
    // The write may fail, and its failure is not the reason for
    // anything. beb decides admission before it reads a body, so a
    // refusal exits while this is still writing and the pipe breaks --
    // and "cannot write to beb: Broken pipe" is what the operator then
    // sees instead of the sentence beb wrote explaining itself. Worse,
    // returning here left the child unreaped, one zombie per attempt.
    // So the error is kept, the child is always waited for, and beb's
    // own words win.
    let wrote = child
        .stdin
        .take()
        .expect("stdin piped")
        .write_all(frame);
    let out = child
        .wait_with_output()
        .map_err(|e| Fail::from(format!("cannot wait for beb: {e}")))?;
    if out.status.success() {
        note(String::from_utf8_lossy(&out.stderr).trim());
        return Ok(());
    }
    if let Err(e) = wrote {
        // Only worth saying when beb said nothing itself.
        if out.stderr.is_empty() {
            return Err(Fail::from(format!("cannot write to beb: {e}")));
        }
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
fn cmd_carry(args: &[String]) -> Result<(), Fail> {
    if !args.is_empty() {
        return Err("carry takes nothing: beb-courier carry".into());
    }
    let root = root()?;
    // Read here only to fail now rather than inside a thread. Both
    // halves read it again for themselves, every pass, so a route added
    // while this is running is a route this is using.
    let routes = read_routes(&root)?;
    if routes.is_empty() {
        return Err(refused(format!(
            "nowhere to carry to: {} has no routes\n\
             beb-courier route add * ssh://host names one for everything",
            root.display()
        )));
    }
    match default_route(&routes) {
        Some(p) => note(&format!("carrying both ways, collecting from {}", show(p))),
        None => note(
            "carrying outbound only, since nothing shelves for you\n\
             mail still lands the moment a peer drops it in",
        ),
    }

    let r = root.clone();
    std::thread::spawn(move || outbound(&r));

    let mut wait = BACKOFF_MIN;
    loop {
        match read_routes(&root).ok().and_then(|rs| default_route(&rs).cloned()) {
            // Nothing shelves for this machine, so there is nothing to
            // hold a connection to. Looked at again anyway, since a `*`
            // route may be added while this runs.
            None => {
                std::thread::sleep(BACKOFF_MAX);
                continue;
            }
            Some(place) => match collect(&root, &place, "pickup") {
                // A blocking pickup that returned means the connection
                // ended, not that there is nothing to do. Reconnect.
                Ok(n) => {
                    if n > 0 {
                        wait = BACKOFF_MIN; // it worked, so start over patient
                    }
                }
                // A refusal is about that place's answer and will not
                // change by asking again in a second.
                Err(f) if f.code == 3 => return Err(f),
                Err(f) => note(&f.msg),
            },
        }
        std::thread::sleep(wait);
        wait = (wait * 2).min(BACKOFF_MAX);
    }
}

/// The outbound half of `carry`: ship what is waiting, as it appears.
///
/// Two backoffs, and they are not the same backoff. A place that cannot
/// be reached is held off by itself, so the rest of the outbox keeps
/// moving at poll speed. A frame that is refused, or has nowhere to go,
/// will be refused every time it is offered, and at four looks a second
/// that is the same line four times a second forever -- so the pass
/// itself quiets down to once a minute while anything is stuck in it.
fn outbound(root: &Path) -> ! {
    let mut held = Held::default();
    let mut wait = BACKOFF_MIN;
    loop {
        // Read every pass. A table held from the start is a `route add`
        // that changes what `route` prints and not what this does.
        let routes = match read_routes(root) {
            Ok(r) => r,
            Err(f) => {
                note(&f.msg);
                std::thread::sleep(wait);
                wait = (wait * 2).min(BACKOFF_MAX);
                continue;
            }
        };
        match push(root, &routes, &mut held) {
            Ok(pass) => {
                if pass.sent > 0 {
                    note(&format!("{} sent", pass.sent));
                }
                if pass.refused_here > 0 || !pass.unroutable.is_empty() {
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

fn ssh(depot: &Place, root: &Path) -> Command {
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

/// Where `beb` actually is, resolved the way a shell would and then
/// written down, because the shell that writes a unit and the systemd
/// that runs it do not share a PATH.
fn beb_path() -> String {
    if let Ok(b) = std::env::var("BEB_BIN") {
        if !b.is_empty() {
            return b;
        }
    }
    if let Some(paths) = std::env::var_os("PATH") {
        for dir in std::env::split_paths(&paths) {
            let c = dir.join("beb");
            if c.is_file() {
                return c.to_string_lossy().to_string();
            }
        }
    }
    "beb".to_string()
}

/// The supervisor file for whichever supervisor this machine has.
///
/// It printed a systemd unit on every platform, which on the one macOS
/// box in the fleet was a file nothing would ever read. A verb whose
/// whole job is "hand this to your init system" has to know which one
/// it is talking to, and the binary already knows: it is built per
/// platform.
///
/// Still printed and never installed. Where a supervisor file belongs,
/// and whether it is wanted, is the operator's.
/// Whether this machine can still carry, and what it carries for.
///
/// A courier is several things that have to agree and are written in
/// different places: its key, the places it names, the supervisor file
/// that starts it, and the beb that file points at. The one outage here
/// was that last pair drifting -- a unit resolving a beb four versions
/// behind, which refused every frame while the same command in a login
/// shell worked. Every part looked healthy alone.
///
/// Every place is probed with an intent it will refuse. The refusal is
/// the answer: it proves the host is reachable, the key authenticates,
/// and sshd ran the forced command -- and it moves no mail, which a
/// pickup or a drain would. A depot refuses an intent it does not serve;
/// a peer's `beb drop` refuses an empty frame; either way the far side
/// spoke, which is the whole question.
fn cmd_status(args: &[String]) -> Result<(), Fail> {
    if !args.is_empty() {
        return Err("status takes nothing: beb-courier status".into());
    }
    let root = root()?;
    let mut wrong = Vec::new();

    let key = key_path(&root);
    if !key.is_file() {
        wrong.push(format!("no key in {}; beb-courier init makes one", root.display()));
    }
    let routes = match read_routes(&root) {
        Ok(r) if r.is_empty() => {
            wrong.push(format!(
                "no routes in {}; beb-courier route add ssh://host names one",
                root.display()
            ));
            Vec::new()
        }
        Ok(r) => r,
        Err(f) => {
            wrong.push(f.msg);
            Vec::new()
        }
    };

    let mine = recipients().unwrap_or_default();
    // Counted the way `push` reads it: a frame is `<id>-<address>`, and
    // the counter and the lock beside it are not mail. Counting the
    // directory said "2 waiting to leave" on an empty outbox, which is
    // the kind of wrong that teaches an operator to ignore the verb.
    let waiting = outbox_addresses().len();

    // The beb this will hand mail to, resolved the way `carry` will.
    let beb = std::env::var("BEB_BIN").unwrap_or_else(|_| beb_path());
    let beb_version = std::process::Command::new(&beb)
        .arg("--version")
        .output()
        .ok()
        .filter(|o| o.status.success())
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_string());
    if beb_version.is_none() {
        wrong.push(format!("{beb} does not answer --version, so nothing can be delivered here"));
    }

    // The supervisor file, if this machine has one where they live.
    let sup = supervisor_path();
    if let Some(sp) = &sup {
        if sp.is_file() {
            let text = fs::read_to_string(sp).unwrap_or_default();
            if !text.contains(&beb) {
                wrong.push(format!(
                    "{} does not name {beb}, so the service and this shell disagree about beb",
                    sp.display()
                ));
            }
            if let Ok(exe) = std::env::current_exe() {
                if !text.contains(&exe.display().to_string()) {
                    wrong.push(format!(
                        "{} runs a different beb-courier than the one you just ran",
                        sp.display()
                    ));
                }
            }
            // And the verb it runs, which the two checks above both pass
            // while the service restart-loops on "no such command". A
            // supervisor file is written once, from whatever `unit`
            // printed that day, so it outlives the name it was given.
            if !text.contains("carry") {
                wrong.push(format!(
                    "{} does not run carry, so it names a verb this version does not have\n\
                     beb-courier unit prints the file this one wants",
                    sp.display()
                ));
            }
        }
    }

    note(&format!(
        "{} at {}, root {}",
        env!("CARGO_PKG_VERSION"),
        std::env::current_exe().map(|p| p.display().to_string()).unwrap_or_else(|_| "?".into()),
        root.display()
    ));
    note(&format!(
        "{} addresses, {waiting} waiting to leave, beb is {}",
        mine.len(),
        beb_version.unwrap_or_else(|| "missing".into())
    ));
    if !routes.is_empty() {
        note(&format!(
            "{} {}, {}",
            routes.len(),
            plural(routes.len(), "route", "routes"),
            match default_route(&routes) {
                Some(p) => format!("collecting from {}", show(p)),
                None => "collecting from nowhere, since no route takes what is not named; \
                         mail arrives here by being dropped in"
                    .to_string(),
            }
        ));
    }
    match (&sup, sup.as_ref().map(|p| p.is_file())) {
        (Some(p), Some(true)) => note(&format!("supervised by {}", p.display())),
        (Some(p), _) => note(&format!("no supervisor file at {}; beb-courier unit prints one", p.display())),
        _ => {}
    }

    // A frame with nowhere to go is the one fault the table cannot show
    // by being read, and the one the `*` route hides: mail keeps flowing
    // for everybody else while this waits forever.
    for a in stranded(&routes) {
        wrong.push(format!(
            "{} is waiting to leave with no route: beb-courier route add {a} ssh://host",
            trim_to(&a, 8)
        ));
    }

    // Each distinct place once, however many addresses point at it.
    let mut places: Vec<&Place> = Vec::new();
    for r in &routes {
        if !places.iter().any(|p| **p == r.place) {
            places.push(&r.place);
        }
    }
    for p in places {
        match probe(&root, p) {
            Probe::Answered => note(&format!("{} answers and knows this key", show(p))),
            Probe::Unreachable(why) => wrong.push(format!("cannot reach {}: {why}", show(p))),
        }
    }

    if wrong.is_empty() {
        return Ok(());
    }
    for w in &wrong {
        note(w);
    }
    Err(refused(if wrong.len() == 1 {
        "one thing does not agree".to_string()
    } else {
        format!("{} things do not agree", wrong.len())
    }))
}

enum Probe {
    Answered,
    Unreachable(String),
}

/// Ask a place for something nothing serves.
///
/// Its own refusal is the healthy answer: the connection was made, the
/// key authenticated, and sshd ran the forced command. 255 is ssh
/// itself, which means the place was never reached.
///
/// Two outcomes and not three, because a route records where and never
/// what: a depot refuses an intent it does not have, a peer's `beb drop`
/// refuses an empty frame, and this cannot judge an answer it was never
/// told to expect. `output()` gives the far side no stdin, which is what
/// makes the peer case a refusal rather than a wait.
fn probe(root: &Path, depot: &Place) -> Probe {
    let out = match ssh(depot, root).arg("status-probe").output() {
        Ok(o) => o,
        Err(e) => return Probe::Unreachable(e.to_string()),
    };
    let said = String::from_utf8_lossy(&out.stderr).trim().to_string();
    match out.status.code() {
        Some(255) | None => {
            Probe::Unreachable(if said.is_empty() { "no answer".into() } else { said })
        }
        _ => Probe::Answered,
    }
}

/// Where this platform keeps the file that would supervise `carry`.
fn supervisor_path() -> Option<PathBuf> {
    let home = home().ok()?;
    Some(if cfg!(target_os = "macos") {
        home.join("Library/LaunchAgents/dev.getbeb.courier.plist")
    } else {
        home.join(".config/systemd/user/beb-courier.service")
    })
}

fn cmd_unit(args: &[String]) -> Result<(), Fail> {
    if !args.is_empty() {
        return Err("unit takes nothing: beb-courier unit".into());
    }
    let exe = std::env::current_exe()
        .map_err(|e| Fail::from(format!("cannot find my own path: {e}")))?;
    let root = root()?;
    // The beb this operator can see, named by absolute path.
    //
    // A supervisor inherits none of the operator's PATH: systemd gives a
    // user service /usr/local/bin and /usr/bin and nothing else, and
    // launchd is no better. On the first machine to run one, that
    // resolved a beb from another install -- four minor versions behind,
    // refusing every frame as malformed -- while the same command in a
    // login shell used the right one and worked. The file says which, so
    // the two cannot disagree.
    let beb = beb_path();
    let (exe, root) = (exe.display().to_string(), root.display().to_string());
    let mut out = io::stdout().lock();

    if cfg!(target_os = "macos") {
        write!(
            out,
            "\
<?xml version=\"1.0\" encoding=\"UTF-8\"?>
<!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">
<plist version=\"1.0\">
<dict>
  <key>Label</key>
  <string>dev.getbeb.courier</string>
  <key>ProgramArguments</key>
  <array>
    <string>{exe}</string>
    <string>carry</string>
  </array>
  <key>EnvironmentVariables</key>
  <dict>
    <key>BEB_COURIER_ROOT</key>
    <string>{root}</string>
    <key>BEB_BIN</key>
    <string>{beb}</string>
  </dict>
  <key>RunAtLoad</key>
  <true/>
  <key>KeepAlive</key>
  <true/>
</dict>
</plist>
"
        )
        .map_err(|e| format!("cannot write: {e}"))?;
        drop(out);
        note("write that to ~/Library/LaunchAgents/dev.getbeb.courier.plist");
        note("then: launchctl bootstrap gui/$(id -u) ~/Library/LaunchAgents/dev.getbeb.courier.plist");
        return Ok(());
    }

    write!(
        out,
        "\
[Unit]
Description=beb-courier: carry beb mail both ways
After=network-online.target

[Service]
ExecStart={exe} carry
Environment=BEB_COURIER_ROOT={root}
Environment=BEB_BIN={beb}
Restart=always
RestartSec=5

[Install]
WantedBy=default.target
"
    )
    .map_err(|e| format!("cannot write: {e}"))?;
    drop(out);
    note("write that to ~/.config/systemd/user/beb-courier.service");
    note("then: systemctl --user enable --now beb-courier");
    Ok(())
}
